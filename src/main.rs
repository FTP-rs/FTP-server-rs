// Spec found at https://tools.ietf.org/html/rfc959

/*
 * FIXME Filezilla says: Le serveur ne supporte pas les caractères non-ASCII.
 * FIXME: The list command should specify which files are directory.
 */

extern crate bytes;
#[macro_use]
extern crate cfg_if;
extern crate futures;
extern crate time;
extern crate tokio_core;
extern crate tokio_io;

mod cmd; // FIXME: rename this module.
mod codec;
mod ftp;

use std::cell::RefCell;
use std::ffi::OsStr;
use std::fmt::Display;
use std::fs::{read_dir, DirEntry, Metadata};
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Component, PathBuf};
use std::rc::Rc;

use futures::{AsyncSink, Future, Sink, Stream};
use futures::stream::SplitSink;
use tokio_core::reactor::{Core, Handle};
use tokio_core::net::{TcpListener, TcpStream};
use tokio_io::AsyncRead;
use tokio_io::codec::{Encoder, Framed};

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
    address: String,
    cwd: PathBuf,
    data_port: Option<u16>,
    data_writer: Rc<RefCell<Option<DataWriter>>>,
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
            data_writer: Rc::new(RefCell::new(None)),
            handle,
            name: None,
            transfer_type: TransferType::Ascii,
            writer,
        }
    }

    fn handle_cmd(&mut self, cmd: Command) {
        println!("Received command: {:?}", cmd);
        match cmd {
            Command::Auth => self.send(Answer::new(ResultCode::CommandNotImplemented, "Not implemented")),
            Command::Cwd(directory) => {
                // TODO: Actually implement the command. Since chroot works only on UNIX
                // platforms, we can't use it for that. :'(
                self.send(Answer::new(ResultCode::Ok, &format!("Directory changed to \"{}\"", directory)));
            },
            Command::List(path) => self.list(path),
            Command::Pasv => self.pasv(),
            Command::Port(port) => {
                self.data_port = Some(port);
                self.send(Answer::new(ResultCode::Ok, &format!("Data port is now {}", port)));
            },
            Command::Pwd => {
                let message = format!("\"{}\" ", self.cwd.to_str().unwrap());
                self.send(Answer::new(ResultCode::PATHNAMECreated, &message))
            }, // TODO: handle error.
            Command::Syst => self.send(Answer::new(ResultCode::Ok, "I won't tell!")),
            Command::Type(typ) => {
                self.transfer_type = typ;
                self.send(Answer::new(ResultCode::Ok, "Transfer type changed successfully"));
            },
            Command::Unknown => self.send(Answer::new(ResultCode::UnknownCommand, "Not implemented")),
            Command::User(content) => {
                if content.is_empty() {
                    self.send(Answer::new(ResultCode::InvalidParameterOrArgument, "Invalid username"))
                } else {
                    self.name = Some(content.to_owned());
                    self.send(Answer::new(ResultCode::UserloggedIn, &format!("Welcome {}!", content)))
                }
            }
        }
    }

    fn list(&mut self, path: Option<PathBuf>) {
        if self.data_writer.borrow().is_some() {
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
                self.send(Answer::new(ResultCode::DataConnectionAlreadyOpen, "Starting to list directory..."));
                let mut out = String::new();
                for entry in read_dir(tmp).unwrap() { // TODO: handle error.
                    add_file_info(entry.unwrap(), &mut out); // TODO: handle error.
                }
                self.send_data(out);
                println!("-> and done!");
            } else {
                self.send(Answer::new(ResultCode::LocalErrorInProcessing,
                                      &format!("\"{}\" doesn't exist", tmp.to_str().unwrap())))
            }
        } else {
            self.send(Answer::new(ResultCode::ConnectionClosed, "No opened data connection"));
        }
        if self.data_writer.borrow().is_some() {
            *self.data_writer.borrow_mut() = None;
            self.send(Answer::new(ResultCode::ClosingDataConnection, "Transfer done"));
        }
    }

    fn pasv(&mut self) {
        // TODO: I believe this command should be blocking.
        let port =
            if let Some(port) = self.data_port {
                port
            } else {
                DEFAULT_PORT
            };
        if self.data_writer.borrow().is_some() {
            return self.send(Answer::new(ResultCode::DataConnectionAlreadyOpen, "Already listening..."));
        }
        self.send(Answer::new(ResultCode::EnteringPassiveMode,
                              &format!("127,0,0,1,{},{}", port >> 8, port & 0xFF)));

        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), port);
        let listener = TcpListener::bind(&addr, &self.handle).unwrap(); // TODO: handle error

        println!("Waiting clients on port {}...", port);
        let data_writer = self.data_writer.clone();
        let future = listener.incoming()
            .into_future()
            .map_err(|_| ())
            .and_then(move |(client, _rest)| {
                if let Some((stream, _addr)) = client {
                    let (writer, _reader) = stream.framed(StringCodec).split();
                    *data_writer.borrow_mut() = Some(writer);
                }
                Ok(())
            });
        self.handle.spawn(future);
    }

    fn send(&mut self, answer: Answer) {
        send(&mut self.writer, answer);
    }

    fn send_data(&mut self, data: String) {
        if let Some(ref mut writer) = *self.data_writer.borrow_mut() {
            send(writer, data);
        }
    }
}

fn handle_client(stream: TcpStream, handle: Handle, address: String) -> Box<Future<Item=(), Error=io::Error>> {
    let (writer, reader) = stream.framed(FtpCodec).split();
    Box::new(writer.send(Answer::new(ResultCode::ServiceReadyForNewUser, "Welcome to this FTP server!"))
        .and_then(|writer| {
              let mut client = Client::new(address, handle, writer);
              reader.for_each(move |cmd| {
                  client.handle_cmd(cmd);
                  Ok(())
              })
        }
    ))
}

fn main() {
    let mut core = Core::new()
        .expect("Cannot create tokio Core");
    let handle = core.handle();

    let port = 1234;
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), port);
    let listener = TcpListener::bind(&addr, &handle).unwrap();

    println!("Waiting clients on port {}...", port);
    let server = listener.incoming()
        .for_each(|(stream, addr)| {
            let address = format!("[address : {}]", addr);

            println!("New client: {}", address);
            let future = handle_client(stream, handle.clone(), address);
            println!("Waiting another client...");
            future
        });

    core.run(server)
        .expect("Run tokio server");
}

fn send<S: Encoder>(writer: &mut SplitSink<Framed<TcpStream, S>>, data: S::Item)
where S::Error: Display
{
    // TODO: not sure about that. Do you know a better way of doing it?
    let mut error = None;
    match writer.start_send(data) {
        Ok(AsyncSink::Ready) => {
            if let Err(poll_error) = writer.poll_complete() {
                error = Some(poll_error.to_string());
            }
        },
        Ok(AsyncSink::NotReady(_)) => error = Some("not ready to send to client".to_string()),
        Err(send_error) =>
            error = Some(format!("cannot send a message to the web process: {}", send_error)),
    }
    if let Some(error) = error {
        panic!("Error: {}", error); // Handle error.
    }
}
