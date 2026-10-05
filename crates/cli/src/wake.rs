//! Waiting in the terminal front ends' loops (`attach`, `tui`, control's
//! link) until the daemon's connection has something to read, another
//! thread wakes us (keys, API answers), or time is up.
//!
//! On Unix that's `poll` on the connection and a socket pair. Windows can't
//! wait on a socket and a pipe at once without overlapped I/O, so there the
//! loop waits on a condvar in short slices and asks the connection whether
//! it has anything ([`Stream::readable`]): a few milliseconds of latency,
//! and nothing read on another thread (S29: a blocked read on a pipe holds
//! up writes to it).

use std::time::Duration;

#[cfg(unix)]
use std::{
    io::{Read, Write},
    os::{fd::AsFd, unix::net::UnixStream},
    sync::Arc,
};
#[cfg(windows)]
use std::{
    sync::{Arc, Condvar, Mutex},
    time::Instant,
};

use crate::http::Stream;

/// Wakes the loop from another thread.
#[derive(Clone)]
pub struct Waker {
    #[cfg(unix)]
    w: Arc<UnixStream>,
    #[cfg(windows)]
    w: Arc<(Mutex<bool>, Condvar)>,
}

/// The loop's end of a [`Waker`].
pub struct Wake {
    #[cfg(unix)]
    r: UnixStream,
    #[cfg(windows)]
    r: Arc<(Mutex<bool>, Condvar)>,
}

impl Waker {
    pub fn wake(&self) {
        #[cfg(unix)]
        {
            // Full means a wake-up is pending already.
            let _ = (&*self.w).write(&[1]);
        }
        #[cfg(windows)]
        {
            *self.w.0.lock().unwrap() = true;
            self.w.1.notify_one();
        }
    }
}

impl Wake {
    pub fn pair() -> std::io::Result<(Wake, Waker)> {
        #[cfg(unix)]
        {
            let (r, w) = UnixStream::pair()?;
            r.set_nonblocking(true)?;
            w.set_nonblocking(true)?;
            Ok((Wake { r }, Waker { w: Arc::new(w) }))
        }
        #[cfg(windows)]
        {
            let both = Arc::new((Mutex::new(false), Condvar::new()));
            Ok((Wake { r: both.clone() }, Waker { w: both }))
        }
    }

    /// Clear pending wake-ups.
    pub fn drain(&mut self) {
        #[cfg(unix)]
        {
            let mut buf = [0u8; 64];
            while matches!(self.r.read(&mut buf), Ok(n) if n > 0) {}
        }
        #[cfg(windows)]
        {
            *self.r.0.lock().unwrap() = false;
        }
    }

    /// Wait until `conn` (if any) has something to read, a wake-up comes, or
    /// `timeout` passes: (readable, woken). Either may be a false alarm; the
    /// caller reads without blocking.
    pub fn wait(&mut self, conn: Option<&dyn Stream>, timeout: Duration) -> std::io::Result<(bool, bool)> {
        #[cfg(unix)]
        {
            use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
            let wake = self.r.as_fd();
            let mut fds: Vec<PollFd> = conn.iter().map(|c| PollFd::new(c.fd(), PollFlags::POLLIN)).collect();
            fds.push(PollFd::new(wake, PollFlags::POLLIN));
            match poll(&mut fds, PollTimeout::try_from(timeout.as_millis() as u64).unwrap_or(PollTimeout::MAX)) {
                Ok(_) | Err(nix::errno::Errno::EINTR) => {}
                Err(e) => return Err(e.into()),
            }
            let r = |f: &PollFd| f.revents().is_some_and(|e| !e.is_empty());
            let woken = r(fds.last().unwrap());
            Ok((conn.is_some() && r(&fds[0]), woken))
        }
        #[cfg(windows)]
        {
            // Short slices: the connection can't wake us itself.
            const SLICE: Duration = Duration::from_millis(3);
            let deadline = Instant::now() + timeout;
            let (lock, cv) = &*self.r;
            let mut woken = lock.lock().unwrap();
            loop {
                if *woken {
                    *woken = false;
                    return Ok((conn.is_some_and(|c| c.readable()), true));
                }
                if conn.is_some_and(|c| c.readable()) {
                    return Ok((true, false));
                }
                let now = Instant::now();
                if now >= deadline {
                    return Ok((false, false));
                }
                woken = cv.wait_timeout(woken, SLICE.min(deadline - now)).unwrap().0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    #[test]
    fn a_wake_from_another_thread_ends_the_wait() {
        let (mut wake, waker) = Wake::pair().unwrap();
        let t = Instant::now();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            waker.wake();
        });
        let (readable, woken) = wake.wait(None, Duration::from_secs(5)).unwrap();
        assert!(woken && !readable);
        assert!(t.elapsed() < Duration::from_secs(2));
        wake.drain();
    }

    #[test]
    fn with_nothing_to_wait_for_it_times_out() {
        let (mut wake, _waker) = Wake::pair().unwrap();
        let t = Instant::now();
        assert_eq!(wake.wait(None, Duration::from_millis(60)).unwrap(), (false, false));
        assert!(t.elapsed() >= Duration::from_millis(50));
    }

    #[test]
    fn a_connection_with_bytes_waiting_is_readable() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let ours = std::net::TcpStream::connect(l.local_addr().unwrap()).unwrap();
        let (mut theirs, _) = l.accept().unwrap();
        ours.set_nonblocking(true).unwrap();
        let (mut wake, _waker) = Wake::pair().unwrap();
        std::io::Write::write_all(&mut theirs, b"x").unwrap();
        let (readable, _) = wake.wait(Some(&ours), Duration::from_secs(5)).unwrap();
        assert!(readable);
    }
}
