use std::io;
use std::result;
use std::str::Utf8Error;
use std::string::FromUtf8Error;

use self::Error::*;

pub enum Error {
    FromUtf8(FromUtf8Error),
    Io(io::Error),
    Msg(String),
    Utf8(Utf8Error),
}

impl Error {
    pub fn to_io_error(self) -> io::Error {
        match self {
            Io(error) => error,
            FromUtf8(_) | Msg(_) | Utf8(_) => io::ErrorKind::Other.into(),
        }
    }
}

pub type Result<T> = result::Result<T, Error>;

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Io(error)
    }
}

impl<'a> From<&'a str> for Error {
    fn from(message: &'a str) -> Self {
        Msg(message.to_string())
    }
}

impl From<Utf8Error> for Error {
    fn from(error: Utf8Error) -> Self {
        Utf8(error)
    }
}

impl From<FromUtf8Error> for Error {
    fn from(error: FromUtf8Error) -> Self {
        FromUtf8(error)
    }
}
