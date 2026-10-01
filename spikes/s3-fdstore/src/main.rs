//! S3: does a pane's shell survive `systemctl --user restart` of the daemon?
//!
//! First start: open a PTY, run bash in its own transient scope (so the
//! service's cgroup kill on restart misses it), hand the PTY master to
//! systemd's fd store, and probe the shell. After a restart: take the master
//! back from LISTEN_FDS and probe again. Same shell pid = pass.

use std::{
    io::IoSlice,
    os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use nix::{
    poll::{PollFd, PollFlags, PollTimeout, poll},
    pty::openpty,
    sys::socket::{
        AddressFamily, ControlMessage, MsgFlags, SockFlag, SockType, UnixAddr, sendmsg, socket,
    },
    unistd::{getpid, read, write},
};

const FD_NAME: &str = "pane-1";

fn sd_notify(state: &str, fds: &[RawFd]) {
    let Ok(path) = std::env::var("NOTIFY_SOCKET") else {
        eprintln!("no NOTIFY_SOCKET; not under systemd?");
        return;
    };
    let addr = match path.strip_prefix('@') {
        Some(abs) => UnixAddr::new_abstract(abs.as_bytes()).unwrap(),
        None => UnixAddr::new(path.as_str()).unwrap(),
    };
    let sock = socket(AddressFamily::Unix, SockType::Datagram, SockFlag::SOCK_CLOEXEC, None).unwrap();
    let iov = [IoSlice::new(state.as_bytes())];
    let cmsg = [ControlMessage::ScmRights(fds)];
    let cmsgs: &[ControlMessage] = if fds.is_empty() { &[] } else { &cmsg };
    sendmsg(sock.as_raw_fd(), &iov, cmsgs, MsgFlags::empty(), Some(&addr)).unwrap();
}

/// fds passed back by systemd: (name, fd), per sd_listen_fds_with_names.
fn listen_fds() -> Vec<(String, OwnedFd)> {
    let ours = std::env::var("LISTEN_PID").ok().and_then(|p| p.parse::<i32>().ok()) == Some(getpid().as_raw());
    if !ours {
        return vec![];
    }
    let n: i32 = std::env::var("LISTEN_FDS").ok().and_then(|n| n.parse().ok()).unwrap_or(0);
    let names = std::env::var("LISTEN_FDNAMES").unwrap_or_default();
    let names: Vec<&str> = names.split(':').collect();
    (0..n)
        .map(|i| {
            let name = names.get(i as usize).copied().unwrap_or("unknown").to_string();
            (name, unsafe { OwnedFd::from_raw_fd(3 + i) })
        })
        .collect()
}

/// Ask the shell for its pid through the PTY and read the answer back.
fn probe(master: BorrowedFd, tag: &str) -> Option<u32> {
    let marker = format!("MARK-{tag} pid=");
    write(master, format!("echo {marker}$$\n").as_bytes()).unwrap();
    let mut seen = String::new();
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        let mut pfd = [PollFd::new(master, PollFlags::POLLIN)];
        if poll(&mut pfd, PollTimeout::from(200u16)).unwrap() == 0 {
            continue;
        }
        let mut buf = [0u8; 4096];
        match read(master, &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => seen.push_str(&String::from_utf8_lossy(&buf[..n])),
        }
        // The echoed command line contains "pid=$$"; the output has digits.
        for (i, _) in seen.match_indices(&marker) {
            let digits: String = seen[i + marker.len()..].chars().take_while(char::is_ascii_digit).collect();
            if !digits.is_empty() && seen[i + marker.len() + digits.len()..].contains('\n') {
                return digits.parse().ok();
            }
        }
    }
    eprintln!("probe saw: {seen:?}");
    None
}

fn main() {
    let restored = listen_fds();
    eprintln!("start pid={} restored fds={:?}", getpid(), restored.iter().map(|(n, f)| (n, f.as_raw_fd())).collect::<Vec<_>>());

    let master = if let Some((_, fd)) = restored.into_iter().find(|(n, _)| n == FD_NAME) {
        let pid = probe(fd.as_fd_borrowed(), "restored");
        eprintln!("RESULT restored master; shell pid via PTY = {pid:?}");
        fd
    } else {
        let pty = openpty(None, None).unwrap();
        // openpty does not set CLOEXEC; without it the shell inherits its own
        // master, never sees a hangup, and outlives a clean stop.
        nix::fcntl::fcntl(&pty.master, nix::fcntl::FcntlArg::F_SETFD(nix::fcntl::FdFlag::FD_CLOEXEC)).unwrap();
        let slave = |fd: &OwnedFd| Stdio::from(fd.try_clone().unwrap());
        // --scope: systemd-run execs bash in place, inside a new transient
        // scope unit, so it is outside this service's cgroup. setsid -c makes
        // the PTY its controlling terminal.
        let child = Command::new("systemd-run")
            .args(["--user", "--scope", "--quiet", "--collect"])
            .arg(format!("--unit=s3-pane-{}", getpid()))
            .arg("--")
            .args(["setsid", "-c", "bash", "--norc", "--noprofile"])
            .stdin(slave(&pty.slave))
            .stdout(slave(&pty.slave))
            .stderr(slave(&pty.slave))
            .spawn()
            .unwrap();
        drop(pty.slave);
        eprintln!("spawned shell, systemd-run pid {}", child.id());
        std::thread::sleep(Duration::from_millis(500));
        let pid = probe(pty.master.as_fd_borrowed(), "fresh");
        eprintln!("RESULT fresh shell pid via PTY = {pid:?}");
        sd_notify(&format!("FDSTORE=1\nFDNAME={FD_NAME}\nFDPOLL=0"), &[pty.master.as_raw_fd()]);
        eprintln!("stored master in fd store");
        pty.master
    };

    sd_notify("READY=1", &[]);
    let _keep = master;
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

trait Borrow {
    fn as_fd_borrowed(&self) -> BorrowedFd<'_>;
}
impl Borrow for OwnedFd {
    fn as_fd_borrowed(&self) -> BorrowedFd<'_> {
        use std::os::fd::AsFd;
        self.as_fd()
    }
}
