//! What the daemon asks the OS about processes and open files: /proc on
//! Linux, libproc and sysctl on macOS, the process API on Windows.

#[cfg(unix)]
use std::os::fd::RawFd;
use std::{
    ffi::OsString,
    fs::{File, Metadata},
    path::{Path, PathBuf},
};

/// A process's start time, which with its pid identifies it even if the pid
/// is later reused. Linux: clock ticks since boot; macOS: microseconds since
/// the epoch. Only compared with itself.
pub fn start_time(pid: u32) -> Option<u64> {
    imp::start_time(pid)
}

/// A process's working directory.
pub fn cwd(pid: u32) -> Option<PathBuf> {
    imp::cwd(pid)
}

/// The foreground process group of the terminal a session leader controls.
pub fn foreground(pid: u32) -> Option<u32> {
    imp::foreground(pid)
}

/// A process's parent.
pub fn ppid(pid: u32) -> Option<u32> {
    imp::ppid(pid).filter(|p| *p > 0)
}

/// A process's command line.
pub fn argv(pid: u32) -> Option<Vec<String>> {
    imp::argv(pid).map(|raw| raw.iter().map(|a| String::from_utf8_lossy(a).into_owned()).collect())
}

/// A process's short name (`comm`).
pub fn comm(pid: u32) -> Option<String> {
    imp::comm(pid)
}

/// The executable a process runs.
pub fn exe(pid: u32) -> Option<PathBuf> {
    imp::exe(pid)
}

/// The path an open descriptor refers to now.
#[cfg(unix)]
pub fn fd_path(fd: RawFd) -> std::io::Result<PathBuf> {
    imp::fd_path(fd)
}

/// The entries of an open directory (`real` is the path it was opened by,
/// already checked against the descriptor), without `.` and `..`, each with
/// its own metadata (links not followed).
pub fn list_dir(dir: &File, real: &Path) -> std::io::Result<Vec<(OsString, Metadata)>> {
    imp::list_dir(dir, real)
}

/// Whether a process with this pid exists (someone else's included).
pub fn alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // EPERM is someone's process all the same; only ESRCH means it's gone.
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None) != Err(nix::errno::Errno::ESRCH)
    }
    #[cfg(not(unix))]
    start_time(pid).is_some()
}

/// End a process now (SIGKILL; TerminateProcess on Windows).
#[cfg(any(windows, test))]
pub fn kill(pid: u32) {
    #[cfg(unix)]
    let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), nix::sys::signal::SIGKILL);
    #[cfg(not(unix))]
    imp::kill(pid);
}

/// Block until a process (not necessarily our child) has ended. Returns at
/// once if it's already gone; `false` if it can't be watched.
pub fn wait_gone(pid: u32) -> bool {
    imp::wait_gone(pid)
}

#[cfg(target_os = "linux")]
mod imp {
    use std::{
        ffi::OsString,
        fs::{File, Metadata},
        os::fd::{AsRawFd, RawFd},
        path::{Path, PathBuf},
    };

    use nix::libc;

    pub fn list_dir(dir: &File, _real: &Path) -> std::io::Result<Vec<(OsString, Metadata)>> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(format!("/proc/self/fd/{}", dir.as_raw_fd()))?.flatten() {
            if let Ok(meta) = e.metadata() {
                out.push((e.file_name(), meta));
            }
        }
        Ok(out)
    }

    /// Fields of /proc/PID/stat after the command, which is in parentheses
    /// and may contain spaces.
    fn stat_field(pid: u32, n: usize) -> Option<String> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        stat.rsplit_once(')')?.1.split_whitespace().nth(n).map(str::to_owned)
    }

    pub fn start_time(pid: u32) -> Option<u64> {
        // starttime is field 22, the 20th after the command.
        stat_field(pid, 19)?.parse().ok()
    }

    pub fn cwd(pid: u32) -> Option<PathBuf> {
        std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
    }

    pub fn ppid(pid: u32) -> Option<u32> {
        // ppid is field 4.
        stat_field(pid, 1)?.parse().ok()
    }

    pub fn foreground(pid: u32) -> Option<u32> {
        // tpgid is field 8.
        let t: i32 = stat_field(pid, 5)?.parse().ok()?;
        (t > 0).then_some(t as u32)
    }

    pub fn argv(pid: u32) -> Option<Vec<Vec<u8>>> {
        let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        Some(raw.split(|b| *b == 0).filter(|a| !a.is_empty()).map(<[u8]>::to_vec).collect())
    }

    pub fn comm(pid: u32) -> Option<String> {
        Some(std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?.trim().to_owned())
    }

    pub fn exe(pid: u32) -> Option<PathBuf> {
        std::fs::read_link(format!("/proc/{pid}/exe")).ok()
    }

    pub fn fd_path(fd: RawFd) -> std::io::Result<PathBuf> {
        std::fs::read_link(format!("/proc/self/fd/{fd}"))
    }

    pub fn wait_gone(pid: u32) -> bool {
        // SAFETY: pidfd_open takes a pid and flags and returns a new fd.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) } as i32;
        if fd < 0 {
            return std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
        }
        let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
        // SAFETY: one valid pollfd; the fd is ours and closed below.
        while unsafe { libc::poll(&mut pfd, 1, -1) } < 0 {}
        unsafe { libc::close(fd) };
        true
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::{
        ffi::{CStr, OsStr, OsString},
        fs::{File, Metadata},
        mem::{MaybeUninit, size_of},
        os::{fd::RawFd, unix::ffi::OsStrExt},
        path::{Path, PathBuf},
    };

    use nix::libc;

    /// No /proc/self/fd here (and /dev/fd/N can't be listed): read the
    /// descriptor itself with fdopendir.
    pub fn list_dir(dir: &File, real: &Path) -> std::io::Result<Vec<(OsString, Metadata)>> {
        let fd = nix::unistd::dup(dir)?;
        let mut d = nix::dir::Dir::from_fd(fd)?;
        let mut out = Vec::new();
        for e in d.iter().flatten() {
            let name = OsStr::from_bytes(e.file_name().to_bytes());
            if name == "." || name == ".." {
                continue;
            }
            if let Ok(meta) = std::fs::symlink_metadata(real.join(name)) {
                out.push((name.to_owned(), meta));
            }
        }
        Ok(out)
    }

    fn pidinfo<T>(pid: u32, flavor: libc::c_int) -> Option<T> {
        let mut out = MaybeUninit::<T>::zeroed();
        let size = size_of::<T>() as libc::c_int;
        // SAFETY: the kernel writes at most `size` bytes into `out`.
        let n = unsafe { libc::proc_pidinfo(pid as libc::c_int, flavor, 0, out.as_mut_ptr().cast(), size) };
        // SAFETY: a full-size answer filled the struct (and it was zeroed).
        (n == size).then(|| unsafe { out.assume_init() })
    }

    fn bsdinfo(pid: u32) -> Option<libc::proc_bsdinfo> {
        pidinfo(pid, libc::PROC_PIDTBSDINFO)
    }

    fn c_path(bytes: &[u8]) -> Option<PathBuf> {
        let s = CStr::from_bytes_until_nul(bytes).ok()?.to_bytes();
        (!s.is_empty()).then(|| PathBuf::from(OsStr::from_bytes(s)))
    }

    pub fn start_time(pid: u32) -> Option<u64> {
        let b = bsdinfo(pid)?;
        Some(b.pbi_start_tvsec * 1_000_000 + b.pbi_start_tvusec)
    }

    pub fn cwd(pid: u32) -> Option<PathBuf> {
        let v: libc::proc_vnodepathinfo = pidinfo(pid, libc::PROC_PIDVNODEPATHINFO)?;
        let path = v.pvi_cdir.vip_path;
        // SAFETY: [[c_char; 32]; 32] is MAXPATHLEN contiguous bytes.
        let bytes: &[u8] = unsafe { std::slice::from_raw_parts(path.as_ptr().cast(), size_of_val(&path)) };
        c_path(bytes)
    }

    pub fn ppid(pid: u32) -> Option<u32> {
        Some(bsdinfo(pid)?.pbi_ppid)
    }

    pub fn foreground(pid: u32) -> Option<u32> {
        let t = bsdinfo(pid)?.e_tpgid;
        (t > 0).then_some(t)
    }

    /// `KERN_PROCARGS2`: argc, the executable path, padding, then argv.
    pub fn argv(pid: u32) -> Option<Vec<Vec<u8>>> {
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
        let mut max: libc::c_int = 0;
        let mut size = size_of::<libc::c_int>();
        let mut argmax = [libc::CTL_KERN, libc::KERN_ARGMAX];
        // SAFETY: reads one c_int.
        if unsafe { libc::sysctl(argmax.as_mut_ptr(), 2, (&raw mut max).cast(), &mut size, std::ptr::null_mut(), 0) }
            != 0
        {
            return None;
        }
        let mut buf = vec![0u8; max as usize];
        let mut len = buf.len();
        // SAFETY: the kernel writes at most `len` bytes and updates it.
        if unsafe { libc::sysctl(mib.as_mut_ptr(), 3, buf.as_mut_ptr().cast(), &mut len, std::ptr::null_mut(), 0) } != 0
        {
            return None;
        }
        buf.truncate(len);
        let argc = i32::from_ne_bytes(buf.get(..4)?.try_into().ok()?).max(0) as usize;
        let rest = &buf[4..];
        // Skip the executable path and the NULs after it.
        let start = rest.iter().position(|b| *b == 0)?;
        let start = start + rest[start..].iter().position(|b| *b != 0)?;
        let mut args = Vec::with_capacity(argc);
        for a in rest[start..].split(|b| *b == 0) {
            if args.len() == argc {
                break;
            }
            args.push(a.to_vec());
        }
        Some(args.into_iter().filter(|a| !a.is_empty()).collect())
    }

    pub fn comm(pid: u32) -> Option<String> {
        let b = bsdinfo(pid)?;
        // pbi_name is the longer one; pbi_comm is truncated to 16.
        let pick = if b.pbi_name[0] != 0 { &b.pbi_name[..] } else { &b.pbi_comm[..] };
        // SAFETY: c_char and u8 have the same layout.
        let bytes: &[u8] = unsafe { std::slice::from_raw_parts(pick.as_ptr().cast(), pick.len()) };
        Some(CStr::from_bytes_until_nul(bytes).ok()?.to_string_lossy().into_owned())
    }

    pub fn exe(pid: u32) -> Option<PathBuf> {
        let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        // SAFETY: the kernel writes at most the buffer's size.
        let n = unsafe { libc::proc_pidpath(pid as libc::c_int, buf.as_mut_ptr().cast(), buf.len() as u32) };
        (n > 0).then(|| PathBuf::from(OsStr::from_bytes(&buf[..n as usize])))
    }

    pub fn fd_path(fd: RawFd) -> std::io::Result<PathBuf> {
        let mut buf = vec![0u8; libc::PATH_MAX as usize];
        // SAFETY: F_GETPATH writes a NUL-terminated path of at most MAXPATHLEN.
        if unsafe { libc::fcntl(fd, libc::F_GETPATH, buf.as_mut_ptr()) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        c_path(&buf).ok_or_else(|| std::io::Error::other("F_GETPATH returned nothing"))
    }

    /// `SZOMB` in `<sys/proc.h>`: exited, not yet reaped.
    const SZOMB: u32 = 5;

    /// Running: it exists and isn't a zombie.
    fn running(pid: u32) -> bool {
        bsdinfo(pid).is_some_and(|b| b.pbi_status != SZOMB)
    }

    pub fn wait_gone(pid: u32) -> bool {
        // SAFETY: plain kqueue calls on a queue we own and close.
        unsafe {
            let kq = libc::kqueue();
            if kq < 0 {
                return false;
            }
            let mut ev: libc::kevent = std::mem::zeroed();
            ev.ident = pid as usize;
            ev.filter = libc::EVFILT_PROC;
            ev.flags = libc::EV_ADD | libc::EV_ONESHOT;
            ev.fflags = libc::NOTE_EXIT;
            // Registering fails with ESRCH if it's already gone.
            if libc::kevent(kq, &ev, 1, std::ptr::null_mut(), 0, std::ptr::null()) < 0 {
                libc::close(kq);
                return std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
            }
            // A watch placed on a zombie may never fire, so check again
            // every second as well.
            let second = libc::timespec { tv_sec: 1, tv_nsec: 0 };
            let mut out: libc::kevent = std::mem::zeroed();
            while running(pid) {
                if libc::kevent(kq, std::ptr::null(), 0, &mut out, 1, &second) > 0 {
                    break;
                }
            }
            libc::close(kq);
            true
        }
    }
}

/// Windows: start times and waiting are real; what a pane's process is
/// doing (cwd, argv, the foreground program) comes in M60 (#223).
#[cfg(windows)]
mod imp {
    use std::{
        ffi::OsString,
        fs::{File, Metadata},
        path::{Path, PathBuf},
    };

    use windows_sys::Win32::{
        Foundation::{CloseHandle, FILETIME, HANDLE, WAIT_OBJECT_0},
        System::Threading::{
            GetProcessTimes, INFINITE, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
            PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
        },
    };

    struct Process(HANDLE);
    impl Drop for Process {
        fn drop(&mut self) {
            // SAFETY: a handle we opened.
            unsafe { CloseHandle(self.0) };
        }
    }

    fn open(pid: u32, access: u32) -> Option<Process> {
        // SAFETY: a plain call; a null handle means no such process.
        let h = unsafe { OpenProcess(access, 0, pid) };
        (!h.is_null()).then_some(Process(h))
    }

    pub fn list_dir(_dir: &File, real: &Path) -> std::io::Result<Vec<(OsString, Metadata)>> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(real)?.flatten() {
            if let Ok(meta) = std::fs::symlink_metadata(e.path()) {
                out.push((e.file_name(), meta));
            }
        }
        Ok(out)
    }

    /// Its creation time, in 100 ns since 1601.
    pub fn start_time(pid: u32) -> Option<u64> {
        let p = open(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
        let z = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        let (mut created, mut exited, mut kernel, mut user) = (z, z, z, z);
        // SAFETY: four valid FILETIMEs for the call to fill.
        let ok = unsafe { GetProcessTimes(p.0, &mut created, &mut exited, &mut kernel, &mut user) };
        (ok != 0).then_some((created.dwHighDateTime as u64) << 32 | created.dwLowDateTime as u64)
    }

    pub fn cwd(_pid: u32) -> Option<PathBuf> {
        None
    }

    pub fn ppid(_pid: u32) -> Option<u32> {
        None
    }

    pub fn foreground(_pid: u32) -> Option<u32> {
        None
    }

    pub fn argv(_pid: u32) -> Option<Vec<Vec<u8>>> {
        None
    }

    pub fn comm(_pid: u32) -> Option<String> {
        None
    }

    pub fn exe(_pid: u32) -> Option<PathBuf> {
        None
    }

    pub fn kill(pid: u32) {
        if let Some(p) = open(pid, PROCESS_TERMINATE) {
            // SAFETY: a handle we opened, with PROCESS_TERMINATE.
            unsafe { TerminateProcess(p.0, 1) };
        }
    }

    pub fn wait_gone(pid: u32) -> bool {
        let Some(p) = open(pid, PROCESS_SYNCHRONIZE) else { return true };
        // SAFETY: a handle we opened, with SYNCHRONIZE.
        unsafe { WaitForSingleObject(p.0, INFINITE) == WAIT_OBJECT_0 }
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    #[test]
    fn start_time_and_waiting() {
        let me = std::process::id();
        assert!(start_time(me).is_some());
        assert_eq!(start_time(me), start_time(me));
        let mut child = std::process::Command::new("cmd").args(["/c", "exit 0"]).spawn().unwrap();
        let pid = child.id();
        assert!(wait_gone(pid));
        let _ = child.wait();
    }
}

// Unix: they read /proc or libproc, and spawn `sh`.
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn reads_our_own_process() {
        let me = std::process::id();
        assert!(start_time(me).is_some());
        assert_eq!(cwd(me), std::env::current_dir().ok());
        let args = argv(me).unwrap();
        assert!(!args.is_empty());
        assert!(!comm(me).unwrap().is_empty());
        assert_eq!(
            exe(me).and_then(|e| e.canonicalize().ok()),
            std::env::current_exe().ok().and_then(|e| e.canonicalize().ok())
        );
    }

    #[test]
    fn argv_of_a_child() {
        let mut child = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        let pid = child.id();
        // Until it execs, the child is still a copy of us.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while argv(pid).unwrap() != ["sleep", "30"] && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(argv(pid).unwrap(), ["sleep", "30"]);
        assert_eq!(comm(pid).as_deref(), Some("sleep"));
        child.kill().unwrap();
        child.wait().unwrap();
        // Reaped: gone, and waiting returns at once.
        assert!(wait_gone(pid));
        assert!(start_time(pid).is_none());
    }

    #[test]
    fn waits_for_a_process_that_is_not_our_child() {
        // `sh` forks `sleep` and prints its pid; we wait on the grandchild.
        let out =
            std::process::Command::new("sh").args(["-c", "sleep 0.3 >/dev/null 2>&1 & echo $!"]).output().unwrap();
        let pid: u32 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap();
        let t = std::time::Instant::now();
        assert!(wait_gone(pid));
        assert!(t.elapsed() >= std::time::Duration::from_millis(100));
    }

    #[test]
    fn names_an_open_file_and_lists_an_open_dir() {
        use std::os::fd::AsRawFd;
        let dir = std::env::temp_dir().canonicalize().unwrap().join(format!("procinfo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a"), b"x").unwrap();
        let f = std::fs::File::open(dir.join("a")).unwrap();
        assert_eq!(fd_path(f.as_raw_fd()).unwrap(), dir.join("a"));
        let d = std::fs::File::open(&dir).unwrap();
        let entries = list_dir(&d, &dir).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, "a");
        assert_eq!(entries[0].1.len(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
