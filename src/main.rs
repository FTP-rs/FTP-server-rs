// Spec found at https://tools.ietf.org/html/rfc959

/*
 * FIXME Filezilla says: Le serveur ne supporte pas les caractères non-ASCII.
 * FIXME: The list command should specify which files are directory.
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

use std::ffi::OsStr;
use std::fs::{read_dir, DirEntry, Metadata};
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Component, PathBuf};

use futures::{Sink, Stream};
use futures::prelude::{async, await};
use futures::stream::SplitSink;
use tokio_core::reactor::{Core, Handle};
use tokio_core::net::{TcpListener, TcpStream};
use tokio_io::AsyncRead;
use tokio_io::codec::Framed;

use cmd::{Command, TransferType};
use codec::{FtpCodec, StringCodec};
use ftp::{Answer, ResultCode};

const DEFAULT_PORT: u16 = 4321;
const MONTHS: [&'static str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun",
                                     "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

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

fn add_file_info(entry: DirEntry, out: &mut String) {
    let path = entry.path(); // TODO: handle error.
    let extra = if path.is_dir() { "/" } else { "" };
    let is_dir = if path.is_dir() { "d" } else { "-" };

    let meta = ::std::fs::metadata(&path).unwrap(); // TODO: handle error.
    let (time, file_size) = get_file_info(&meta);
    let path = if path.starts_with("./") {
        path.strip_prefix("./")
            .unwrap() // TODO: handle error.
            .to_str()
            .unwrap() // TODO: handle error.
    } else {
        path.to_str().unwrap()
    };
    let file_str = format!("{} {} {} {} {}:{} {}{}\r\n",
                           is_dir,
                           file_size,
                           MONTHS[time.tm_mon as usize],
                           time.tm_mday,
                           time.tm_hour,
                           time.tm_min,
                           path,
                           extra);
    out.push_str(&file_str);
    println!("==> {:?}", &file_str);
}

#[allow(dead_code)]
struct Client {
    address: String, // TODO: remove this?
    cwd: PathBuf,
    data_port: Option<u16>,
    data_writer: Option<DataWriter>,
    handle: Handle,
    name: Option<String>,
    transfer_type: TransferType,
    writer: Writer,
}

impl Client {
    fn new(address: String, handle: Handle, writer: Writer) -> Client {
        Client {
            address,
            cwd: PathBuf::from("/"),
            data_port: None,
            data_writer: None,
            handle,
            name: None,
            transfer_type: TransferType::Ascii,
            writer,
        }
    }

    #[async]
    fn handle_cmd(mut self, cmd: Command) -> Result<Self, ()> {
        println!("Received command: {:?}", cmd);
        match cmd {
            Command::Auth =>
                // TODO: create a macro await_self to avoid this awkward syntax?
                self = await!(self.send(Answer::new(ResultCode::CommandNotImplemented, "Not implemented")))?
            ,
            Command::Cwd(directory) => {
                // TODO: Actually implement the command. Since chroot works only on UNIX
                // platforms, we can't use it for that. :'(
                self = await!(self.send(Answer::new(ResultCode::Ok, &format!("Directory changed to \"{}\"", directory))))?;
            },
            Command::List(path) => self = await!(self.list(path))?,
            Command::Pasv => self = await!(self.pasv())?,
            Command::Port(port) => {
                self.data_port = Some(port);
                self = await!(self.send(Answer::new(ResultCode::Ok, &format!("Data port is now {}", port))))?;
            },
            Command::Pwd => {
                let message = format!("\"{}\" ", self.cwd.to_str().unwrap());
                self = await!(self.send(Answer::new(ResultCode::PATHNAMECreated, &message)))?;
            }, // TODO: handle error.
            Command::Syst => {
                self = await!(self.send(Answer::new(ResultCode::Ok, "I won't tell!")))?;
            },
            Command::Type(typ) => {
                self.transfer_type = typ;
                self = await!(self.send(Answer::new(ResultCode::Ok, "Transfer type changed successfully")))?;
            },
            Command::Unknown =>
                self = await!(self.send(Answer::new(ResultCode::UnknownCommand, "Not implemented")))?
            ,
            Command::User(content) => {
                if content.is_empty() {
                    self = await!(self.send(Answer::new(ResultCode::InvalidParameterOrArgument, "Invalid username")))?;
                } else {
                    self.name = Some(content.to_owned());
                    self = await!(self.send(Answer::new(ResultCode::UserloggedIn, &format!("Welcome {}!", content))))?;
                }
            }
        }
        Ok(self)
    }

    #[async]
    fn list(mut self, path: Option<PathBuf>) -> Result<Self, ()> {
        if self.data_writer.is_some() {
            let mut tmp = PathBuf::from(".");
            {
                let path = path.as_ref().unwrap_or(&self.cwd);
                // TODO: would it be better to check if the directory is a child on the root?
                for item in path.components().skip(1) {
                    match item {
                        Component::Normal(ref s) if s != &OsStr::new("..") => tmp.push(s),
                        _ => {}
                    }
                }
            }
            if tmp.is_dir() {
                self = await!(self.send(Answer::new(ResultCode::DataConnectionAlreadyOpen, "Starting to list directory...")))?;
                let mut out = String::new();
                for entry in read_dir(tmp).unwrap() { // TODO: handle error.
                    add_file_info(entry.unwrap(), &mut out); // TODO: handle error.
                }
                self = await!(self.send_data(out))?;
                println!("-> and done!");
            } else {
                self = await!(self.send(Answer::new(ResultCode::LocalErrorInProcessing,
                                      &format!("\"{}\" doesn't exist", tmp.to_str().unwrap()))))?;
            }
        } else {
            self = await!(self.send(Answer::new(ResultCode::ConnectionClosed, "No opened data connection")))?;
        }
        if self.data_writer.is_some() {
            self.data_writer = None;
            self = await!(self.send(Answer::new(ResultCode::ClosingDataConnection, "Transfer done")))?;
        }
        Ok(self)
    }

    #[async]
    fn pasv(mut self) -> Result<Self, ()> {
        // TODO: I believe this command should be blocking.
        let port =
            if let Some(port) = self.data_port {
                port
            } else {
                DEFAULT_PORT
            };
        if self.data_writer.is_some() {
            self = await!(self.send(Answer::new(ResultCode::DataConnectionAlreadyOpen, "Already listening...")))?;
            return Ok(self);
        }
        self = await!(self.send(Answer::new(ResultCode::EnteringPassiveMode,
                              &format!("127,0,0,1,{},{}", port >> 8, port & 0xFF))))?;

        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), port);
        let listener = TcpListener::bind(&addr, &self.handle).unwrap(); // TODO: handle error

        println!("Waiting clients on port {}...", port);
        // TODO: use into_future() instead of for loop?
        #[async]
        for (stream, _rest) in listener.incoming().map_err(|_| ()) {
            let (writer, _reader) = stream.framed(StringCodec).split();
            self.data_writer = Some(writer);
            break;
        }
        Ok(self)
    }

    #[async]
    fn send(mut self, answer: Answer) -> Result<Self, ()> {
        self.writer = await!(self.writer.send(answer)).map_err(|_| ())?;
        Ok(self)
    }

    #[async]
    fn send_data(mut self, data: String) -> Result<Self, ()> {
        if let Some(mut writer) = self.data_writer {
            self.data_writer = Some(await!(writer.send(data)).map_err(|_| ())?);
        }
        Ok(self)
    }
}

#[async]
fn handle_client(stream: TcpStream, handle: Handle, address: String) -> Result<(), ()> {
    let (writer, reader) = stream.framed(FtpCodec).split();
    let writer = await!(writer.send(Answer::new(ResultCode::ServiceReadyForNewUser, "Welcome to this FTP server!")))
        .map_err(|_| ())?;
    let mut client = Client::new(address, handle, writer);
    #[async]
    for cmd in reader.map_err(|_| ()) {
        client = await!(client.handle_cmd(cmd))?;
    }
    Ok(())
}

#[async]
fn server(handle: Handle) -> io::Result<()> {
    let port = 1234;
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), port);
    let listener = TcpListener::bind(&addr, &handle).unwrap();

    println!("Waiting clients on port {}...", port);
    #[async]
    for (stream, addr) in listener.incoming() {
        let address = format!("[address : {}]", addr);
        println!("New client: {}", address);
        handle.spawn(handle_client(stream, handle.clone(), address));
        println!("Waiting another client...");
    }
    Ok(())
}

fn main() {
    let mut core = Core::new()
        .expect("Cannot create tokio Core");
    let handle = core.handle();

    core.run(server(handle))
        .expect("Run tokio server");
}
