//! Keeping pane terminals open while the daemon restarts, without systemd.
//!
//! On Linux under systemd, a pane's PTY master waits in the FD store while
//! the daemon is gone (M2b). Elsewhere (macOS, a box without systemd) the
//! pane's shim keeps it instead: it holds a copy of the master and lends it
//! to whichever daemon connects to its socket, over `SCM_RIGHTS`.
//!
//! A daemon keeps its connection to each shim open: that's its lease. When
//! the last lease ends (the daemon stopped or crashed) the shim waits
//! [`GRACE`] for a new daemon; if none comes, it hangs the terminal up and
//! the pane's programs end, as they would have without it. So a restart or
//! an upgrade keeps panes, and stopping the daemon still ends them.

use std::{
    collections::HashMap,
    io::{IoSlice, IoSliceMut, Read},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
        unix::{
            fs::PermissionsExt,
            net::{UnixListener, UnixStream},
        },
    },
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
    time::Duration,
};

use illogical_proto::PaneId;
use nix::{
    libc,
    sys::socket::{ControlMessage, ControlMessageOwned, MsgFlags, recvmsg, sendmsg},
};
use tracing::{info, warn};

/// How long a shim keeps a pane with no daemon before hanging it up.
/// launchd waits up to 10 s before restarting a job that exited.
pub const GRACE: Duration = Duration::from_secs(60);

/// [`GRACE`], or `ILLOGICAL_KEEP_GRACE_MS` (for tests).
fn grace() -> Duration {
    std::env::var("ILLOGICAL_KEEP_GRACE_MS")
        .ok()
        .and_then(|ms| ms.parse().ok())
        .map(Duration::from_millis)
        .unwrap_or(GRACE)
}

/// The shim's socket for a pane: in its block directory, unless that path
/// is too long for a Unix socket; then one in the temp dir named by a hash
/// of the directory (callers are checked by uid either way).
pub fn socket_for(pane_dir: &Path) -> PathBuf {
    let plain = pane_dir.join("hold");
    if plain.as_os_str().len() < 100 {
        return plain;
    }
    // FNV-1a: stable across runs, unlike std's hasher.
    let hash = pane_dir
        .as_os_str()
        .as_encoded_bytes()
        .iter()
        .fold(0xcbf29ce484222325u64, |h, b| (h ^ *b as u64).wrapping_mul(0x100000001b3));
    std::env::temp_dir().join(format!("illogical-hold-{hash:016x}.sock"))
}

// ---------------------------------------------------------------- daemon side

/// Leases this daemon holds, by pane. Dropping one tells its shim we're gone.
static LEASES: Mutex<Option<HashMap<PaneId, UnixStream>>> = Mutex::new(None);

fn keep_lease(pane: PaneId, stream: UnixStream) {
    LEASES.lock().unwrap().get_or_insert_with(HashMap::new).insert(pane, stream);
}

/// Connect to a pane's shim: take a copy of its terminal and keep the lease.
pub fn borrow(pane: PaneId, socket: &Path) -> std::io::Result<OwnedFd> {
    let stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut byte = [0u8; 1];
    let mut iov = [IoSliceMut::new(&mut byte)];
    let mut space = nix::cmsg_space!([RawFd; 1]);
    let msg = recvmsg::<()>(stream.as_raw_fd(), &mut iov, Some(&mut space), MsgFlags::empty())?;
    let fd = msg
        .cmsgs()?
        .find_map(|c| match c {
            ControlMessageOwned::ScmRights(fds) => fds.first().copied(),
            _ => None,
        })
        .ok_or_else(|| std::io::Error::other("the shim sent no terminal"))?;
    // SAFETY: SCM_RIGHTS gave us this descriptor; we own it now.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    nix::fcntl::fcntl(&fd, nix::fcntl::FcntlArg::F_SETFD(nix::fcntl::FdFlag::FD_CLOEXEC))?;
    stream.set_read_timeout(None)?;
    keep_lease(pane, stream);
    Ok(fd)
}

/// On start: the terminals shims kept for panes in `blocks`, by the FD
/// store's names (`pane-N`), so restore adopts them the same way.
pub fn collect(state_dir: &Path) -> HashMap<String, OwnedFd> {
    let mut out = HashMap::new();
    let Ok(entries) = std::fs::read_dir(state_dir.join("blocks")) else { return out };
    for e in entries.flatten() {
        let Some(pane) = e.file_name().to_str().and_then(|n| n.parse::<PaneId>().ok()) else { continue };
        let socket = socket_for(&e.path());
        if !socket.exists() {
            continue;
        }
        match borrow(pane, &socket) {
            Ok(fd) => {
                out.insert(format!("pane-{pane}"), fd);
            }
            Err(e) => info!(pane, error = %e, "no terminal kept for this pane"),
        }
    }
    info!(panes = out.len(), "terminals kept by pane shims");
    out
}

// ------------------------------------------------------------------ shim side

/// What the shim does before forking the program: claim the master the
/// daemon passed as `fd`, leave the daemon's process group (launchd kills
/// a job's whole group when it stops), and listen.
pub struct Holder {
    master: OwnedFd,
    listener: UnixListener,
    socket: PathBuf,
}

impl Holder {
    pub fn prepare(socket: &Path, fd: RawFd) -> std::io::Result<Self> {
        // SAFETY: the daemon passed us this descriptor and nothing else owns it.
        let master = unsafe { OwnedFd::from_raw_fd(fd) };
        // The program must not inherit it.
        nix::fcntl::fcntl(&master, nix::fcntl::FcntlArg::F_SETFD(nix::fcntl::FdFlag::FD_CLOEXEC))?;
        let _ = nix::unistd::setsid();
        let _ = std::fs::remove_file(socket);
        let listener = UnixListener::bind(socket)?;
        std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
        Ok(Self { master, listener, socket: socket.to_owned() })
    }

    /// After the fork: lend the terminal to daemons that connect, and hang
    /// it up if none holds a lease for [`GRACE`]. The daemon that started
    /// us connects right away.
    pub fn serve(self, child: libc::pid_t) {
        let leases = Arc::new((Mutex::new(Leases { count: 0, epoch: 0 }), Condvar::new()));
        let master = Arc::new(self.master);
        // Watchdog: a lease-less spell longer than GRACE ends the pane.
        {
            let leases = leases.clone();
            let master = master.clone();
            std::thread::spawn(move || watchdog(&leases, &master, child));
        }
        for conn in self.listener.incoming() {
            let Ok(conn) = conn else { continue };
            if !same_user(&conn) {
                continue;
            }
            let fds = [master.as_raw_fd()];
            let sent = sendmsg::<()>(
                conn.as_raw_fd(),
                &[IoSlice::new(b"t")],
                &[ControlMessage::ScmRights(&fds)],
                MsgFlags::empty(),
                None,
            );
            if sent.is_err() {
                continue;
            }
            {
                let (lock, cv) = &*leases;
                let mut l = lock.lock().unwrap();
                l.count += 1;
                l.epoch += 1;
                cv.notify_all();
            }
            // The lease lasts as long as the daemon keeps the connection.
            let leases = leases.clone();
            std::thread::spawn(move || {
                let mut conn = conn;
                let mut buf = [0u8; 64];
                while matches!(conn.read(&mut buf), Ok(n) if n > 0) {}
                let (lock, cv) = &*leases;
                let mut l = lock.lock().unwrap();
                l.count -= 1;
                l.epoch += 1;
                cv.notify_all();
            });
        }
    }

    /// The socket's path, removed when the program has ended.
    pub fn socket(&self) -> &Path {
        &self.socket
    }
}

struct Leases {
    count: u32,
    /// Bumped on every change, so a wait can tell it was interrupted.
    epoch: u64,
}

fn watchdog(leases: &(Mutex<Leases>, Condvar), master: &OwnedFd, child: libc::pid_t) {
    let (lock, cv) = leases;
    let grace = grace();
    // The starting daemon connects within moments; give it the same grace.
    let mut l = lock.lock().unwrap();
    loop {
        if l.count > 0 {
            l = cv.wait(l).unwrap();
            continue;
        }
        let epoch = l.epoch;
        let (next, timeout) = cv.wait_timeout_while(l, grace, |l| l.epoch == epoch).unwrap();
        l = next;
        if timeout.timed_out() && l.count == 0 {
            warn!(pid = child, "no daemon came back; hanging up the pane");
            // Read and drop what's left on the terminal: on macOS a process
            // leaving a terminal waits for its output to drain, and nobody
            // else reads it now. Ends when the last program has let go.
            let fd = master.as_raw_fd();
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                // SAFETY: reading into our own buffer from a descriptor the
                // shim keeps open until it exits.
                while unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) } > 0 {}
            });
            // As a terminal hangup would: SIGHUP, then SIGKILL for what
            // ignores it. The shim records the exit and goes.
            // SAFETY: plain signal sends to the program's process group.
            unsafe { libc::killpg(child, libc::SIGHUP) };
            std::thread::sleep(Duration::from_secs(3));
            unsafe { libc::killpg(child, libc::SIGKILL) };
            return;
        }
    }
}

/// Only our own user may borrow a terminal (the socket is 0600 anyway, but
/// it may be in a shared temp dir).
fn same_user(conn: &UnixStream) -> bool {
    let me = nix::unistd::getuid().as_raw();
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        use nix::sys::socket::{getsockopt, sockopt::PeerCredentials};
        getsockopt(conn, PeerCredentials).is_ok_and(|c| c.uid() == me)
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        let (mut uid, mut gid) = (0, 0);
        // SAFETY: getpeereid writes two ids.
        unsafe { libc::getpeereid(conn.as_raw_fd(), &mut uid, &mut gid) == 0 && uid == me }
    }
}
