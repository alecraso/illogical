//! Windows panes (M56, #219): a pseudoconsole and the program on it, in a
//! Job Object so closing the pane takes the program's whole tree.
//!
//! The ConPTY is Microsoft's current one when it ships beside this exe
//! (`conpty.dll`, which starts `OpenConsole.exe` from its own directory),
//! else the one Windows has. S29 measured the difference: Windows' own
//! renders on a 60 Hz timer and adds a frame (about 16 ms) to every echo;
//! 1.25 echoes in 0.07 ms and moves output six times faster.
//! `ARUGULA_CONPTY=inbox` uses Windows' own; `ARUGULA_CONPTY=<path>`
//! another `conpty.dll`.

use std::{
    ffi::c_void,
    fs::File,
    io,
    os::windows::io::{FromRawHandle, OwnedHandle},
    path::Path,
    ptr,
    sync::OnceLock,
    time::Duration,
};

use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE},
    System::{
        Console::{COORD, ClosePseudoConsole, CreatePseudoConsole, HPCON, ResizePseudoConsole},
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
            TerminateJobObject,
        },
        LibraryLoader::{GetProcAddress, LoadLibraryW},
        Pipes::CreatePipe,
        Threading::{
            CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList,
            EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, INFINITE, InitializeProcThreadAttributeList,
            LPPROC_THREAD_ATTRIBUTE_LIST, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, PROCESS_INFORMATION, ResumeThread,
            STARTF_USESTDHANDLES, STARTUPINFOEXW, UpdateProcThreadAttribute, WaitForSingleObject,
        },
    },
};

/// How long a closed program has after its console goes before its job is
/// ended (the shim's KILL_AFTER on Unix).
pub const KILL_AFTER: Duration = Duration::from_secs(3);

type CreateFn = unsafe extern "system" fn(COORD, HANDLE, HANDLE, u32, *mut HPCON) -> i32;
type ResizeFn = unsafe extern "system" fn(HPCON, COORD) -> i32;
type CloseFn = unsafe extern "system" fn(HPCON);

struct Api {
    create: CreateFn,
    resize: ResizeFn,
    close: CloseFn,
    /// Which one, for the log.
    from: String,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn api() -> &'static Api {
    static API: OnceLock<Api> = OnceLock::new();
    API.get_or_init(|| {
        let inbox = || Api {
            create: CreatePseudoConsole,
            resize: ResizePseudoConsole,
            close: ClosePseudoConsole,
            from: "Windows".into(),
        };
        let dll = match std::env::var("ARUGULA_CONPTY") {
            Ok(v) if v == "inbox" => return inbox(),
            Ok(v) if !v.is_empty() => Some(std::path::PathBuf::from(v)),
            _ => std::env::current_exe().ok().map(|e| e.with_file_name("conpty.dll")).filter(|p| p.is_file()),
        };
        let Some(dll) = dll else { return inbox() };
        match load(&dll) {
            Some(api) => api,
            None => {
                tracing::warn!(dll = %dll.display(), "can't load this ConPTY; using Windows' own");
                inbox()
            }
        }
    })
}

#[allow(clippy::missing_transmute_annotations)]
fn load(dll: &Path) -> Option<Api> {
    // SAFETY: loading a library and looking up functions whose signatures
    // are ConPTY's own (conpty.dll exports the kernel32 names).
    unsafe {
        let m = LoadLibraryW(wide(&dll.display().to_string()).as_ptr());
        if m.is_null() {
            return None;
        }
        let f = |n: &[u8]| GetProcAddress(m, n.as_ptr());
        Some(Api {
            create: std::mem::transmute(f(b"CreatePseudoConsole\0")?),
            resize: std::mem::transmute(f(b"ResizePseudoConsole\0")?),
            close: std::mem::transmute(f(b"ClosePseudoConsole\0")?),
            from: dll.display().to_string(),
        })
    }
}

/// Which ConPTY panes get, for the log.
pub fn which() -> &'static str {
    &api().from
}

/// A program on a pseudoconsole.
pub struct Pty {
    hpc: HPCON,
    closed: std::sync::atomic::AtomicBool,
    job: OwnedHandle,
    process: OwnedHandle,
    pub pid: u32,
}

// SAFETY: the handles are plain kernel handles; ConPTY's calls on an HPCON
// may come from any thread.
unsafe impl Send for Pty {}
unsafe impl Sync for Pty {}

/// What [`spawn`] hands back: the pane's pseudoconsole, and the pipe ends
/// its input goes into and its output comes out of.
pub struct Spawned {
    pub pty: Pty,
    pub input: File,
    pub output: File,
}

pub struct Command<'a> {
    pub program: &'a str,
    pub args: &'a [String],
    pub cwd: &'a Path,
    /// Over this process's own environment (names compared without case).
    pub env: &'a [(String, String)],
    pub cols: u16,
    pub rows: u16,
}

fn last() -> io::Error {
    io::Error::last_os_error()
}

fn owned(h: HANDLE) -> OwnedHandle {
    // SAFETY: a handle we just made and nothing else owns.
    unsafe { OwnedHandle::from_raw_handle(h) }
}

pub fn spawn(c: &Command) -> io::Result<Spawned> {
    let api = api();
    // SAFETY: Win32 calls with valid pointers; every handle made here is
    // owned (and closed) by what it ends up in.
    unsafe {
        let (mut in_r, mut in_w, mut out_r, mut out_w) =
            (ptr::null_mut(), ptr::null_mut(), ptr::null_mut(), ptr::null_mut());
        if CreatePipe(&mut in_r, &mut in_w, ptr::null(), 0) == 0 {
            return Err(last());
        }
        let (in_r, in_w) = (owned(in_r), owned(in_w));
        if CreatePipe(&mut out_r, &mut out_w, ptr::null(), 0) == 0 {
            return Err(last());
        }
        let (out_r, out_w) = (owned(out_r), owned(out_w));
        let mut hpc: HPCON = 0;
        let size = COORD { X: c.cols.max(1) as i16, Y: c.rows.max(1) as i16 };
        use std::os::windows::io::AsRawHandle;
        let hr = (api.create)(size, in_r.as_raw_handle(), out_w.as_raw_handle(), 0, &mut hpc);
        // The pseudoconsole has its own copies.
        drop((in_r, out_w));
        if hr < 0 {
            return Err(io::Error::other(format!("CreatePseudoConsole: {hr:#x}")));
        }
        let close_on_error = |e: io::Error| {
            (api.close)(hpc);
            e
        };

        let mut size = 0usize;
        InitializeProcThreadAttributeList(ptr::null_mut(), 1, 0, &mut size);
        let mut attrs = vec![0u8; size];
        let list = attrs.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
        if InitializeProcThreadAttributeList(list, 1, 0, &mut size) == 0 {
            return Err(close_on_error(last()));
        }
        // The attribute's value is the HPCON itself, not a pointer to it.
        if UpdateProcThreadAttribute(
            list,
            0,
            PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
            hpc as *const c_void,
            std::mem::size_of::<HPCON>(),
            ptr::null_mut(),
            ptr::null(),
        ) == 0
        {
            DeleteProcThreadAttributeList(list);
            return Err(close_on_error(last()));
        }
        let mut si: STARTUPINFOEXW = std::mem::zeroed();
        si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        // Null std handles (S29): a daemon whose own are redirected (a
        // logon task, a service, an ssh session) would otherwise hand them to
        // the program, which then never reads the pseudoconsole.
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.lpAttributeList = list;
        let mut cmdline = wide(&command_line(c.program, c.args));
        let mut env = env_block(c.env);
        let cwd = wide(&c.cwd.display().to_string());
        let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
        let ok = CreateProcessW(
            ptr::null(),
            cmdline.as_mut_ptr(),
            ptr::null(),
            ptr::null(),
            0,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT,
            env.as_mut_ptr() as *const c_void,
            if c.cwd.is_dir() { cwd.as_ptr() } else { ptr::null() },
            &si.StartupInfo,
            &mut pi,
        );
        DeleteProcThreadAttributeList(list);
        if ok == 0 {
            return Err(close_on_error(io::Error::other(format!("can't start {}: {}", c.program, last()))));
        }
        let process = owned(pi.hProcess);
        // The program's tree lives as long as this job: closing the pane
        // ends it all (killpg's part on Unix).
        let job = CreateJobObjectW(ptr::null(), ptr::null());
        if job.is_null() {
            CloseHandle(pi.hThread);
            return Err(close_on_error(last()));
        }
        let job = owned(job);
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const c_void,
            std::mem::size_of_val(&info) as u32,
        );
        if AssignProcessToJobObject(job.as_raw_handle(), process.as_raw_handle()) == 0 {
            tracing::warn!(error = %last(), "the pane's program isn't in a job; closing it may leave its children");
        }
        ResumeThread(pi.hThread);
        CloseHandle(pi.hThread);
        Ok(Spawned {
            pty: Pty { hpc, closed: Default::default(), job, process, pid: pi.dwProcessId },
            input: File::from(in_w),
            output: File::from(out_r),
        })
    }
}

impl Pty {
    pub fn resize(&self, cols: u16, rows: u16) {
        let size = COORD { X: cols.max(1) as i16, Y: rows.max(1) as i16 };
        // SAFETY: a live pseudoconsole.
        let hr = unsafe { (api().resize)(self.hpc, size) };
        if hr < 0 {
            tracing::warn!(hr, "ResizePseudoConsole failed");
        }
    }

    /// Block until the program has exited; its exit code.
    pub fn wait(&self) -> i32 {
        use std::os::windows::io::AsRawHandle;
        let mut code = 0u32;
        // SAFETY: our process handle.
        unsafe {
            WaitForSingleObject(self.process.as_raw_handle(), INFINITE);
            GetExitCodeProcess(self.process.as_raw_handle(), &mut code);
        }
        code as i32
    }

    /// The console goes: the program gets CTRL_CLOSE_EVENT, and the output
    /// pipe ends once what's left in it is read. Off this thread:
    /// ClosePseudoConsole can wait for its output to be drained.
    pub fn close_console(&self) {
        if self.closed.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let hpc = self.hpc;
        std::thread::spawn(move || {
            // SAFETY: a pseudoconsole closed once.
            unsafe { (api().close)(hpc) };
        });
    }

    /// Close the pane: the console now, the job if it's still there a moment
    /// later.
    pub fn hang_up(&self) {
        self.close_console();
        let job = self.job.try_clone();
        std::thread::spawn(move || {
            std::thread::sleep(KILL_AFTER);
            if let Ok(job) = job {
                use std::os::windows::io::AsRawHandle;
                // SAFETY: our job handle.
                unsafe { TerminateJobObject(job.as_raw_handle(), 1) };
            }
        });
    }
}

/// One command line from a program and its arguments, quoted the way
/// Windows programs split them (`CommandLineToArgvW`'s rules).
pub fn command_line(program: &str, args: &[String]) -> String {
    std::iter::once(program).chain(args.iter().map(String::as_str)).map(quote).collect::<Vec<_>>().join(" ")
}

fn quote(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\x0b', '"']) {
        return arg.to_owned();
    }
    let mut out = String::from("\"");
    let mut backslashes = 0;
    for ch in arg.chars() {
        match ch {
            '\\' => backslashes += 1,
            '"' => {
                // Backslashes before a quote are doubled, and the quote escaped.
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(ch);
                backslashes = 0;
            }
        }
    }
    // Before the closing quote, too.
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
    out
}

/// This process's environment with `extra` over it, as CreateProcessW wants
/// it: `NAME=value\0` each, sorted without case, then one more `\0`. Names
/// are compared without case (Windows has `Path`; panes are given `PATH`).
fn env_block(extra: &[(String, String)]) -> Vec<u16> {
    let mut vars: Vec<(String, String)> = Vec::new();
    for (k, v) in std::env::vars_os()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
        .chain(extra.iter().cloned())
    {
        vars.retain(|(have, _)| !have.eq_ignore_ascii_case(&k));
        vars.push((k, v));
    }
    vars.sort_by_key(|(k, _)| k.to_uppercase());
    let mut block = Vec::new();
    for (k, v) in vars {
        // `=C:` and friends (per-drive directories) start with '='; keep them.
        if k.is_empty() {
            continue;
        }
        block.extend(format!("{k}={v}").encode_utf16());
        block.push(0);
    }
    block.push(0);
    if block.len() == 1 {
        block.push(0);
    }
    block
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_follows_windows_rules() {
        assert_eq!(command_line("pwsh", &["-NoLogo".into()]), "pwsh -NoLogo");
        assert_eq!(quote(""), "\"\"");
        assert_eq!(quote("a b"), "\"a b\"");
        assert_eq!(quote(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(quote(r"C:\Program Files\x\"), r#""C:\Program Files\x\\""#);
        assert_eq!(quote(r"C:\plain\path"), r"C:\plain\path");
    }

    #[test]
    fn the_environment_block_overrides_without_case() {
        let block = env_block(&[("PATH".into(), "X".into()), ("ARUGULA_PANE".into(), "3".into())]);
        let text = String::from_utf16_lossy(&block);
        let vars: Vec<&str> = text.split('\0').filter(|s| !s.is_empty()).collect();
        assert_eq!(vars.iter().filter(|v| v.to_uppercase().starts_with("PATH=")).count(), 1);
        assert!(vars.contains(&"PATH=X"));
        assert!(vars.contains(&"ARUGULA_PANE=3"));
        assert!(block.ends_with(&[0, 0]));
    }

    #[test]
    fn a_pane_runs_reads_writes_and_exits() {
        use std::io::{Read, Write};
        let dir = std::env::temp_dir();
        let s = spawn(&Command {
            program: "cmd",
            // Delayed expansion: cmd expands %X% when it reads the line.
            args: &["/v:on".into(), "/c".into(), "set /p X=& echo got !X!& exit 7".into()],
            cwd: &dir,
            env: &[],
            cols: 80,
            rows: 24,
        })
        .unwrap();
        let mut input = s.input;
        input.write_all(b"hello\r\n").unwrap();
        let pty = std::sync::Arc::new(s.pty);
        let waiter = pty.clone();
        let code = std::thread::spawn(move || {
            let c = waiter.wait();
            waiter.close_console();
            c
        });
        let mut out = Vec::new();
        let mut output = s.output;
        let _ = output.read_to_end(&mut out);
        assert_eq!(code.join().unwrap(), 7);
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("got hello"), "{text:?}");
    }
}
