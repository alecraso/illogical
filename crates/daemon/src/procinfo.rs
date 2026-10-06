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

/// One variable of a process's environment as it started (#379: a pane's
/// `CLAUDE_CONFIG_DIR`). Linux only (/proc, the user's own processes):
/// macOS no longer shows another process's environment, and Windows isn't
/// asked.
pub fn env_var(pid: u32, name: &str) -> Option<String> {
    #[cfg(unix)]
    {
        let want = format!("{name}=");
        imp::environ(pid)?
            .iter()
            .rev()
            .find_map(|kv| kv.strip_prefix(want.as_bytes()).map(|v| String::from_utf8_lossy(v).into_owned()))
    }
    #[cfg(not(unix))]
    {
        let _ = (pid, name);
        None
    }
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

#[cfg_attr(windows, allow(dead_code))] // Windows waits on the process handle (conpty).
/// Block until a process (not necessarily our child) has ended. Returns at
/// once if it's already gone; `false` if it can't be watched.
pub fn wait_gone(pid: u32) -> bool {
    imp::wait_gone(pid)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
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

    pub fn environ(pid: u32) -> Option<Vec<Vec<u8>>> {
        let raw = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
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

    /// macOS 26's `KERN_PROCARGS2` has no environment for another process,
    /// the user's own included (`ps -E` shows none either).
    pub fn environ(_pid: u32) -> Option<Vec<Vec<u8>>> {
        None
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

/// Windows: the process API, and for argv and the working directory the
/// process's own parameters (`NtQueryInformationProcess`, and its PEB read
/// with `ReadProcessMemory`), as Process Explorer reads them. There are no
/// process groups: a pane's "foreground" is the newest program its shell
/// started, followed down through shells it started in turn (`cmd /c` shims,
/// a nested pwsh), as a nested shell's job would be on Unix.
#[cfg(windows)]
mod imp {
    use std::{
        collections::HashMap,
        ffi::{OsString, c_void},
        fs::{File, Metadata},
        mem::size_of,
        os::windows::ffi::OsStringExt,
        path::{Path, PathBuf},
        ptr,
    };

    use windows_sys::{
        Wdk::System::Threading::{NtQueryInformationProcess, ProcessBasicInformation, ProcessCommandLineInformation},
        Win32::{
            Foundation::{CloseHandle, FILETIME, HANDLE, INVALID_HANDLE_VALUE, LocalFree, WAIT_OBJECT_0},
            System::{
                Diagnostics::{
                    Debug::ReadProcessMemory,
                    ToolHelp::{
                        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
                    },
                },
                Threading::{
                    GetProcessTimes, INFINITE, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_INFORMATION,
                    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, PROCESS_VM_READ,
                    QueryFullProcessImageNameW, TerminateProcess, WaitForSingleObject,
                },
            },
            UI::Shell::CommandLineToArgvW,
        },
    };

    struct Handle(HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: a handle we opened.
            unsafe { CloseHandle(self.0) };
        }
    }

    fn open(pid: u32, access: u32) -> Option<Handle> {
        // SAFETY: a plain call; a null handle means no such process (or no
        // access to it).
        let h = unsafe { OpenProcess(access, 0, pid) };
        (!h.is_null()).then_some(Handle(h))
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

    /// `PROCESS_BASIC_INFORMATION`, without the PEB's type.
    #[repr(C)]
    struct BasicInfo {
        exit_status: i32,
        peb: usize,
        affinity: usize,
        priority: i32,
        pid: usize,
        parent: usize,
    }

    fn basic_info(p: &Handle) -> Option<BasicInfo> {
        let mut info = BasicInfo { exit_status: 0, peb: 0, affinity: 0, priority: 0, pid: 0, parent: 0 };
        // SAFETY: a buffer of the size we say.
        let status = unsafe {
            NtQueryInformationProcess(
                p.0,
                ProcessBasicInformation,
                (&raw mut info).cast(),
                size_of::<BasicInfo>() as u32,
                ptr::null_mut(),
            )
        };
        (status >= 0).then_some(info)
    }

    pub fn ppid(pid: u32) -> Option<u32> {
        let p = open(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
        u32::try_from(basic_info(&p)?.parent).ok()
    }

    /// The command line as the process was given it, split as its C
    /// runtime would.
    pub fn argv(pid: u32) -> Option<Vec<Vec<u8>>> {
        let p = open(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
        // A UNICODE_STRING, then the text it points to, in one buffer.
        let mut len = 0u32;
        // SAFETY: asking the size only.
        unsafe { NtQueryInformationProcess(p.0, ProcessCommandLineInformation, ptr::null_mut(), 0, &mut len) };
        if len == 0 {
            return None;
        }
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        // SAFETY: a buffer of at least `len` bytes, aligned for the struct.
        let status = unsafe {
            NtQueryInformationProcess(p.0, ProcessCommandLineInformation, buf.as_mut_ptr().cast(), len, &mut len)
        };
        if status < 0 {
            return None;
        }
        // SAFETY: the call wrote a UNICODE_STRING at the start, whose buffer
        // lies within ours.
        let line: Vec<u16> = unsafe {
            let us = &*(buf.as_ptr() as *const UnicodeString);
            std::slice::from_raw_parts(us.buffer as *const u16, us.length as usize / 2).to_vec()
        };
        Some(split(&line))
    }

    #[repr(C)]
    struct UnicodeString {
        length: u16,
        max: u16,
        buffer: usize,
    }

    fn split(line: &[u16]) -> Vec<Vec<u8>> {
        if line.is_empty() {
            return vec![];
        }
        let z: Vec<u16> = line.iter().copied().chain([0]).collect();
        let mut n = 0i32;
        // SAFETY: a NUL-terminated string; the array it returns is freed
        // below.
        let words = unsafe { CommandLineToArgvW(z.as_ptr(), &mut n) };
        if words.is_null() {
            return vec![];
        }
        let out = (0..n as usize)
            .map(|i| {
                // SAFETY: `n` NUL-terminated strings.
                let w = unsafe { *words.add(i) };
                let len = (0..).take_while(|j| unsafe { *w.add(*j) } != 0).count();
                let s = unsafe { std::slice::from_raw_parts(w, len) };
                String::from_utf16_lossy(s).into_bytes()
            })
            .collect();
        // SAFETY: CommandLineToArgvW's allocation.
        unsafe { LocalFree(words.cast()) };
        out
    }

    fn read<T: Copy>(p: &Handle, at: usize) -> Option<T> {
        let mut v = std::mem::MaybeUninit::<T>::uninit();
        let mut got = 0usize;
        // SAFETY: room for one T; only used if all of it was read.
        let ok =
            unsafe { ReadProcessMemory(p.0, at as *const c_void, v.as_mut_ptr().cast(), size_of::<T>(), &mut got) };
        (ok != 0 && got == size_of::<T>()).then(|| unsafe { v.assume_init() })
    }

    /// The working directory: `ProcessParameters->CurrentDirectory` in its
    /// PEB (64-bit layout; a 32-bit process's own copy isn't read).
    #[cfg(target_pointer_width = "64")]
    pub fn cwd(pid: u32) -> Option<PathBuf> {
        let p = open(pid, PROCESS_QUERY_INFORMATION | PROCESS_VM_READ)?;
        let peb = basic_info(&p)?.peb;
        let params: usize = read(&p, peb.checked_add(0x20)?)?;
        let (length, _max, _pad): (u16, u16, u32) = read(&p, params.checked_add(0x38)?)?;
        let buffer: usize = read(&p, params.checked_add(0x40)?)?;
        if length == 0 {
            return None;
        }
        let mut text = vec![0u16; length as usize / 2];
        let mut got = 0usize;
        // SAFETY: room for `length` bytes.
        let ok = unsafe {
            ReadProcessMemory(p.0, buffer as *const c_void, text.as_mut_ptr().cast(), length as usize, &mut got)
        };
        if ok == 0 || got != length as usize {
            return None;
        }
        let mut dir = PathBuf::from(OsString::from_wide(&text));
        // `C:\dir\` as Windows keeps it; `C:\` stays as it is.
        let s = dir.to_string_lossy();
        if s.len() > 3 && s.ends_with('\\') {
            dir = PathBuf::from(s.trim_end_matches('\\').to_owned());
        }
        Some(dir)
    }

    #[cfg(not(target_pointer_width = "64"))]
    pub fn cwd(_pid: u32) -> Option<PathBuf> {
        None
    }

    pub fn exe(pid: u32) -> Option<PathBuf> {
        let p = open(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
        let mut buf = vec![0u16; 32_768];
        let mut n = buf.len() as u32;
        // SAFETY: a buffer of `n` characters.
        let ok = unsafe { QueryFullProcessImageNameW(p.0, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut n) };
        (ok != 0).then(|| PathBuf::from(OsString::from_wide(&buf[..n as usize])))
    }

    /// The program's name, less `.exe` (`pwsh`, `vim`, `node`).
    pub fn comm(pid: u32) -> Option<String> {
        let exe = exe(pid).or_else(|| processes().into_iter().find(|p| p.pid == pid).map(|p| PathBuf::from(p.name)))?;
        Some(exe.file_stem()?.to_string_lossy().into_owned())
    }

    struct Proc {
        pid: u32,
        parent: u32,
        name: String,
    }

    fn processes() -> Vec<Proc> {
        // SAFETY: a snapshot we close; entries filled by the API.
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snap == INVALID_HANDLE_VALUE {
                return vec![];
            }
            let snap = Handle(snap);
            let mut out = Vec::new();
            let mut e: PROCESSENTRY32W = std::mem::zeroed();
            e.dwSize = size_of::<PROCESSENTRY32W>() as u32;
            let mut ok = Process32FirstW(snap.0, &mut e);
            while ok != 0 {
                let len = e.szExeFile.iter().position(|c| *c == 0).unwrap_or(e.szExeFile.len());
                out.push(Proc {
                    pid: e.th32ProcessID,
                    parent: e.th32ParentProcessID,
                    name: String::from_utf16_lossy(&e.szExeFile[..len]),
                });
                ok = Process32NextW(snap.0, &mut e);
            }
            out
        }
    }

    pub fn foreground(shell: u32) -> Option<u32> {
        let all = processes();
        let mut children: HashMap<u32, Vec<&Proc>> = HashMap::new();
        for p in &all {
            children.entry(p.parent).or_default().push(p);
        }
        // A child that started after its parent (a parent's pid can be
        // reused), and isn't the console's own host.
        let born = |pid: u32| start_time(pid).unwrap_or(0);
        let newest_child = |of: u32| {
            let since = born(of);
            children
                .get(&of)?
                .iter()
                .filter(|c| !is_named(&c.name, &["conhost", "openconsole"]))
                .map(|c| (born(c.pid), c.pid))
                .filter(|(at, _)| *at >= since)
                .max()
                .map(|(_, pid)| pid)
        };
        let name = |pid: u32| all.iter().find(|p| p.pid == pid).map(|p| p.name.as_str()).unwrap_or("");
        let Some(mut fg) = newest_child(shell) else { return Some(shell) };
        for _ in 0..16 {
            if !is_named(name(fg), &["cmd", "pwsh", "powershell", "bash", "sh", "zsh", "fish", "nu", "wsl"]) {
                break;
            }
            match newest_child(fg) {
                Some(c) => fg = c,
                None => break,
            }
        }
        Some(fg)
    }

    fn is_named(exe: &str, names: &[&str]) -> bool {
        let lower = exe.to_lowercase();
        names.contains(&lower.strip_suffix(".exe").unwrap_or(&lower))
    }

    pub fn kill(pid: u32) {
        if let Some(p) = open(pid, PROCESS_TERMINATE) {
            // SAFETY: a handle we opened, with PROCESS_TERMINATE.
            unsafe { TerminateProcess(p.0, 1) };
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
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
    fn reads_our_own_process() {
        let me = std::process::id();
        assert_eq!(cwd(me), std::env::current_dir().ok());
        let args = argv(me).unwrap();
        assert!(!args.is_empty(), "{args:?}");
        assert_eq!(
            exe(me).and_then(|e| e.canonicalize().ok()),
            std::env::current_exe().ok().and_then(|e| e.canonicalize().ok())
        );
        assert!(!comm(me).unwrap().is_empty());
        assert!(ppid(me).is_some());
    }

    #[test]
    fn a_child_s_argv_cwd_and_its_shell_s_foreground() {
        let dir = std::env::temp_dir();
        let mut shell = std::process::Command::new("cmd")
            .args(["/d", "/c", "ping -n 30 127.0.0.1 >NUL"])
            .current_dir(&dir)
            .spawn()
            .unwrap();
        let pid = shell.id();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let fg = loop {
            let fg = foreground(pid).unwrap();
            if fg != pid || std::time::Instant::now() > deadline {
                break fg;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        assert_ne!(fg, pid);
        assert_eq!(comm(fg).unwrap().to_lowercase(), "ping");
        assert_eq!(argv(fg).unwrap().last().map(String::as_str), Some("127.0.0.1"));
        assert_eq!(ppid(fg), Some(pid));
        assert_eq!(cwd(pid).and_then(|d| d.canonicalize().ok()), dir.canonicalize().ok());
        assert_eq!(argv(pid).unwrap()[1..3], ["/d", "/c"]);
        let _ = shell.kill();
        let _ = shell.wait();
        kill(fg);
    }

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
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .env("ILLOGICAL_PROCINFO_TEST", "/a dir/x")
            .env_remove("ILLOGICAL_PROCINFO_ABSENT")
            .spawn()
            .unwrap();
        let pid = child.id();
        // Until it execs, the child is still a copy of us.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while argv(pid).unwrap() != ["sleep", "30"] && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(argv(pid).unwrap(), ["sleep", "30"]);
        assert_eq!(comm(pid).as_deref(), Some("sleep"));
        // #379: one variable of its environment, and none it hasn't.
        #[cfg(target_os = "linux")]
        assert_eq!(env_var(pid, "ILLOGICAL_PROCINFO_TEST").as_deref(), Some("/a dir/x"));
        assert_eq!(env_var(pid, "ILLOGICAL_PROCINFO_ABSENT"), None);
        assert_eq!(env_var(pid, "ILLOGICAL_PROCINFO"), None, "the whole name");
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
