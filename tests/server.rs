extern crate ftp;

use std::process::Command;
use std::thread;
use std::time::Duration;

use ftp::FtpStream;

struct Teardown<F: FnMut()> {
    callback: F,
}

impl<F: FnMut()> Drop for Teardown<F> {
    fn drop(&mut self) {
        (self.callback)();
    }
}

fn defer<F: FnMut()>(callback: F) -> Teardown<F> {
    Teardown {
        callback,
    }
}

#[test]
fn test_pwd() {
    let mut child = Command::new("./target/debug/ftp-server")
        .spawn().unwrap();
    let _teardown = defer(move || {
        let _ = child.kill();
    });

    thread::sleep(Duration::from_millis(100));

    let mut ftp = FtpStream::connect("127.0.0.1:1234").unwrap();

    let pwd = ftp.pwd().unwrap();
    assert_eq!("/", pwd);

    ftp.login("ferris", "").unwrap();

    ftp.cwd("src").unwrap();
    let pwd = ftp.pwd().unwrap();
    assert_eq!("/src", pwd);

    let _ = ftp.cdup();
    let pwd = ftp.pwd().unwrap();
    assert_eq!("/", pwd);

    ftp.quit().unwrap();
}
