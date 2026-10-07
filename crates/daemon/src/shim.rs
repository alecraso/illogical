//! `arugulad _shim --record FILE [--hold SOCKET] -- PROGRAM ARGS...`
//!
//! Sits between the daemon and a pane's program so the daemon can be
//! restarted without its panes noticing. The shim forks the program as the
//! session leader on the PTY (its stdin), records the program's pid and start
//! time, waits for it, and records how it ended. The daemon that started it
//! may be long gone by then; whichever daemon is running reads the record.
//!
//! Record lines (appended):
//! ```text
//! pid <pid> <starttime>      once the program is running
//! shim <pid> <starttime>     right after: who to ask for a close
//! exit <code>                or
//! signal <n>                 when it ends
//! ```
//!
//! The pid is recorded only once the program has exec'd, so it is already
//! in its own session and process group: a hangup sent to that group can't
//! be lost (#35).
//!
//! A close: the daemon hangs up the program's group and sends the shim
//! [`CLOSE`]. The shim hangs the group up too and, if anything is left a few
//! seconds later, kills it. That doesn't depend on the daemon still running.
//!
//! With `--hold`, the daemon has passed the PTY master as fd 3, and the shim
//! keeps it for the next daemon (see [`crate::holder`]): there is no
//! systemd FD store to keep it.
//!
//! It must run before any threads exist (it forks), so `main` dispatches to
//! it before starting the async runtime.

#[cfg(unix)]
use std::{
    ffi::CString,
    fs::OpenOptions,
    io::Write,
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    sync::atomic::{AtomicBool, AtomicI32, Ordering},
};

#[cfg(unix)]
use nix::{
    fcntl::{FcntlArg, FdFlag, fcntl},
    libc,
    sys::{
        signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction},
        wait::{WaitStatus, waitpid},
    },
    unistd::{ForkResult, execvp, fork, pipe, setsid},
};

#[cfg(unix)]
/// What the daemon sends the shim to close the pane.
pub const CLOSE: Signal = Signal::SIGUSR1;
#[cfg(unix)]
/// How long a closed program has to go after its hangup before it's killed.
const KILL_AFTER: u32 = 3;

#[cfg(unix)]
/// The program's pid (and process group), for the signal handlers.
static CHILD: AtomicI32 = AtomicI32::new(0);
#[cfg(unix)]
static CLOSING: AtomicBool = AtomicBool::new(false);
#[cfg(unix)]
static KILLED: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn on_close(_: libc::c_int) {
    let child = CHILD.load(Ordering::SeqCst);
    if child > 0 && !CLOSING.swap(true, Ordering::SeqCst) {
        // SAFETY: killpg and alarm are async-signal-safe.
        unsafe {
            libc::killpg(child, libc::SIGHUP);
            libc::alarm(KILL_AFTER);
        }
    }
}

#[cfg(unix)]
extern "C" fn on_alarm(_: libc::c_int) {
    let child = CHILD.load(Ordering::SeqCst);
    if child > 0 {
        // SAFETY: async-signal-safe.
        unsafe { libc::killpg(child, libc::SIGKILL) };
    }
    KILLED.store(true, Ordering::SeqCst);
}

#[cfg(unix)]
pub fn run(args: &[String]) -> ! {
    let (record, hold, argv) = match parse(args) {
        Some(x) => x,
        None => {
            eprintln!("usage: arugulad _shim --record FILE [--hold SOCKET] -- PROGRAM [ARGS...]");
            std::process::exit(2);
        }
    };
    let cargs: Vec<CString> = argv.iter().map(|a| CString::new(a.as_str()).unwrap_or_default()).collect();
    let holder = hold.and_then(|socket| match crate::holder::Holder::prepare(socket.as_ref(), HELD_FD) {
        Ok(h) => Some(h),
        Err(e) => {
            // The pane still runs; it just won't outlive the daemon.
            eprintln!("arugula: can't keep the terminal ({e})\r");
            None
        }
    });

    // The exec handshake: the child holds the write end until exec closes
    // it, or writes a byte if exec fails.
    let (ready_r, ready_w) = match pipe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("arugula: pipe failed: {e}\r");
            std::process::exit(126);
        }
    };
    let _ = fcntl(&ready_w, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC));
    let _ = fcntl(&ready_r, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC));

    // SAFETY: single-threaded here (no runtime yet), so fork is sound.
    match unsafe { fork() } {
        Ok(ForkResult::Child) => {
            drop(ready_r);
            // A new session with the PTY as its controlling terminal, so the
            // shell does job control as if the daemon had started it.
            let _ = setsid();
            // SAFETY: TIOCSCTTY on our stdin, the PTY slave.
            unsafe { libc::ioctl(0, libc::TIOCSCTTY as _, 0) };
            let err = execvp(&cargs[0], &cargs).unwrap_err();
            // SAFETY: one byte from our own buffer.
            unsafe { libc::write(ready_w.as_raw_fd(), b"x".as_ptr().cast(), 1) };
            let _ = writeln!(std::io::stderr(), "arugula: can't run {}: {err}\r", argv[0]);
            std::process::exit(127);
        }
        Ok(ForkResult::Parent { child }) => {
            drop(ready_w);
            // Wait for the exec (EOF) or its failure (a byte). Either way the
            // child has its own session by now.
            let mut b = [0u8; 1];
            // SAFETY: reading into our own buffer.
            while unsafe { libc::read(ready_r.as_raw_fd(), b.as_mut_ptr().cast(), 1) } < 0
                && nix::errno::Errno::last() == nix::errno::Errno::EINTR
            {}
            drop(ready_r);
            // A terminal that hung up before the program made it its own
            // (the daemon went while starting it) won't hang up again.
            let mut tty = libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 };
            // SAFETY: polls one descriptor, without waiting.
            let hung_up = unsafe { libc::poll(&mut tty, 1, 0) } > 0 && tty.revents & libc::POLLHUP != 0;
            // Let go of the PTY: only the program should hold it, so the
            // terminal hangs up when it (and its children) are gone.
            if let Ok(null) = OpenOptions::new().read(true).write(true).open("/dev/null") {
                for fd in 0..3 {
                    // SAFETY: replacing our own standard descriptors.
                    unsafe { libc::dup2(null.as_raw_fd(), fd) };
                }
            }
            // Ignore the terminal's signals; this process outlives nothing
            // but its child.
            // SAFETY: setting dispositions to ignore.
            unsafe {
                libc::signal(libc::SIGHUP, libc::SIG_IGN);
                libc::signal(libc::SIGINT, libc::SIG_IGN);
                libc::signal(libc::SIGQUIT, libc::SIG_IGN);
            }
            CHILD.store(child.as_raw(), Ordering::SeqCst);
            for (sig, handler) in [(CLOSE, on_close as extern "C" fn(libc::c_int)), (Signal::SIGALRM, on_alarm)] {
                let action = SigAction::new(SigHandler::Handler(handler), SaFlags::SA_RESTART, SigSet::empty());
                // SAFETY: the handlers only touch atomics and make
                // async-signal-safe calls.
                let _ = unsafe { sigaction(sig, &action) };
            }
            let pid = child.as_raw() as u32;
            // Listening before the pid is recorded: the daemon waits for the
            // record, then connects.
            let socket = holder.as_ref().map(|h| h.socket().to_owned());
            if let Some(h) = holder {
                std::thread::spawn(move || h.serve(child.as_raw()));
            }
            let me = std::process::id();
            let recorded = append(
                &record,
                &format!("pid {pid} {}\nshim {me} {}\n", start_time(pid).unwrap_or(0), start_time(me).unwrap_or(0)),
            );
            // Nobody is left to close it, or could find it to: close it now.
            if hung_up || !recorded {
                on_close(0);
            }
            let status = loop {
                match waitpid(child, None) {
                    Ok(WaitStatus::Exited(_, code)) => break format!("exit {code}\n"),
                    Ok(WaitStatus::Signaled(_, sig, _)) => break format!("signal {}\n", sig as i32),
                    Ok(_) => continue,
                    Err(nix::errno::Errno::EINTR) => continue,
                    Err(_) => break "exit -1\n".into(),
                }
            };
            append(&record, &status);
            if let Some(s) = socket {
                let _ = std::fs::remove_file(s);
            }
            // A close was asked for: whatever the program left in its group
            // gets the same deadline.
            while CLOSING.load(Ordering::SeqCst) && !KILLED.load(Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("arugula: fork failed: {e}\r");
            std::process::exit(126);
        }
    }
}

/// Where the daemon puts the PTY master for `--hold`.
#[cfg(unix)]
pub const HELD_FD: i32 = 3;

#[cfg(any(unix, test))]
fn parse(args: &[String]) -> Option<(String, Option<String>, Vec<String>)> {
    let mut it = args.iter();
    if it.next()? != "--record" {
        return None;
    }
    let record = it.next()?.clone();
    let mut hold = None;
    let mut next = it.next()?;
    if next == "--hold" {
        hold = Some(it.next()?.clone());
        next = it.next()?;
    }
    if next != "--" {
        return None;
    }
    let argv: Vec<String> = it.cloned().collect();
    (!argv.is_empty()).then_some((record, hold, argv))
}

#[cfg(unix)]
fn append(path: &str, line: &str) -> bool {
    let Ok(mut f) = OpenOptions::new().create(true).append(true).mode(0o600).open(path) else { return false };
    let written = f.write_all(line.as_bytes()).is_ok();
    let _ = f.sync_data();
    written
}

/// A process's start time. With the pid it identifies a process even if the
/// pid is later reused.
pub fn start_time(pid: u32) -> Option<u64> {
    crate::procinfo::start_time(pid)
}

/// What a record says about the program.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Record {
    pub pid: Option<(u32, u64)>,
    /// The shim itself, from shims that take [`CLOSE`] (on Windows, the
    /// pane's host).
    pub shim: Option<(u32, u64)>,
    pub exit: Option<Ended>,
    /// Windows: the pane host's pipes (`crate::host`).
    pub pipe: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    Code(i32),
    Signal(i32),
}

pub fn read_record(path: &std::path::Path) -> Record {
    let mut r = Record::default();
    let Ok(text) = std::fs::read_to_string(path) else { return r };
    for line in text.lines() {
        let w: Vec<&str> = line.split_whitespace().collect();
        match w.as_slice() {
            ["pid", pid, start] => {
                if let (Ok(p), Ok(s)) = (pid.parse(), start.parse()) {
                    r = Record { pid: Some((p, s)), ..Record::default() };
                }
            }
            ["shim", pid, start] => {
                if let (Ok(p), Ok(s)) = (pid.parse(), start.parse()) {
                    r.shim = Some((p, s));
                }
            }
            ["pipe", name] => r.pipe = Some((*name).to_owned()),
            ["exit", code] => r.exit = code.parse().ok().map(Ended::Code),
            ["signal", n] => r.exit = n.parse().ok().map(Ended::Signal),
            _ => {}
        }
    }
    r
}

#[cfg(unix)]
/// Ask the shim to close the program: it hangs the group up, and kills it if
/// it's still there a few seconds later. False if the shim is gone or too old
/// to be asked.
pub fn close(record: &Record) -> bool {
    match record.shim {
        #[cfg(unix)]
        Some((pid, start)) if record.exit.is_none() && start_time(pid) == Some(start) => {
            nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), CLOSE).is_ok()
        }
        _ => false,
    }
}

/// Whether the process the record names is still the one running under that
/// pid.
pub fn alive(record: &Record) -> bool {
    match record.pid {
        Some((pid, start)) if record.exit.is_none() => start_time(pid) == Some(start),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_args_and_records() {
        let a = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(parse(&a(&["--record", "/r", "--", "bash", "-l"])), Some(("/r".into(), None, a(&["bash", "-l"]))));
        assert_eq!(
            parse(&a(&["--record", "/r", "--hold", "/h", "--", "zsh"])),
            Some(("/r".into(), Some("/h".into()), a(&["zsh"])))
        );
        assert_eq!(parse(&a(&["--record", "/r", "--hold", "/h", "zsh"])), None);
        assert_eq!(parse(&a(&["--record", "/r", "--"])), None);
        assert_eq!(parse(&a(&["bash"])), None);

        let dir = std::env::temp_dir().join(format!("arugula-shim-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("process");
        std::fs::write(&path, "pid 1 5\nsignal 9\npid 42 7\n").unwrap();
        assert_eq!(
            read_record(&path),
            Record { pid: Some((42, 7)), ..Default::default() },
            "a new start replaces the old"
        );
        std::fs::write(&path, "pid 1 5\nshim 2 6\nsignal 9\npid 42 7\nshim 43 8\n").unwrap();
        assert_eq!(read_record(&path), Record { pid: Some((42, 7)), shim: Some((43, 8)), exit: None, pipe: None });
        std::fs::write(&path, "pid 42 7\nexit 3\n").unwrap();
        assert_eq!(read_record(&path).exit, Some(Ended::Code(3)));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn our_own_start_time_matches() {
        let me = std::process::id();
        let start = start_time(me).unwrap();
        assert!(alive(&Record { pid: Some((me, start)), ..Default::default() }));
        assert!(!alive(&Record { pid: Some((me, start + 1)), ..Default::default() }), "pid reuse guard");
        #[cfg(unix)]
        assert!(!close(&Record { shim: Some((me, start + 1)), ..Default::default() }), "pid reuse guard");
    }
}
