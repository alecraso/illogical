use std::{
    ffi::c_void,
    io, mem,
    os::windows::io::AsRawHandle,
    ptr,
    time::{Duration, Instant},
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::windows::named_pipe::{ClientOptions, ServerOptions},
    sync::{mpsc, watch},
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, ERROR_PIPE_BUSY, HANDLE},
    System::LibraryLoader::{GetProcAddress, LoadLibraryW},
    Security::{
        Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1},
        EqualSid, GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
    },
    Storage::FileSystem::{ReadFile, WriteFile},
    System::{
        Console::{
            ClosePseudoConsole, COORD, CreatePseudoConsole, ENABLE_VIRTUAL_TERMINAL_INPUT, GetStdHandle, HPCON,
            ResizePseudoConsole, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleMode,
        },
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
            TerminateJobObject,
        },
        Pipes::{CreatePipe, GetNamedPipeClientProcessId},
        Threading::{
            CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT,
            CreateProcessW, DETACHED_PROCESS, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT,
            GetCurrentProcess, GetExitCodeProcess, INFINITE, InitializeProcThreadAttributeList,
            LPPROC_THREAD_ATTRIBUTE_LIST, OpenProcess, OpenProcessToken, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
            PROCESS_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW, STARTUPINFOW,
            UpdateProcThreadAttribute, WaitForSingleObject,
        },
    },
};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn last() -> io::Error {
    io::Error::last_os_error()
}

struct Handle(HANDLE);
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}
impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CloseHandle(self.0) };
        }
    }
}

#[derive(Debug)]
enum Frame {
    Data(Vec<u8>),
    Resize(u16, u16),
    Close,
    Exit(i32),
}

async fn write_frame<W: AsyncWriteExt + Unpin>(w: &mut W, f: &Frame) -> io::Result<()> {
    let (kind, payload): (u8, Vec<u8>) = match f {
        Frame::Data(d) => (0, d.clone()),
        Frame::Resize(c, r) => (1, [c.to_le_bytes(), r.to_le_bytes()].concat()),
        Frame::Close => (2, vec![]),
        Frame::Exit(code) => (3, code.to_le_bytes().to_vec()),
    };
    let mut buf = Vec::with_capacity(5 + payload.len());
    buf.push(kind);
    buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    buf.extend_from_slice(&payload);
    w.write_all(&buf).await
}

async fn read_frame<R: AsyncReadExt + Unpin>(r: &mut R) -> io::Result<Frame> {
    let kind = r.read_u8().await?;
    let len = r.read_u32_le().await? as usize;
    let mut p = vec![0; len];
    r.read_exact(&mut p).await?;
    Ok(match kind {
        0 => Frame::Data(p),
        1 if len == 4 => Frame::Resize(u16::from_le_bytes([p[0], p[1]]), u16::from_le_bytes([p[2], p[3]])),
        2 => Frame::Close,
        3 if len == 4 => Frame::Exit(i32::from_le_bytes([p[0], p[1], p[2], p[3]])),
        _ => return Err(io::Error::other(format!("bad frame {kind}"))),
    })
}

// ---- which ConPTY: Windows' own, or a newer one we'd ship (S29_CONPTY_DLL,
// conpty.dll beside its OpenConsole.exe, from Microsoft's ConPTY package)

type CreateFn = unsafe extern "system" fn(COORD, HANDLE, HANDLE, u32, *mut HPCON) -> i32;
type ResizeFn = unsafe extern "system" fn(HPCON, COORD) -> i32;
type CloseFn = unsafe extern "system" fn(HPCON);

struct Api {
    create: CreateFn,
    resize: ResizeFn,
    close: CloseFn,
}

#[allow(clippy::missing_transmute_annotations)]
fn api() -> &'static Api {
    static API: std::sync::OnceLock<Api> = std::sync::OnceLock::new();
    API.get_or_init(|| {
        if let Ok(dll) = std::env::var("S29_CONPTY_DLL") {
            unsafe {
                let m = LoadLibraryW(wide(&dll).as_ptr());
                assert!(!m.is_null(), "LoadLibrary {dll}: {}", last());
                let f = |n: &str| GetProcAddress(m, format!("{n}\0").as_ptr()).unwrap_or_else(|| panic!("{n}"));
                return Api {
                    create: mem::transmute::<_, CreateFn>(f("CreatePseudoConsole")),
                    resize: mem::transmute::<_, ResizeFn>(f("ResizePseudoConsole")),
                    close: mem::transmute::<_, CloseFn>(f("ClosePseudoConsole")),
                };
            }
        }
        Api { create: CreatePseudoConsole, resize: ResizePseudoConsole, close: ClosePseudoConsole }
    })
}

// ---- the pane: a pseudoconsole and its program, in a job

struct Pty {
    hpc: HPCON,
    job: Handle,
    pid: u32,
    input: mpsc::Sender<Vec<u8>>,
    output: mpsc::Receiver<Vec<u8>>,
    exit: watch::Receiver<Option<i32>>,
}

unsafe impl Send for Pty {}
unsafe impl Sync for Pty {}

impl Pty {
    fn spawn(cmdline: &str, cols: u16, rows: u16) -> io::Result<Pty> {
        unsafe {
            let (mut in_r, mut in_w, mut out_r, mut out_w) =
                (ptr::null_mut(), ptr::null_mut(), ptr::null_mut(), ptr::null_mut());
            if CreatePipe(&mut in_r, &mut in_w, ptr::null(), 0) == 0 || CreatePipe(&mut out_r, &mut out_w, ptr::null(), 0) == 0
            {
                return Err(last());
            }
            let mut hpc: HPCON = 0;
            // S29_CONPTY_FLAGS: try ConPTY's flags (8 is passthrough mode).
            let flags = std::env::var("S29_CONPTY_FLAGS").ok().and_then(|f| f.parse().ok()).unwrap_or(0);
            let hr = (api().create)(COORD { X: cols as i16, Y: rows as i16 }, in_r, out_w, flags, &mut hpc);
            CloseHandle(in_r);
            CloseHandle(out_w);
            if hr < 0 {
                return Err(io::Error::other(format!("CreatePseudoConsole: {hr:#x}")));
            }
            let mut size = 0usize;
            InitializeProcThreadAttributeList(ptr::null_mut(), 1, 0, &mut size);
            let mut attrs = vec![0u8; size];
            let list = attrs.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
            if InitializeProcThreadAttributeList(list, 1, 0, &mut size) == 0 {
                return Err(last());
            }
            // The attribute's value is the HPCON itself, not a pointer to it.
            if UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                hpc as *const c_void,
                mem::size_of::<HPCON>(),
                ptr::null_mut(),
                ptr::null(),
            ) == 0
            {
                return Err(last());
            }
            let mut si: STARTUPINFOEXW = mem::zeroed();
            si.StartupInfo.cb = mem::size_of::<STARTUPINFOEXW>() as u32;
            // Null std handles: otherwise a parent whose own are redirected
            // (an ssh session, a logon task) hands them to the child, which
            // then reads and writes those instead of the pseudoconsole.
            si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
            si.lpAttributeList = list;
            let mut pi: PROCESS_INFORMATION = mem::zeroed();
            let mut cmd = wide(cmdline);
            let ok = CreateProcessW(
                ptr::null(),
                cmd.as_mut_ptr(),
                ptr::null(),
                ptr::null(),
                0,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT,
                ptr::null(),
                ptr::null(),
                &si.StartupInfo as *const STARTUPINFOW,
                &mut pi,
            );
            DeleteProcThreadAttributeList(list);
            if ok == 0 {
                return Err(last());
            }
            // The pane's tree lives exactly as long as this host holds the
            // job: killpg's replacement.
            let job = Handle(CreateJobObjectW(ptr::null(), ptr::null()));
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const c_void,
                mem::size_of_val(&info) as u32,
            );
            if AssignProcessToJobObject(job.0, pi.hProcess) == 0 {
                eprintln!("host: AssignProcessToJobObject: {}", last());
            }
            ResumeThread(pi.hThread);
            CloseHandle(pi.hThread);

            let (in_tx, mut in_rx) = mpsc::channel::<Vec<u8>>(64);
            let in_w = Handle(in_w);
            std::thread::spawn(move || {
                let in_w = in_w;
                while let Some(b) = in_rx.blocking_recv() {
                    let mut n = 0u32;
                    if WriteFile(in_w.0, b.as_ptr(), b.len() as u32, &mut n, ptr::null_mut()) == 0 {
                        break;
                    }
                }
            });
            // Small bound: with nobody reading, ConPTY's writer blocks, the
            // way a pty master nobody reads does.
            let (out_tx, out_rx) = mpsc::channel::<Vec<u8>>(4);
            let out_r = Handle(out_r);
            std::thread::spawn(move || {
                let out_r = out_r;
                let mut buf = vec![0u8; 64 * 1024];
                loop {
                    let mut n = 0u32;
                    if ReadFile(out_r.0, buf.as_mut_ptr(), buf.len() as u32, &mut n, ptr::null_mut()) == 0 || n == 0 {
                        break;
                    }
                    if out_tx.blocking_send(buf[..n as usize].to_vec()).is_err() {
                        break;
                    }
                }
            });
            let (exit_tx, exit_rx) = watch::channel(None);
            let process = Handle(pi.hProcess);
            std::thread::spawn(move || {
                let process = process;
                WaitForSingleObject(process.0, INFINITE);
                let mut code = 0u32;
                GetExitCodeProcess(process.0, &mut code);
                let _ = exit_tx.send(Some(code as i32));
            });
            Ok(Pty { hpc, job, pid: pi.dwProcessId, input: in_tx, output: out_rx, exit: exit_rx })
        }
    }

    fn resize(&self, cols: u16, rows: u16) {
        unsafe { (api().resize)(self.hpc, COORD { X: cols as i16, Y: rows as i16 }) };
    }

    /// Close: the console goes (the program gets CTRL_CLOSE_EVENT), then
    /// the job after a moment, as the shim's HUP then KILL.
    fn close(&self) {
        let hpc = self.hpc;
        // ClosePseudoConsole can wait for its output to be read: keep it off
        // this thread.
        std::thread::spawn(move || unsafe { (api().close)(hpc) });
        let job = self.job.0 as usize;
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(3));
            unsafe { TerminateJobObject(job as HANDLE, 1) };
        });
    }
}

// ---- who may connect: only this user (a DACL), checked again by SID

/// Owner and SYSTEM only; nobody else can even open the pipe. (A named
/// pipe's default DACL gives Everyone read.)
pub(crate) fn owner_only() -> io::Result<SECURITY_ATTRIBUTES> {
    let mut sd: PSECURITY_DESCRIPTOR = ptr::null_mut();
    let sddl = wide("D:P(A;;GA;;;OW)(A;;GA;;;SY)");
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), SDDL_REVISION_1, &mut sd, ptr::null_mut())
    } == 0
    {
        return Err(last());
    }
    Ok(SECURITY_ATTRIBUTES {
        nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd,
        bInheritHandle: 0,
    })
}

fn token_user(process: HANDLE) -> io::Result<Vec<u8>> {
    unsafe {
        let mut token = ptr::null_mut();
        if OpenProcessToken(process, TOKEN_QUERY, &mut token) == 0 {
            return Err(last());
        }
        let token = Handle(token);
        let mut len = 0u32;
        GetTokenInformation(token.0, TokenUser, ptr::null_mut(), 0, &mut len);
        let mut buf = vec![0u8; len as usize];
        if GetTokenInformation(token.0, TokenUser, buf.as_mut_ptr() as *mut c_void, len, &mut len) == 0 {
            return Err(last());
        }
        Ok(buf)
    }
}

/// The client on `pipe` runs as this process's user.
pub(crate) fn same_user(pipe: HANDLE) -> io::Result<bool> {
    unsafe {
        let mut pid = 0u32;
        if GetNamedPipeClientProcessId(pipe, &mut pid) == 0 {
            return Err(last());
        }
        let p = Handle(OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid));
        if p.0.is_null() {
            return Err(last());
        }
        let theirs = token_user(p.0)?;
        let ours = token_user(GetCurrentProcess())?;
        let a = (*(theirs.as_ptr() as *const TOKEN_USER)).User.Sid;
        let b = (*(ours.as_ptr() as *const TOKEN_USER)).User.Sid;
        Ok(EqualSid(a, b) != 0)
    }
}

pub(crate) fn pipe_name(name: &str) -> String {
    if name.starts_with(r"\\.\pipe\") { name.to_owned() } else { format!(r"\\.\pipe\{name}") }
}

// ---- host

async fn host(pipe: &str, grace: Duration, cmdline: &str) -> io::Result<()> {
    let name = pipe_name(pipe);
    let mut pty = Pty::spawn(cmdline, 120, 30)?;
    eprintln!("host: pane pid {} on {name}", pty.pid);
    let mut sa = owner_only()?;
    let mut first = true;
    loop {
        // SAFETY: `sa` is a valid SECURITY_ATTRIBUTES for the call.
        let server = unsafe {
            ServerOptions::new()
                .first_pipe_instance(first)
                .reject_remote_clients(true)
                .create_with_security_attributes_raw(&name, &mut sa as *mut _ as *mut c_void)?
        };
        first = false;
        // A lease: with no client for `grace`, the pane goes.
        let mut exit = pty.exit.clone();
        tokio::select! {
            r = server.connect() => r?,
            _ = tokio::time::sleep(grace) => {
                eprintln!("host: no client for {grace:?}; closing the pane");
                pty.close();
                tokio::time::sleep(Duration::from_secs(4)).await;
                return Ok(());
            }
            code = async { exit.wait_for(|c| c.is_some()).await.map(|v| (*v).unwrap_or(-1)).unwrap_or(-1) } => {
                eprintln!("host: the pane exited with no client: {code}");
                return Ok(());
            }
        }
        match same_user(server.as_raw_handle() as HANDLE) {
            Ok(true) => {}
            other => {
                eprintln!("host: refused a client: {other:?}");
                continue;
            }
        }
        let (mut r, mut w) = tokio::io::split(server);
        let (ftx, mut frx) = mpsc::channel::<Frame>(16);
        let reader = tokio::spawn(async move {
            while let Ok(f) = read_frame(&mut r).await {
                if ftx.send(f).await.is_err() {
                    break;
                }
            }
        });
        let mut exit = pty.exit.clone();
        let done = loop {
            tokio::select! {
                Some(chunk) = pty.output.recv() => {
                    // (A real host keeps a chunk it couldn't send for the next client.)
                    if write_frame(&mut w, &Frame::Data(chunk)).await.is_err() { break false; }
                }
                f = frx.recv() => match f {
                    Some(Frame::Data(d)) => { let _ = pty.input.send(d).await; }
                    Some(Frame::Resize(c, r)) => pty.resize(c, r),
                    Some(Frame::Close) => pty.close(),
                    Some(Frame::Exit(_)) => {}
                    None => break false,
                },
                code = async { exit.wait_for(|c| c.is_some()).await.map(|v| (*v).unwrap_or(-1)).unwrap_or(-1) } => {
                    // Let the last output through first.
                    while let Ok(Some(chunk)) = tokio::time::timeout(Duration::from_millis(200), pty.output.recv()).await {
                        let _ = write_frame(&mut w, &Frame::Data(chunk)).await;
                    }
                    let _ = write_frame(&mut w, &Frame::Exit(code)).await;
                    let _ = w.flush().await;
                    break true;
                }
            }
        };
        reader.abort();
        if done {
            eprintln!("host: the pane exited; leaving");
            tokio::time::sleep(Duration::from_millis(300)).await;
            return Ok(());
        }
        eprintln!("host: client left; waiting {grace:?} for another");
    }
}

/// Start a host that outlives this process (and the ssh session or
/// service that ran it).
fn spawn_detached(args: &[String]) -> io::Result<()> {
    let exe = std::env::current_exe()?;
    let mut cmd = format!("\"{}\" host", exe.display());
    for a in args {
        cmd.push(' ');
        if a.contains(' ') || a.contains('"') {
            cmd.push_str(&format!("\"{}\"", a.replace('"', "\\\"")));
        } else {
            cmd.push_str(a);
        }
    }
    let mut w = wide(&cmd);
    unsafe {
        let mut si: STARTUPINFOW = mem::zeroed();
        si.cb = mem::size_of::<STARTUPINFOW>() as u32;
        let mut pi: PROCESS_INFORMATION = mem::zeroed();
        let base = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_UNICODE_ENVIRONMENT;
        let mut ok = CreateProcessW(
            ptr::null(),
            w.as_mut_ptr(),
            ptr::null(),
            ptr::null(),
            0,
            base | CREATE_BREAKAWAY_FROM_JOB,
            ptr::null(),
            ptr::null(),
            &si,
            &mut pi,
        );
        let mut broke_away = true;
        if ok == 0 {
            // The job we're in may not allow breakaway.
            eprintln!("spawn: breakaway refused ({}); starting inside the job", last());
            broke_away = false;
            ok = CreateProcessW(
                ptr::null(),
                w.as_mut_ptr(),
                ptr::null(),
                ptr::null(),
                0,
                base,
                ptr::null(),
                ptr::null(),
                &si,
                &mut pi,
            );
        }
        if ok == 0 {
            return Err(last());
        }
        CloseHandle(pi.hThread);
        CloseHandle(pi.hProcess);
        println!("host pid {} (broke away from the job: {broke_away})", pi.dwProcessId);
    }
    Ok(())
}

// ---- clients

struct Conn {
    tx: mpsc::Sender<Frame>,
    rx: mpsc::Receiver<Frame>,
    seen: Vec<u8>,
    /// Bytes of output received so far, and lines in them.
    bytes: usize,
    lines: usize,
}

impl Conn {
    async fn pipe(pipe: &str) -> io::Result<Conn> {
        let name = pipe_name(pipe);
        let client = loop {
            match ClientOptions::new().open(&name) {
                Ok(c) => break c,
                Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                    tokio::time::sleep(Duration::from_millis(20)).await
                }
                Err(e) => return Err(e),
            }
        };
        let (mut r, mut w) = tokio::io::split(client);
        let (tx, mut out) = mpsc::channel::<Frame>(16);
        let (inc, rx) = mpsc::channel::<Frame>(16);
        tokio::spawn(async move {
            while let Some(f) = out.recv().await {
                if write_frame(&mut w, &f).await.is_err() {
                    break;
                }
            }
        });
        tokio::spawn(async move {
            while let Ok(f) = read_frame(&mut r).await {
                if inc.send(f).await.is_err() {
                    break;
                }
            }
        });
        Ok(Conn { tx, rx, seen: vec![], bytes: 0, lines: 0 })
    }

    /// The same, with the pane in this process: no host, no pipe.
    fn direct(cmdline: &str) -> io::Result<Conn> {
        let mut pty = Pty::spawn(cmdline, 120, 30)?;
        let (tx, mut out) = mpsc::channel::<Frame>(16);
        let (inc, rx) = mpsc::channel::<Frame>(16);
        tokio::spawn(async move {
            let mut exit = pty.exit.clone();
            loop {
                tokio::select! {
                    Some(c) = pty.output.recv() => { let _ = inc.send(Frame::Data(c)).await; }
                    f = out.recv() => match f {
                        Some(Frame::Data(d)) => { let _ = pty.input.send(d).await; }
                        Some(Frame::Resize(c, r)) => pty.resize(c, r),
                        Some(Frame::Close) => pty.close(),
                        _ => {}
                    },
                    code = async { exit.wait_for(|c| c.is_some()).await.map(|v| (*v).unwrap_or(-1)).unwrap_or(-1) } => {
                        let _ = inc.send(Frame::Exit(code)).await;
                        break;
                    }
                }
            }
        });
        Ok(Conn { tx, rx, seen: vec![], bytes: 0, lines: 0 })
    }

    async fn send(&self, s: &[u8]) {
        let _ = self.tx.send(Frame::Data(s.to_vec())).await;
    }

    /// Wait until the output since the last call holds `want`.
    async fn until(&mut self, want: &[u8], limit: Duration) -> io::Result<Duration> {
        let start = Instant::now();
        loop {
            if let Some(i) = self.seen.windows(want.len()).position(|w| w == want) {
                self.seen.drain(..i + want.len());
                return Ok(start.elapsed());
            }
            // Only a tail that could still hold the start of `want` (and a
            // little more, for the timeout's message).
            let keep = (want.len() + 300).min(self.seen.len());
            self.seen.drain(..self.seen.len() - keep);
            match tokio::time::timeout(limit.saturating_sub(start.elapsed()), self.rx.recv()).await {
                Ok(Some(Frame::Data(d))) => {
                    self.bytes += d.len();
                    self.lines += d.iter().filter(|b| **b == b'\n').count();
                    self.seen.extend_from_slice(&d)
                }
                Ok(Some(Frame::Exit(c))) => return Err(io::Error::other(format!("pane exited ({c})"))),
                Ok(Some(_)) => {}
                Ok(None) => return Err(io::Error::other("connection closed")),
                Err(_) => {
                    let tail = String::from_utf8_lossy(&self.seen[self.seen.len().saturating_sub(300)..]).into_owned();
                    return Err(io::Error::other(format!("timed out waiting for {:?}; last output {tail:?}", String::from_utf8_lossy(want))));
                }
            }
        }
    }

    async fn exit(&mut self, limit: Duration) -> io::Result<i32> {
        let start = Instant::now();
        loop {
            match tokio::time::timeout(limit.saturating_sub(start.elapsed()), self.rx.recv()).await {
                Ok(Some(Frame::Exit(c))) => return Ok(c),
                Ok(Some(_)) => {}
                Ok(None) => return Err(io::Error::other("connection closed before exit")),
                Err(_) => return Err(io::Error::other("no exit")),
            }
        }
    }
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() - 1) as f64 * p).round() as usize]
}

fn me() -> String {
    std::env::current_exe().unwrap().display().to_string()
}

async fn open(direct: bool, pipe: Option<&str>, cmdline: &str) -> io::Result<Conn> {
    if direct { Conn::direct(cmdline) } else { Conn::pipe(pipe.ok_or_else(|| io::Error::other("--pipe or --direct"))?).await }
}

async fn bench(what: &str, direct: bool, pipe: Option<&str>, opt: &dyn Fn(&str) -> Option<String>) -> io::Result<()> {
    let how = if direct { "direct" } else { "host+pipe" };
    match what {
        // Keystroke to echo, through a program that echoes raw input.
        "echo" => {
            let mut c = open(direct, pipe, &format!("\"{}\" echo", me())).await?;
            c.until(b"ECHO-READY", Duration::from_secs(10)).await?;
            let mut v = vec![];
            for i in 0..300 {
                let ch = [b'a' + (i % 26) as u8];
                let t = Instant::now();
                c.send(&ch).await;
                c.until(&ch, Duration::from_secs(5)).await?;
                v.push(t.elapsed().as_secs_f64() * 1000.0);
            }
            println!(
                "echo {how}: p50 {:.2} ms, p90 {:.2} ms, p99 {:.2} ms, max {:.2} ms (300 keys)",
                pct(&mut v, 0.5),
                pct(&mut v, 0.9),
                pct(&mut v, 0.99),
                pct(&mut v, 1.0)
            );
        }
        // A big file through `type`.
        "bulk" => {
            let file = std::env::temp_dir().join("s29-bulk.txt");
            let mb = 20usize;
            if !file.exists() {
                let line = format!("{}\r\n", "0123456789abcdefghijklmnopqrstuvwxyz".repeat(3));
                std::fs::write(&file, line.repeat(mb * 1024 * 1024 / line.len()))?;
            }
            // `type` writes a line at a time; `cat` (ours) 64 KB at a time.
            let cmd = if opt("--by").as_deref() == Some("cat") {
                format!("\"{}\" cat \"{}\"", me(), file.display())
            } else {
                format!("cmd /k type \"{}\" & echo BULK-DONE", file.display())
            };
            let mut c = open(direct, pipe, &cmd).await?;
            let t = Instant::now();
            c.until(b"BULK-DONE", Duration::from_secs(300)).await?;
            let s = t.elapsed().as_secs_f64();
            let sent = std::fs::metadata(&file)?.len() as usize;
            let sent_lines = std::fs::read(&file)?.iter().filter(|b| **b == b'\n').count();
            println!(
                "bulk {how}: {mb} MB in {s:.2} s ({:.1} MB/s of input); received {:.1} MB, {} of {} lines ({:.1}%)",
                mb as f64 / s,
                c.bytes as f64 / 1048576.0,
                c.lines,
                sent_lines,
                100.0 * c.lines as f64 / sent_lines as f64
            );
            let _ = sent;
        }
        // Resize, then ask the program what size it sees.
        "resize" => {
            let mut c = open(direct, pipe, "pwsh -NoLogo -NoProfile").await?;
            c.send(b"'READY'\r").await;
            c.until(b"READY", Duration::from_secs(20)).await?;
            let _ = c.tx.send(Frame::Resize(101, 37)).await;
            tokio::time::sleep(Duration::from_millis(300)).await;
            c.send(b"'SIZE=' + [Console]::WindowWidth + 'x' + [Console]::WindowHeight\r").await;
            c.until(b"SIZE=101x37", Duration::from_secs(10)).await?;
            println!("resize {how}: the program sees 101x37");
        }
        // Close: the program and its children go.
        "close" => {
            let mut c = open(direct, pipe, "pwsh -NoLogo -NoProfile").await?;
            c.send(b"'READY'\r").await;
            c.until(b"READY", Duration::from_secs(20)).await?;
            c.send(b"Start-Process -NoNewWindow ping -ArgumentList '-t','127.0.0.1'; 'STARTED'\r").await;
            c.until(b"STARTED", Duration::from_secs(10)).await?;
            let t = Instant::now();
            let _ = c.tx.send(Frame::Close).await;
            let code = c.exit(Duration::from_secs(15)).await?;
            println!("close {how}: exited ({code}) {:.0} ms after close", t.elapsed().as_secs_f64() * 1000.0);
        }
        // A "daemon" that types something and goes away.
        "put" => {
            let c = open(false, pipe, "").await?;
            let text = opt("--text").unwrap_or_default().replace("\\r", "\r");
            c.send(text.as_bytes()).await;
            tokio::time::sleep(Duration::from_millis(500)).await;
            println!("put: sent {text:?}, leaving");
        }
        // A later "daemon": send, and wait for what should come back.
        "expect" => {
            let t = Instant::now();
            let mut c = open(false, pipe, "").await?;
            let connected = t.elapsed();
            let send = opt("--send").unwrap_or_default().replace("\\r", "\r");
            // `\e` for ESC, to look for escape sequences.
            let want = opt("--want").unwrap_or_default().replace("\\e", "\x1b");
            c.send(send.as_bytes()).await;
            c.until(want.as_bytes(), Duration::from_secs(20)).await?;
            println!("expect: connected in {:.1} ms, saw {want:?}", connected.as_secs_f64() * 1000.0);
            if let Some(also) = opt("--also").map(|a| a.replace("\\e", "\x1b")) {
                c.until(also.as_bytes(), Duration::from_secs(20)).await?;
                println!("expect: also saw {also:?}");
            }
        }
        _ => return Err(io::Error::other(format!("no bench {what}"))),
    }
    Ok(())
}

/// A file to stdout in big writes, then a marker; stays open.
fn cat(path: &str) {
    let data = std::fs::read(path).expect("read");
    unsafe {
        let o = GetStdHandle(STD_OUTPUT_HANDLE);
        for chunk in data.chunks(64 * 1024).chain([&b"BULK-DONE"[..]]) {
            let mut m = 0u32;
            WriteFile(o, chunk.as_ptr(), chunk.len() as u32, &mut m, ptr::null_mut());
        }
    }
    std::thread::sleep(Duration::from_secs(3600));
}

/// Raw stdin to stdout, for the echo bench.
fn echo() {
    unsafe {
        let i = GetStdHandle(STD_INPUT_HANDLE);
        let o = GetStdHandle(STD_OUTPUT_HANDLE);
        SetConsoleMode(i, ENABLE_VIRTUAL_TERMINAL_INPUT);
        let ready = b"ECHO-READY";
        let mut m = 0u32;
        WriteFile(o, ready.as_ptr(), ready.len() as u32, &mut m, ptr::null_mut());
        let mut buf = [0u8; 256];
        loop {
            let mut n = 0u32;
            if ReadFile(i, buf.as_mut_ptr(), buf.len() as u32, &mut n, ptr::null_mut()) == 0 || n == 0 {
                return;
            }
            let mut m = 0u32;
            WriteFile(o, buf.as_ptr(), n, &mut m, ptr::null_mut());
        }
    }
}

async fn attach(pipe: &str) -> io::Result<()> {
    let mut c = Conn::pipe(pipe).await?;
    unsafe {
        SetConsoleMode(GetStdHandle(STD_INPUT_HANDLE), ENABLE_VIRTUAL_TERMINAL_INPUT);
    }
    let tx = c.tx.clone();
    std::thread::spawn(move || unsafe {
        let i = GetStdHandle(STD_INPUT_HANDLE);
        let mut buf = [0u8; 256];
        loop {
            let mut n = 0u32;
            if ReadFile(i, buf.as_mut_ptr(), buf.len() as u32, &mut n, ptr::null_mut()) == 0 || n == 0 {
                return;
            }
            if buf[..n as usize].contains(&0x1d) {
                std::process::exit(0);
            }
            let _ = tx.blocking_send(Frame::Data(buf[..n as usize].to_vec()));
        }
    });
    let mut out = tokio::io::stdout();
    while let Some(f) = c.rx.recv().await {
        match f {
            Frame::Data(d) => {
                out.write_all(&d).await?;
                out.flush().await?;
            }
            Frame::Exit(code) => {
                eprintln!("\r\n[pane exited: {code}]");
                return Ok(());
            }
            _ => {}
        }
    }
    Ok(())
}

pub fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let opt = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let rest = || args.iter().position(|a| a == "--").map(|i| args[i + 1..].join(" ")).unwrap_or_default();
    let grace = Duration::from_secs(opt("--grace").and_then(|g| g.parse().ok()).unwrap_or(60));
    let rt = tokio::runtime::Runtime::new().unwrap();
    let r = match args.first().map(String::as_str) {
        Some("echo") => {
            echo();
            Ok(())
        }
        Some("http-serve") => rt.block_on(crate::http::serve(&opt("--pipe").expect("--pipe"), &opt("--tcp").expect("--tcp"))),
        Some("http-bench") => crate::http::bench(&opt("--pipe").expect("--pipe"), &opt("--tcp").expect("--tcp")),
        Some("cat") => {
            cat(&args[1]);
            Ok(())
        }
        Some("host") => rt.block_on(host(&opt("--pipe").expect("--pipe"), grace, &rest())),
        Some("spawn") => spawn_detached(&args[1..]),
        Some("attach") => rt.block_on(attach(&opt("--pipe").expect("--pipe"))),
        Some("bench") => {
            let what = args.get(1).cloned().unwrap_or_default();
            let direct = args.iter().any(|a| a == "--direct");
            rt.block_on(bench(&what, direct, opt("--pipe").as_deref(), &opt))
        }
        _ => Err(io::Error::other("usage: see the top of main.rs")),
    };
    if let Err(e) = r {
        eprintln!("conpty-host: {e}");
        std::process::exit(1);
    }
    // Background threads (the pane's readers) would keep the runtime up.
    std::process::exit(0);
}
