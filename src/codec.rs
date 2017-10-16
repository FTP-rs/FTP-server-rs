use std::io;

use bytes::BytesMut;
use tokio_io::codec::{Decoder, Encoder};

use cmd::Command;
use ftp::Answer;

pub struct FtpCodec;
pub struct StringCodec;

impl Decoder for FtpCodec {
    type Item = Command;
    type Error = io::Error;

    fn decode(&mut self, buf: &mut BytesMut) -> io::Result<Option<Command>> {
        if let Some(index) = find_crlf(buf) {
            let line = buf.split_to(index);
            buf.split_to(2); // Remove \r\n.
            Command::new(line.to_vec())
                .map(|command| Some(command))
        } else {
            Ok(None)
        }
    }
}

impl Encoder for FtpCodec {
    type Item = Answer;
    type Error = io::Error;

    fn encode(&mut self, answer: Answer, buf: &mut BytesMut) -> io::Result<()> {
        // TODO: avoid using String?
        let answer =
            if answer.message.is_empty() {
                format!("{}\r\n", answer.code as u32)
            } else {
                format!("{} {}\r\n", answer.code as u32, answer.message)
            };
        buf.extend(answer.as_bytes());
        Ok(())
    }
}

impl Decoder for StringCodec {
    type Item = String;
    type Error = io::Error;

    fn decode(&mut self, buf: &mut BytesMut) -> io::Result<Option<String>> {
        if buf.len() == 0 {
            return Ok(None);
        }
        let data = String::from_utf8(buf.to_vec()).unwrap(); // TODO: handle error.
        Ok(Some(data))
    }
}

impl Encoder for StringCodec {
    type Item = String; // TODO: use &[u8] instead?
    type Error = io::Error;

    fn encode(&mut self, data: String, buf: &mut BytesMut) -> io::Result<()> {
        buf.extend(data.as_bytes());
        Ok(())
    }
}

fn find_crlf(buf: &mut BytesMut) -> Option<usize> {
    buf.windows(2)
        .position(|bytes| bytes == b"\r\n")
}
