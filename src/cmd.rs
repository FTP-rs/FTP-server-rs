use std::io;
use std::path::{Path, PathBuf};
use std::str::{self, FromStr};

#[derive(Clone, Debug)]
pub enum Command {
    Auth,
    Cwd(String),
    List(Option<PathBuf>),
    Port(u16),
    Pasv,
    Pwd,
    Syst,
    Type(TransferType),
    Unknown,
    User(String),
}

impl AsRef<str> for Command {
    fn as_ref(&self) -> &str {
        match *self {
            Command::Auth => "AUTH",
            Command::Cwd(_) => "CWD",
            Command::List(_) => "LIST",
            Command::Pasv => "PASV",
            Command::Port(_) => "PORT",
            Command::Pwd => "PWD",
            Command::Syst => "SYST",
            Command::Type(_) => "TYPE",
            Command::Unknown => "UNKN", // doesn't exist
            Command::User(_) => "USER",
        }
    }
}

impl Command {
    pub fn new(input: Vec<u8>) -> io::Result<Self> {
        let mut iter = input.split(|&byte| byte == b' ');
        let mut command = iter.next().expect("command in input").to_vec(); // TODO: handle error.
        to_uppercase(&mut command);
        let data = iter.next();
        let command =
            match command.as_slice() {
                b"AUTH" => Command::Auth,
                b"CWD" => Command::Cwd(data.map(|bytes| String::from_utf8(bytes.to_vec())
                              .expect("cannot convert bytes to String")).unwrap_or_default()), // TODO: handle error.
                b"LIST" => Command::List(data.map(|bytes| Path::new(str::from_utf8(bytes).unwrap()).to_path_buf())), // TODO: handle error.
                b"PASV" => Command::Pasv,
                b"PORT" => {
                    let addr = data.unwrap().split(|&byte| byte == b',') // TODO: handle error.
                        .filter_map(|bytes| u8::from_str(str::from_utf8(bytes).unwrap()).ok()) // TODO: handle error.
                        .collect::<Vec<u8>>();
                    if addr.len() != 6 {
                        panic!("Invalid address/port"); // TODO. handle error.
                    }

                    let port = (addr[4] as u16) << 8 | (addr[5] as u16);
                    // TODO: check if the port isn't already used already by another connection...
                    if port <= 1000 { // TODO: isn't it 1024?
                        panic!("Port can't be less than 1001"); // TODO: handle error.
                    }
                    Command::Port(port)
                },
                b"PWD" => Command::Pwd,
                b"SYST" => Command::Syst,
                b"TYPE" => {
                    match TransferType::from(data.unwrap()[0]) { // TODO: handle error.
                        TransferType::Unknown => panic!("command not implemented for that parameter"), // TODO: handle error.
                        typ => {
                            Command::Type(typ)
                        },
                    }
                },
                b"USER" => Command::User(data.map(|bytes| String::from_utf8(bytes.to_vec())
                              .expect("cannot convert bytes to String")).unwrap_or_default()), // TODO: handle error.
                _ => Command::Unknown,
            };
        Ok(command)
    }
}

fn to_uppercase(data: &mut [u8]) {
    for byte in data {
        if *byte >= 'a' as u8 && *byte <= 'z' as u8 {
            *byte -= 32;
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum TransferType {
    Ascii,
    Image,
    Unknown,
}

impl From<u8> for TransferType {
    fn from(c: u8) -> TransferType {
        match c {
            b'A' => TransferType::Ascii,
            b'I' => TransferType::Image,
            _ => TransferType::Unknown,
        }
    }
}
