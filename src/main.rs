// Spec found at https://tools.ietf.org/html/rfc959

/*
 * FIXME: Filezilla says: Le serveur ne supporte pas les caractères non-ASCII.
 * FIXME: ftp cli says "WARNING! 71 bare linefeeds received in ASCII mode" when retrieving a file.
 * TODO: LIST does not send all the data in the right order (FileZilla shows the file size in the
 * permission column).
 * FIXME: can't use StringCodec to upload binary file (use Vec<u8> instead for the codec).
 * TODO: check if can upload/download file bigger than 8Kb.
 */

#![feature(proc_macro, conservative_impl_trait, generators)]

extern crate bytes;
#[macro_use]
extern crate cfg_if;
extern crate futures_await as futures;
extern crate time;
extern crate tokio_core;
extern crate tokio_io;

mod cmd; // FIXME: rename this module.
mod codec;
mod ftp;

use std::env;
use std::ffi::OsString;
use std::fs::{File, Metadata, create_dir, read_dir, remove_dir_all};
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf, StripPrefixError};

use futures::{Sink, Stream};
use futures::prelude::{async, await};
use futures::stream::{SplitSink, SplitStream};
use tokio_core::reactor::{Core, Handle};
use tokio_core::net::{TcpListener, TcpStream};
use tokio_io::AsyncRead;
use tokio_io::codec::Framed;

use cmd::{Command, TransferType};
use codec::{FtpCodec, StringCodec};
use ftp::{Answer, ResultCode};

const MONTHS: [&'static str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun",
                                    "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

type DataReader = SplitStream<Framed<TcpStream, StringCodec>>;
type DataWriter = SplitSink<Framed<TcpStream, StringCodec>>;
type Writer = SplitSink<Framed<TcpStream, FtpCodec>>;

cfg_if! {
    if #[cfg(windows)] {
        fn get_file_info(meta: &Metadata) -> (time::Tm, u64) {
            use std::os::windows::prelude::*;
            (time::at(time::Timespec::new(meta.last_write_time())), meta.file_size())
        }
    } else {
        fn get_file_info(meta: &Metadata) -> (time::Tm, u64) {
            use std::os::unix::prelude::*;
            (time::at(time::Timespec::new(meta.mtime(), 0)), meta.size())
        }
    }
}

// If an error occurs when we try to get file's information, we just return and don't send its info.
fn add_file_info(path: PathBuf, out: &mut String) {
    let extra = if path.is_dir() { "/" } else { "" };
    let is_dir = if path.is_dir() { "d" } else { "-" };

    let meta = match ::std::fs::metadata(&path) {
        Ok(meta) => meta,
        _ => return,
    };
    let (time, file_size) = get_file_info(&meta);
    let path = match path.to_str() {
        Some(path) => match path.split("/").last() {
            Some(path) => path,
            _ => return,
        },
        _ => return,
    };
    // TODO: maybe improve how we get rights in here?
    let rights = if meta.permissions().readonly() {
        "r--r--r--"
    } else {
        "rw-rw-rw-"
    };
    let file_str = format!("{is_dir}{rights} {links} {owner} {group} {size} {month} {day} {hour}:{min} {path}{extra}\r\n",
                           is_dir=is_dir,
                           rights=rights,
                           links=1, // number of links
                           owner="anonymous", // owner name
                           group="anonymous", // group name
                           size=file_size,
                           month=MONTHS[time.tm_mon as usize],
                           day=time.tm_mday,
                           hour=time.tm_hour,
                           min=time.tm_min,
                           path=path,
                           extra=extra);
    out.push_str(&file_str);
    println!("==> {:?}", &file_str);
}

#[allow(dead_code)]
struct Client {
    cwd: PathBuf,
    data_port: Option<u16>,
    data_reader: Option<DataReader>,
    data_writer: Option<DataWriter>, // TODO: do we really need to split the data socket? => NOPE
    handle: Handle,
    name: Option<String>,
    server_root: PathBuf,
    transfer_type: TransferType,
    writer: Writer,
}

impl Client {
    fn new(handle: Handle, writer: Writer, server_root: PathBuf) -> Client {
        Client {
            cwd: PathBuf::from(""),
            data_port: None,
            data_reader: None,
            data_writer: None,
            handle,
            name: None,
            server_root,
            transfer_type: TransferType::Ascii,
            writer,
        }
    }

    #[async]
    fn handle_cmd(mut self, cmd: Command) -> Result<Self, ()> {
        println!("Received command: {:?}", cmd);
        match cmd {
            Command::Auth =>
                self = await!(self.send(Answer::new(ResultCode::CommandNotImplemented, "Not implemented")))?
            ,
            Command::Cwd(directory) => self = await!(self.cwd(directory))?,
            Command::List(path) => self = await!(self.list(path))?,
            Command::Pasv => self = await!(self.pasv())?,
            Command::Port(port) => {
                self.data_port = Some(port);
                self = await!(self.send(Answer::new(ResultCode::Ok, &format!("Data port is now {}", port))))?;
            }
            Command::Pwd => {
                let msg = format!("{}", self.cwd.to_str().unwrap_or("")); // small trick
                if !msg.is_empty() {
                    let message = format!("\"/{}\" ", msg);
                    self = await!(self.send(Answer::new(ResultCode::PATHNAMECreated, &message)))?;
                } else {
                    self = await!(self.send(Answer::new(ResultCode::FileNotFound, "No such file or directory")))?;
                }
            }
            Command::Quit => self = await!(self.quit())?,
            Command::Retr(file) => self = await!(self.retr(file))?,
            Command::Stor(file) => self = await!(self.stor(file))?,
            Command::Syst => {
                self = await!(self.send(Answer::new(ResultCode::Ok, "I won't tell!")))?;
            }
            Command::Type(typ) => {
                self.transfer_type = typ;
                self = await!(self.send(Answer::new(ResultCode::Ok, "Transfer type changed successfully")))?;
            }
            Command::User(content) => {
                if content.is_empty() {
                    self = await!(self.send(Answer::new(ResultCode::InvalidParameterOrArgument, "Invalid username")))?;
                } else {
                    self.name = Some(content.to_owned());
                    self = await!(self.send(Answer::new(ResultCode::UserloggedIn, &format!("Welcome {}!", content))))?;
                }
            }
            Command::CdUp => {
                if let Some(path) = self.cwd.parent().map(Path::to_path_buf) {
                    self.cwd = path;
                }
                self = await!(self.send(Answer::new(ResultCode::Ok, "Done")))?;
            }
            Command::Mkd(path) => self = await!(self.mkd(path))?,
            Command::Rmd(path) => self = await!(self.rmd(path))?,
            Command::NoOp => self = await!(self.send(Answer::new(ResultCode::Ok, "Doing nothing")))?,
            Command::Unknown(s) =>
                self = await!(self.send(Answer::new(ResultCode::UnknownCommand,
                                                    &format!("\"{}\": Not implemented", s))))?
            ,
        }
        Ok(self)
    }

    fn close_data_connection(&mut self) {
        self.data_reader = None;
        self.data_writer = None;
    }

    fn complete_path(self, path: PathBuf) -> (Self, Result<PathBuf, io::Error>) {
        let directory = self.server_root.join(if path.has_root() {
            path.iter().skip(1).collect()
        } else {
            path
        });
        let dir = directory.canonicalize();
        if let Ok(ref dir) = dir {
            if !dir.starts_with(&self.server_root) {
                return (self, Err(io::ErrorKind::PermissionDenied.into()));
            }
        }
        (self, dir)
    }

    fn get_parent(self, path: PathBuf) -> (Self, Option<PathBuf>) {
        (self, path.parent().map(|p| p.to_path_buf()))
    }

    fn get_filename(self, path: PathBuf) -> (Self, Option<OsString>) {
        (self, path.file_name().map(|p| p.to_os_string()))
    }

    #[async]
    fn mkd(mut self, path: PathBuf) -> Result<Self, ()> {
        let path = self.cwd.join(&path);
        let (new_self, parent) = self.get_parent(path.clone());
        self = new_self;
        if let Some(parent) = parent {
            let parent = parent.to_path_buf();
            let (new_self, res) = self.complete_path(parent);
            self = new_self;
            if let Ok(mut dir) = res {
                if dir.is_dir() {
                    let (new_self, filename) = self.get_filename(path);
                    self = new_self;
                    if let Some(filename) = filename {
                        dir.push(filename);
                        if create_dir(dir).is_ok() {
                            self = await!(self.send(Answer::new(ResultCode::PATHNAMECreated,
                                                                "Folder successfully created!")))?;
                            return Ok(self);
                        }
                    }
                }
            }
        }
        self = await!(self.send(Answer::new(ResultCode::FileNotFound,
                                            "Couldn't create folder")))?;
        Ok(self)
    }

    #[async]
    fn rmd(mut self, directory: PathBuf) -> Result<Self, ()> {
        let path = self.cwd.join(&directory);
        let (new_self, res) = self.complete_path(path);
        self = new_self;
        if let Ok(dir) = res {
            if remove_dir_all(dir).is_ok() {
                self = await!(self.send(Answer::new(ResultCode::RequestedFileActionOkay,
                                                    "Folder successfully removed")))?;
                return Ok(self);
            }
        }
        self = await!(self.send(Answer::new(ResultCode::FileNotFound,
                                            "Couldn't remove folder")))?;
        Ok(self)
    }

    fn strip_prefix(self, dir: PathBuf) -> (Self, Result<PathBuf, StripPrefixError>) {
        let res = dir.strip_prefix(&self.server_root).map(|p| p.to_path_buf());
        (self, res)
    }

    #[async]
    fn cwd(mut self, directory: PathBuf) -> Result<Self, ()> {
        let path = self.cwd.join(&directory);
        let (new_self, res) = self.complete_path(path);
        self = new_self;
        if let Ok(dir) = res {
            let (new_self, res) = self.strip_prefix(dir);
            self = new_self;
            if let Ok(prefix) = res {
                self.cwd = prefix.to_path_buf();
                self = await!(self.send(Answer::new(ResultCode::Ok,
                                                    &format!("Directory changed to \"{}\"", directory.display()))))?;
                return Ok(self)
            }
        }
        self = await!(self.send(Answer::new(ResultCode::FileNotFound,
                                            "No such file or directory")))?;
        Ok(self)
    }

    #[async]
    fn list(mut self, path: Option<PathBuf>) -> Result<Self, ()> {
        if self.data_writer.is_some() {
            let path = self.cwd.join(path.unwrap_or_default());
            let directory = PathBuf::from(&path);
            let (new_self, res) = self.complete_path(directory);
            self = new_self;
            if let Ok(path) = res {
                self = await!(self.send(Answer::new(ResultCode::DataConnectionAlreadyOpen,
                                                    "Starting to list directory...")))?;
                let mut out = String::new();
                if path.is_dir() {
                    if let Ok(dir) = read_dir(path) {
                        for entry in dir {
                            if let Ok(entry) = entry {
                                add_file_info(entry.path(), &mut out);
                            }
                        }
                    } else {
                        self = await!(self.send(Answer::new(ResultCode::InvalidParameterOrArgument,
                                                            "No such file or directory")))?;
                        return Ok(self);
                    }
                } else {
                    add_file_info(path, &mut out);
                }
                self = await!(self.send_data(out))?;
                println!("-> and done!");
            } else {
                self = await!(self.send(Answer::new(ResultCode::InvalidParameterOrArgument,
                                                    "No such file or directory")))?;
            }
        } else {
            self = await!(self.send(Answer::new(ResultCode::ConnectionClosed, "No opened data connection")))?;
        }
        if self.data_writer.is_some() {
            self.close_data_connection();
            self = await!(self.send(Answer::new(ResultCode::ClosingDataConnection, "Transfer done")))?;
        }
        Ok(self)
    }

    #[async]
    fn pasv(mut self) -> Result<Self, ()> {
        let port =
            if let Some(port) = self.data_port {
                port
            } else {
                0
            };
        if self.data_writer.is_some() {
            self = await!(self.send(Answer::new(ResultCode::DataConnectionAlreadyOpen, "Already listening...")))?;
            return Ok(self);
        }

        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), port);
        let listener = TcpListener::bind(&addr, &self.handle).unwrap(); // TODO: handle error
        let port = listener.local_addr().unwrap().port(); // TODO: handle error.

        self = await!(self.send(Answer::new(ResultCode::EnteringPassiveMode,
                              &format!("127,0,0,1,{},{}", port >> 8, port & 0xFF))))?;

        println!("Waiting clients on port {}...", port);
        // TODO: use into_future() instead of for loop?
        #[async]
        for (stream, _rest) in listener.incoming().map_err(|_| ()) { // TODO: handle error.
            let (writer, reader) = stream.framed(StringCodec).split();
            self.data_writer = Some(writer);
            self.data_reader = Some(reader);
            break;
        }
        Ok(self)
    }

    #[async]
    fn quit(mut self) -> Result<Self, ()> {
        if self.data_writer.is_some() {
            unimplemented!();
        } else {
            self = await!(self.send(Answer::new(ResultCode::ServiceClosingControlConnection, "Closing connection...")))?;
            self.writer.close().unwrap(); // TODO: handle error.
        }
        Ok(self)
    }

    #[async]
    fn retr(mut self, path: PathBuf) -> Result<Self, ()> {
        // TODO: check if multiple data connection can be opened at the same time.
        if self.data_writer.is_some() {
            let path = self.cwd.join(path);
            let (new_self, res) = self.complete_path(path.clone()); // TODO: ugly clone
            self = new_self;
            if let Ok(path) = res {
                if path.is_file() {
                    self = await!(self.send(Answer::new(ResultCode::DataConnectionAlreadyOpen, "Starting to send file...")))?;
                    let mut file = File::open(path).unwrap(); // TODO: handle error.
                    let mut out = String::new();
                    // TODO: send the file chunck by chunck if it is big (if needed).
                    file.read_to_string(&mut out).unwrap(); // TODO: handle error.
                    self = await!(self.send_data(out))?;
                    println!("-> file transfer done!");
                } else {
                    self = await!(self.send(Answer::new(ResultCode::LocalErrorInProcessing,
                                          &format!("\"{}\" doesn't exist", path.to_str().unwrap()))))?;
                }
            } else {
                self = await!(self.send(Answer::new(ResultCode::LocalErrorInProcessing,
                                      &format!("\"{}\" doesn't exist", path.to_str().unwrap()))))?;
            }
        } else {
            self = await!(self.send(Answer::new(ResultCode::ConnectionClosed, "No opened data connection")))?;
        }
        if self.data_writer.is_some() {
            self.close_data_connection();
            self = await!(self.send(Answer::new(ResultCode::ClosingDataConnection, "Transfer done")))?;
        }
        Ok(self)
    }

    #[async]
    fn stor(mut self, path: PathBuf) -> Result<Self, ()> {
        if self.data_reader.is_some() {
            let path = self.cwd.join(path);
            // TODO: check if path contains characters that are not allowed (cannot use
            // canonicalize for a non-existing path).
            self = await!(self.send(Answer::new(ResultCode::DataConnectionAlreadyOpen, "Starting to send file...")))?;
            let (data, new_self) = await!(self.receive_data())?;
            self = new_self;
            let mut file = File::create(path).unwrap(); // TODO: handle error.
            write!(file, "{}", data).unwrap(); // TODO: handle error.
            println!("-> file transfer done!");
            self.close_data_connection();
            self = await!(self.send(Answer::new(ResultCode::ClosingDataConnection, "Transfer done")))?;
        } else {
            self = await!(self.send(Answer::new(ResultCode::ConnectionClosed, "No opened data connection")))?;
        }
        Ok(self)
    }

    #[async]
    fn receive_data(mut self) -> Result<(String, Self), ()> {
        let mut file_data = String::new();
        // NOTE: have to use this weird trick because of futures-await.
        // TODO: fix that when the lifetime stuff is improved for generators.
        if self.data_reader.is_none() {
            return Ok((String::new(), self));
        }
        let reader = self.data_reader.take().unwrap();
        #[async]
        for data in reader.map_err(|_| ()) { // TODO: handle error.
            file_data.push_str(&data);
        }
        Ok((file_data, self))
    }

    #[async]
    fn send(mut self, answer: Answer) -> Result<Self, ()> {
        self.writer = await!(self.writer.send(answer)).map_err(|_| ())?; // TODO: handle error.
        Ok(self)
    }

    #[async]
    fn send_data(mut self, data: String) -> Result<Self, ()> {
        if let Some(writer) = self.data_writer {
            self.data_writer = Some(await!(writer.send(data)).map_err(|_| ())?); // TODO: handle error.
        }
        Ok(self)
    }
}

#[async]
fn handle_client(stream: TcpStream, handle: Handle, server_root: PathBuf) -> Result<(), ()> {
    let (writer, reader) = stream.framed(FtpCodec).split();
    let writer = await!(writer.send(Answer::new(ResultCode::ServiceReadyForNewUser, "Welcome to this FTP server!")))
        .map_err(|_| ())?; // TODO: handle error.
    let mut client = Client::new(handle, writer, server_root);
    #[async]
    for cmd in reader.map_err(|_| ()) { // TODO: handle error.
        client = await!(client.handle_cmd(cmd))?;
    }
    println!("Client closed");
    Ok(())
}

#[async]
fn server(handle: Handle, server_root: PathBuf) -> io::Result<()> {
    let port = 1234;
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), port);
    let listener = TcpListener::bind(&addr, &handle).unwrap();

    println!("Waiting clients on port {}...", port);
    #[async]
    for (stream, addr) in listener.incoming() {
        let address = format!("[address : {}]", addr);
        println!("New client: {}", address);
        handle.spawn(handle_client(stream, handle.clone(), server_root.clone()));
        println!("Waiting another client...");
    }
    Ok(())
}

fn main() {
    let mut core = Core::new().expect("Cannot create tokio Core");
    let handle = core.handle();

    match env::current_dir() {
        Ok(server_root) => {
            core.run(server(handle, server_root)).expect("Run tokio server");
        }
        Err(e) => println!("Couldn't start server: {:?}", e),
    }
}
