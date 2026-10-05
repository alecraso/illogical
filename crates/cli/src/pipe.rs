//! The local daemon's named pipe, as a [`Stream`] (Windows).
//!
//! Opened as a plain (synchronous) handle, which is enough for one thread
//! that reads and writes in turn: a read only blocks when asked to, and
//! "non-blocking" reads ask the pipe first how much is waiting
//! (`PeekNamedPipe`), so the front ends' loops never sit in a read while
//! they have something to write (S29).

use std::{
    fs::File,
    io::{self, Read, Write},
    os::windows::io::AsRawHandle,
    path::Path,
    ptr,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use windows_sys::Win32::{Foundation::ERROR_BROKEN_PIPE, System::Pipes::PeekNamedPipe};

pub struct PipeStream {
    file: File,
    nonblocking: AtomicBool,
    timeout: Mutex<Option<Duration>>,
}

/// Every instance of the pipe is serving someone: the next is a moment away.
const ERROR_PIPE_BUSY: i32 = 231;

impl PipeStream {
    pub fn connect(path: &Path) -> io::Result<Self> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match std::fs::OpenOptions::new().read(true).write(true).open(path) {
                Ok(file) => return Ok(Self { file, nonblocking: AtomicBool::new(false), timeout: Mutex::new(None) }),
                Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(2))
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Bytes waiting; `None` once the other end has gone.
    fn waiting(&self) -> Option<u32> {
        let mut avail = 0u32;
        // SAFETY: peeking our own pipe handle, no buffer.
        let ok = unsafe {
            PeekNamedPipe(self.file.as_raw_handle(), ptr::null_mut(), 0, ptr::null_mut(), &mut avail, ptr::null_mut())
        };
        (ok != 0).then_some(avail)
    }

    pub fn readable(&self) -> bool {
        // Gone counts: the read that follows says so.
        self.waiting().is_none_or(|n| n > 0)
    }

    pub fn set_nonblocking(&self, on: bool) {
        self.nonblocking.store(on, Ordering::Relaxed);
    }

    pub fn set_timeout(&self, t: Option<Duration>) {
        *self.timeout.lock().unwrap() = t;
    }
}

impl Read for PipeStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let nonblocking = self.nonblocking.load(Ordering::Relaxed);
        let timeout = *self.timeout.lock().unwrap();
        if !nonblocking && timeout.is_none() {
            return match self.file.read(buf) {
                Err(e) if e.raw_os_error() == Some(ERROR_BROKEN_PIPE as i32) => Ok(0),
                r => r,
            };
        }
        let deadline = timeout.map(|t| Instant::now() + t);
        loop {
            match self.waiting() {
                // The daemon closed its end.
                None => return Ok(0),
                Some(0) if nonblocking => return Err(io::ErrorKind::WouldBlock.into()),
                Some(0) => {
                    if deadline.is_some_and(|d| Instant::now() >= d) {
                        return Err(io::ErrorKind::TimedOut.into());
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                Some(n) => {
                    let n = (n as usize).min(buf.len());
                    return self.file.read(&mut buf[..n]);
                }
            }
        }
    }
}

impl Write for PipeStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.file.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}
