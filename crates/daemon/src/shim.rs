//! `illogicald _shim --record FILE -- PROGRAM ARGS...`
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
//! exit <code>                or
//! signal <n>                 when it ends
//! ```
//!
//! It must run before any threads exist (it forks), so `main` dispatches to
//! it before starting the async runtime.

use std::{ffi::CString, fs::OpenOptions, io::Write, os::unix::fs::OpenOptionsExt};

use nix::{
    libc,
    sys::wait::{WaitStatus, waitpid},
    unistd::{ForkResult, execvp, fork, setsid},
};

pub fn run(args: &[String]) -> ! {
    let (record, argv) = match parse(args) {
        Some(x) => x,
        None => {
            eprintln!("usage: illogicald _shim --record FILE -- PROGRAM [ARGS...]");
            std::process::exit(2);
        }
    };
    let cargs: Vec<CString> = argv.iter().map(|a| CString::new(a.as_str()).unwrap_or_default()).collect();

    // SAFETY: single-threaded here (no runtime yet), so fork is sound.
    match unsafe { fork() } {
        Ok(ForkResult::Child) => {
            // A new session with the PTY as its controlling terminal, so the
            // shell does job control as if the daemon had started it.
            let _ = setsid();
            // SAFETY: TIOCSCTTY on our stdin, the PTY slave.
            unsafe { libc::ioctl(0, libc::TIOCSCTTY, 0) };
            let err = execvp(&cargs[0], &cargs).unwrap_err();
            let _ = writeln!(std::io::stderr(), "illogical: can't run {}: {err}\r", argv[0]);
            std::process::exit(127);
        }
        Ok(ForkResult::Parent { child }) => {
            // Let go of the PTY: only the program should hold it, so the
            // terminal hangs up when it (and its children) are gone.
            if let Ok(null) = OpenOptions::new().read(true).write(true).open("/dev/null") {
                use std::os::fd::AsRawFd;
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
            let pid = child.as_raw() as u32;
            append(&record, &format!("pid {pid} {}\n", start_time(pid).unwrap_or(0)));
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
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("illogical: fork failed: {e}\r");
            std::process::exit(126);
        }
    }
}

fn parse(args: &[String]) -> Option<(String, Vec<String>)> {
    let mut it = args.iter();
    if it.next()? != "--record" {
        return None;
    }
    let record = it.next()?.clone();
    if it.next()? != "--" {
        return None;
    }
    let argv: Vec<String> = it.cloned().collect();
    (!argv.is_empty()).then_some((record, argv))
}

fn append(path: &str, line: &str) {
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).mode(0o600).open(path) {
        let _ = f.write_all(line.as_bytes());
        let _ = f.sync_data();
    }
}

/// A process's start time (clock ticks since boot), field 22 of
/// /proc/PID/stat. With the pid it identifies a process even if the pid is
/// later reused.
pub fn start_time(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // Fields after the command, which is in parentheses and may contain
    // spaces; starttime is the 20th of them.
    stat.rsplit_once(')')?.1.split_whitespace().nth(19)?.parse().ok()
}

/// What a record says about the program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Record {
    pub pid: Option<(u32, u64)>,
    pub exit: Option<Ended>,
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
                    r = Record { pid: Some((p, s)), exit: None };
                }
            }
            ["exit", code] => r.exit = code.parse().ok().map(Ended::Code),
            ["signal", n] => r.exit = n.parse().ok().map(Ended::Signal),
            _ => {}
        }
    }
    r
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
        assert_eq!(parse(&a(&["--record", "/r", "--", "bash", "-l"])), Some(("/r".into(), a(&["bash", "-l"]))));
        assert_eq!(parse(&a(&["--record", "/r", "--"])), None);
        assert_eq!(parse(&a(&["bash"])), None);

        let dir = std::env::temp_dir().join(format!("illogical-shim-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("process");
        std::fs::write(&path, "pid 1 5\nsignal 9\npid 42 7\n").unwrap();
        assert_eq!(read_record(&path), Record { pid: Some((42, 7)), exit: None }, "a new start replaces the old");
        std::fs::write(&path, "pid 42 7\nexit 3\n").unwrap();
        assert_eq!(read_record(&path).exit, Some(Ended::Code(3)));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn our_own_start_time_matches() {
        let me = std::process::id();
        let start = start_time(me).unwrap();
        assert!(alive(&Record { pid: Some((me, start)), exit: None }));
        assert!(!alive(&Record { pid: Some((me, start + 1)), exit: None }), "pid reuse guard");
    }
}
