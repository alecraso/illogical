//! `illogicald _host --record FILE --pipe NAME --cols C --rows R [--grace S]
//! -- PROGRAM ARGS...` (Windows, M58, #221): the shim's part on Windows. A
//! pane's pseudoconsole can only be resized or closed by the process that
//! made it, and Windows has no fd passing, so each pane gets a small host
//! that owns its pseudoconsole and the program's job, outlives the daemon,
//! and serves the pane to whichever daemon comes next (S29).
//!
//! - **Two pipes** per pane, both this user's alone (the daemon's own
//!   DACL), checked by SID: `NAME-out` carries the program's output and its
//!   exit to the daemon; `NAME-in` carries input, resizes and close from
//!   it. Two handles, so the daemon's reader and writer never wait on each
//!   other (a synchronous handle does one thing at a time, S29).
//! - **Frames** both ways: kind (u8), length (u32 LE), payload. 0 data,
//!   1 resize (cols u16, rows u16), 2 close, 3 exit (code i32).
//! - **The record** (the shim's format, `crate::shim::read_record`): the
//!   program's pid and start, the host's as `shim`, the pipe's name, then
//!   how it exited.
//! - **The lease** is the daemon's connection to `NAME-in`: with none for
//!   `--grace` seconds (60), the pane is closed and the host goes, as a
//!   holder does on Unix.
//! - **Nothing is lost while no daemon is there**: output waits (four
//!   chunks, then the program blocks on its writes, as on a pty nobody
//!   reads), and a chunk that couldn't be sent goes first to the next one.

use std::{
    fs::File,
    io::{self, Read, Write},
    mem::ManuallyDrop,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    path::{Path, PathBuf},
    ptr,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use windows_sys::Win32::{
    Foundation::{ERROR_PIPE_CONNECTED, GetLastError, HANDLE, INVALID_HANDLE_VALUE},
    Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_INBOUND, PIPE_ACCESS_OUTBOUND},
    System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
        PIPE_TYPE_BYTE, PIPE_WAIT,
    },
};

pub const DATA: u8 = 0;
pub const RESIZE: u8 = 1;
pub const CLOSE: u8 = 2;
pub const EXIT: u8 = 3;

/// How long a pane waits for a daemon before it's closed.
const GRACE: Duration = Duration::from_secs(60);

pub fn write_frame(w: &mut impl Write, kind: u8, payload: &[u8]) -> io::Result<()> {
    let mut buf = Vec::with_capacity(5 + payload.len());
    buf.push(kind);
    buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    buf.extend_from_slice(payload);
    w.write_all(&buf)?;
    w.flush()
}

pub fn read_frame(r: &mut impl Read) -> io::Result<(u8, Vec<u8>)> {
    let mut head = [0u8; 5];
    r.read_exact(&mut head)?;
    let len = u32::from_le_bytes([head[1], head[2], head[3], head[4]]) as usize;
    if len > 16 << 20 {
        return Err(io::Error::other("frame too large"));
    }
    let mut p = vec![0u8; len];
    r.read_exact(&mut p)?;
    Ok((head[0], p))
}

/// The pipes' base name for the pane whose record is `record`: this user's,
/// and that pane's (a hash of the record's path).
pub fn pipe_name(record: &Path) -> String {
    let hash = record
        .as_os_str()
        .as_encoded_bytes()
        .iter()
        .fold(0xcbf29ce484222325u64, |h, b| (h ^ *b as u64).wrapping_mul(0x100000001b3));
    let user = std::env::var("USERNAME").unwrap_or_default().to_lowercase();
    let user: String = user.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
    // Frozen (#504): a restarted daemon finds running panes by it.
    format!(r"\\.\pipe\illogical-{user}-pane-{hash:016x}")
}

struct Opts {
    record: PathBuf,
    pipe: String,
    cols: u16,
    rows: u16,
    grace: Duration,
    argv: Vec<String>,
}

fn parse(args: &[String]) -> Option<Opts> {
    let mut o = Opts { record: PathBuf::new(), pipe: String::new(), cols: 80, rows: 24, grace: GRACE, argv: vec![] };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--record" => o.record = PathBuf::from(it.next()?),
            "--pipe" => o.pipe = it.next()?.clone(),
            "--cols" => o.cols = it.next()?.parse().ok()?,
            "--rows" => o.rows = it.next()?.parse().ok()?,
            "--grace" => o.grace = Duration::from_secs(it.next()?.parse().ok()?),
            "--" => {
                o.argv = it.cloned().collect();
                break;
            }
            _ => return None,
        }
    }
    (!o.argv.is_empty() && !o.pipe.is_empty() && !o.record.as_os_str().is_empty()).then_some(o)
}

pub fn run(args: &[String]) -> ! {
    let code = match host(args) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("illogicald _host: {e}");
            1
        }
    };
    std::process::exit(code)
}

fn append(record: &Path, line: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(record) {
        let _ = f.write_all(line.as_bytes());
        let _ = f.sync_data();
    }
}

/// One instance of a pipe this user alone may open; fails if the name is
/// taken (another host, or someone squatting it).
fn server(name: &str, access: u32, sa: &mut crate::pipe::Sa) -> io::Result<OwnedHandle> {
    let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    // SAFETY: a valid name and security attributes.
    let h = unsafe {
        CreateNamedPipeW(
            wide.as_ptr(),
            access | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            64 * 1024,
            64 * 1024,
            0,
            &sa.0,
        )
    };
    if h == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a handle we just made.
    Ok(unsafe { OwnedHandle::from_raw_handle(h) })
}

/// Wait for a client on `h`, and check it's this user.
fn accept(h: &OwnedHandle) -> io::Result<()> {
    loop {
        // SAFETY: our pipe instance, synchronous.
        let ok = unsafe { ConnectNamedPipe(h.as_raw_handle() as HANDLE, ptr::null_mut()) };
        if ok == 0 && unsafe { GetLastError() } != ERROR_PIPE_CONNECTED {
            return Err(io::Error::last_os_error());
        }
        if crate::pipe::same_user(h.as_raw_handle() as HANDLE).unwrap_or(false) {
            return Ok(());
        }
        tracing::warn!("pane host: refused a client that isn't this user");
        drop_client(h);
    }
}

fn drop_client(h: &OwnedHandle) {
    // SAFETY: our pipe instance.
    unsafe { DisconnectNamedPipe(h.as_raw_handle() as HANDLE) };
}

/// The pipe instance as a file, without giving the handle away.
fn file(h: &OwnedHandle) -> ManuallyDrop<File> {
    // SAFETY: the File never closes it (ManuallyDrop); `h` outlives it.
    ManuallyDrop::new(unsafe { File::from_raw_handle(h.as_raw_handle()) })
}

enum Out {
    Data(Vec<u8>),
    Exit(i32),
}

fn host(args: &[String]) -> io::Result<()> {
    let o = parse(args).ok_or_else(|| io::Error::other("usage: _host --record FILE --pipe NAME -- PROGRAM ARGS"))?;
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut sa = crate::pipe::Sa::mine()?;
    // The pipes first: a second host for this pane fails here, before
    // starting anything.
    let out = server(&format!("{}-out", o.pipe), PIPE_ACCESS_OUTBOUND, &mut sa)?;
    let inp = server(&format!("{}-in", o.pipe), PIPE_ACCESS_INBOUND, &mut sa)?;
    let s = crate::conpty::spawn(&crate::conpty::Command {
        program: &o.argv[0],
        args: &o.argv[1..],
        cwd: &cwd,
        env: &[],
        cols: o.cols,
        rows: o.rows,
    })?;
    let pty = Arc::new(s.pty);
    let me = std::process::id();
    let start = |pid| crate::procinfo::start_time(pid).unwrap_or(0);
    let _ = std::fs::remove_file(&o.record);
    append(&o.record, &format!("pid {} {}\nshim {me} {}\npipe {}\n", pty.pid, start(pty.pid), start(me), o.pipe));

    // The program's output, four chunks ahead at most.
    let (out_tx, out_rx) = mpsc::sync_channel::<Out>(4);
    let mut output = s.output;
    let data_tx = out_tx.clone();
    let reader = std::thread::spawn(move || {
        let mut buf = vec![0u8; 64 * 1024];
        while let Ok(n) = output.read(&mut buf) {
            if n == 0 || data_tx.send(Out::Data(buf[..n].to_vec())).is_err() {
                break;
            }
        }
    });
    {
        let pty = pty.clone();
        let record = o.record.clone();
        std::thread::spawn(move || {
            let code = pty.wait();
            append(&record, &format!("exit {code}\n"));
            // Its last output, then the exit.
            pty.close_console();
            let _ = reader.join();
            let _ = out_tx.send(Out::Exit(code));
        });
    }

    // The lease: a daemon connected to `-in`. `epoch` counts connections,
    // so the output side notices one ended while it was idle.
    let leased = Arc::new(AtomicBool::new(false));
    let epoch = Arc::new(AtomicU64::new(0));
    let lost_at = Arc::new(std::sync::Mutex::new(Instant::now()));
    {
        let (pty, leased, epoch, lost_at) = (pty.clone(), leased.clone(), epoch.clone(), lost_at.clone());
        let mut input = s.input;
        std::thread::spawn(move || {
            loop {
                if accept(&inp).is_err() {
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
                epoch.fetch_add(1, Ordering::SeqCst);
                leased.store(true, Ordering::SeqCst);
                let mut r = file(&inp);
                while let Ok((kind, p)) = read_frame(&mut *r) {
                    match kind {
                        DATA => {
                            if input.write_all(&p).is_err() {
                                break;
                            }
                        }
                        RESIZE if p.len() == 4 => {
                            pty.resize(u16::from_le_bytes([p[0], p[1]]), u16::from_le_bytes([p[2], p[3]]))
                        }
                        CLOSE => pty.hang_up(),
                        _ => {}
                    }
                }
                *lost_at.lock().unwrap() = Instant::now();
                leased.store(false, Ordering::SeqCst);
                drop_client(&inp);
            }
        });
    }
    // No daemon for the grace: close the pane, and go.
    {
        let (pty, leased, lost_at, grace) = (pty.clone(), leased.clone(), lost_at.clone(), o.grace);
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(1));
                if !leased.load(Ordering::SeqCst) && lost_at.lock().unwrap().elapsed() > grace {
                    pty.hang_up();
                    std::thread::sleep(crate::conpty::KILL_AFTER + Duration::from_secs(1));
                    std::process::exit(0);
                }
            }
        });
    }

    let mut pending: Option<Out> = None;
    loop {
        if accept(&out).is_err() {
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }
        // Until the daemon goes: its `-in` connection ends (or a write
        // here fails).
        let mine = loop {
            let e = epoch.load(Ordering::SeqCst);
            if leased.load(Ordering::SeqCst) {
                break e;
            }
            std::thread::sleep(Duration::from_millis(20));
            if !leased.load(Ordering::SeqCst) && lost_at.lock().unwrap().elapsed() > Duration::from_secs(5) {
                // It connected one pipe and never the other.
                break u64::MAX;
            }
        };
        let mut w = file(&out);
        loop {
            if mine == u64::MAX || epoch.load(Ordering::SeqCst) != mine || !leased.load(Ordering::SeqCst) {
                break;
            }
            let item = match pending.take() {
                Some(i) => i,
                None => match out_rx.recv_timeout(Duration::from_millis(200)) {
                    Ok(i) => i,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
                },
            };
            let sent = match &item {
                Out::Data(d) => write_frame(&mut *w, DATA, d),
                Out::Exit(code) => write_frame(&mut *w, EXIT, &code.to_le_bytes()),
            };
            match (sent, &item) {
                (Ok(()), Out::Exit(_)) => {
                    // Delivered: the pane is over.
                    std::thread::sleep(Duration::from_millis(200));
                    return Ok(());
                }
                (Ok(()), Out::Data(_)) => {}
                (Err(_), _) => {
                    pending = Some(item);
                    break;
                }
            }
        }
        drop_client(&out);
    }
}

/// What pane hosts run: a copy of this exe (with the ConPTY beside it), one
/// per build, under the state directory. Hosts outlive the daemon, and a
/// running exe can't be replaced on Windows: run from here, they never hold
/// the daemon's own exe, so it can be upgraded (or rebuilt) under them.
/// Copies no host runs any more are removed (a running one can't be).
pub fn exe(state_dir: &Path) -> PathBuf {
    let Ok(me) = std::env::current_exe() else { return PathBuf::from("illogicald.exe") };
    let meta = std::fs::metadata(&me);
    let stamp = meta
        .as_ref()
        .ok()
        .and_then(|m| Some((m.len(), m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos())));
    let Some((len, mtime)) = stamp else { return me };
    let root = state_dir.join("hosts");
    let dir = root.join(format!("{len:x}-{mtime:x}"));
    let copy = dir.join("illogicald.exe");
    if !copy.is_file() {
        let made = std::fs::create_dir_all(&dir).and_then(|_| {
            let tmp = dir.join("illogicald.exe.tmp");
            std::fs::copy(&me, &tmp)?;
            for extra in ["conpty.dll", "OpenConsole.exe"] {
                let from = me.with_file_name(extra);
                if from.is_file() {
                    std::fs::copy(&from, dir.join(extra))?;
                }
            }
            std::fs::rename(&tmp, &copy)
        });
        if let Err(e) = made {
            tracing::warn!(error = %e, "can't copy the pane host; panes hold this exe");
            return me;
        }
    }
    if let Ok(dirs) = std::fs::read_dir(&root) {
        for d in dirs.flatten() {
            if d.path() != dir {
                let _ = std::fs::remove_dir_all(d.path());
            }
        }
    }
    copy
}

/// The panes whose hosts outlived the last daemon (from their records), by
/// the name the restore looks them up by (`pane-N`).
pub fn collect(state_dir: &Path) -> std::collections::HashMap<String, crate::pane::Kept> {
    let mut kept = std::collections::HashMap::new();
    let Ok(dirs) = std::fs::read_dir(state_dir.join("blocks")) else { return kept };
    for d in dirs.flatten() {
        let Some(id) = d.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else { continue };
        let record = d.path().join("process");
        let r = crate::shim::read_record(&record);
        let host_alive = r.shim.is_some_and(|(pid, start)| crate::procinfo::start_time(pid) == Some(start));
        if let Some(pipe) = r.pipe.filter(|_| host_alive) {
            kept.insert(format!("pane-{id}"), crate::pane::Kept { pipe });
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        let mut buf = Vec::new();
        write_frame(&mut buf, RESIZE, &[1, 0, 2, 0]).unwrap();
        write_frame(&mut buf, DATA, b"hi").unwrap();
        let mut r = &buf[..];
        assert_eq!(read_frame(&mut r).unwrap(), (RESIZE, vec![1, 0, 2, 0]));
        assert_eq!(read_frame(&mut r).unwrap(), (DATA, b"hi".to_vec()));
    }

    #[test]
    fn args() {
        let a = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let o = parse(&a(&["--record", "r", "--pipe", "p", "--cols", "100", "--", "pwsh", "-NoLogo"])).unwrap();
        assert_eq!((o.cols, o.rows, o.argv.len()), (100, 24, 2));
        assert!(parse(&a(&["--record", "r", "--", "pwsh"])).is_none());
    }
}
