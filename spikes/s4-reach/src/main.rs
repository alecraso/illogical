//! S4 probe: a stand-in for illogicald inside a sandbox.
//! `s4-probe serve ADDR` accepts TCP connections; each gets a fresh login shell
//! on its own PTY, bridged as raw bytes. Logs go to stderr.
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::Instant;

fn openpty() -> (OwnedFd, OwnedFd) {
    let (mut m, mut s) = (0, 0);
    let ws = libc::winsize { ws_row: 40, ws_col: 120, ws_xpixel: 0, ws_ypixel: 0 };
    let rc = unsafe { libc::openpty(&mut m, &mut s, std::ptr::null_mut(), std::ptr::null(), &ws) };
    assert_eq!(rc, 0, "openpty failed: {}", std::io::Error::last_os_error());
    unsafe {
        libc::fcntl(m, libc::F_SETFD, libc::FD_CLOEXEC);
        (OwnedFd::from_raw_fd(m), OwnedFd::from_raw_fd(s))
    }
}

fn handle(conn: TcpStream, started: Instant) {
    let peer = conn.peer_addr().map(|a| a.to_string()).unwrap_or_default();
    let (master, slave) = openpty();
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into());
    let mut cmd = Command::new(&shell);
    cmd.arg("-l")
        .env("TERM", "xterm-256color")
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave));
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            libc::ioctl(0, libc::TIOCSCTTY as _, 0);
            Ok(())
        });
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return eprintln!("spawn {shell}: {e}"),
    };
    eprintln!("[{:?}] conn {peer}: pid {} on pty", started.elapsed(), child.id());
    let mut m_r = std::fs::File::from(master.try_clone().unwrap());
    let mut m_w = std::fs::File::from(master);
    let mut c_r = conn.try_clone().unwrap();
    let mut c_w = conn;
    std::thread::spawn(move || {
        let mut buf = [0u8; 16384];
        while let Ok(n) = c_r.read(&mut buf) {
            if n == 0 || m_w.write_all(&buf[..n]).is_err() { break; }
        }
    });
    let mut buf = [0u8; 16384];
    while let Ok(n) = m_r.read(&mut buf) {
        if n == 0 || c_w.write_all(&buf[..n]).is_err() { break; }
    }
    let _ = child.kill();
    let _ = child.wait();
    eprintln!("conn {peer}: closed");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let addr = args.get(2).cloned().unwrap_or_else(|| "127.0.0.1:7681".into());
    let started = Instant::now();
    let l = TcpListener::bind(&addr).expect("bind");
    eprintln!("s4-probe listening on {addr} (pid {})", std::process::id());
    for c in l.incoming().flatten() {
        std::thread::spawn(move || handle(c, started));
    }
}
