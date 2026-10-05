//! Many byte streams over one WebSocket (M4c's dial-out transport).
//!
//! A host that can only dial out keeps one WebSocket open to the home
//! daemon. The home daemon opens a stream over it for each request it
//! forwards (an HTTP request, or a client's WebSocket), and the host serves
//! each stream as a connection of its own, as if it had been accepted on a
//! port. Only the home end opens streams: the host can answer, never ask, so
//! nothing routes through a host.
//!
//! Each binary message is one frame: `kind (1) | stream (4, BE) | payload`.
//!
//! - `OPEN`: a new stream (home to host only). An empty payload is a
//!   stream of Noise messages, as always; a payload names a raw stream's
//!   kind (M65: `ssh-guest`, a guest's ssh connection that ends at the
//!   host). A host that doesn't take that kind resets it.
//! - `DATA`: bytes, at most what the receiver has granted.
//! - `FIN`: the sender won't write any more (the reader sees end of file).
//! - `RESET`: the stream is gone, in both directions.
//! - `WINDOW`: payload is a u32: the receiver consumed that many bytes, so
//!   the sender may send that many more.
//!
//! Flow control is per stream: each starts with `WINDOW` bytes of credit,
//! so one slow reader (a client that stopped reading a pane's output) holds
//! up only its own stream, and what a peer can make us buffer is bounded.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, DuplexStream},
    sync::{Semaphore, mpsc},
};

const OPEN: u8 = 1;
const DATA: u8 = 2;
const FIN: u8 = 3;
const RESET: u8 = 4;
const GRANT: u8 = 5;

/// Credit each direction of a stream starts with.
const WINDOW: u32 = 256 * 1024;
/// Largest DATA payload.
const CHUNK: usize = 16 * 1024;
/// The in-process pipe between a stream and its user.
const PIPE: usize = 64 * 1024;

/// One end of a tunnel. Frames from the socket go to [`Mux::handle`];
/// frames for the socket come out of the receiver [`Mux::new`] returns.
#[derive(Clone)]
pub struct Mux {
    inner: Arc<Inner>,
}

struct Inner {
    out: mpsc::UnboundedSender<Vec<u8>>,
    streams: Mutex<HashMap<u32, Slot>>,
    next: AtomicU32,
    /// The host end: where streams the home daemon opens are handed over.
    accept: Option<mpsc::UnboundedSender<DuplexStream>>,
    /// The host end: where raw streams go, with their kind.
    raw: Option<mpsc::UnboundedSender<(Vec<u8>, DuplexStream)>>,
}

struct Slot {
    /// Bytes that arrived for the stream's user; `None` once FIN arrived.
    incoming: Option<mpsc::UnboundedSender<Vec<u8>>>,
    /// What we may still send.
    credit: Arc<Semaphore>,
}

fn frame(kind: u8, id: u32, payload: &[u8]) -> Vec<u8> {
    let mut f = Vec::with_capacity(5 + payload.len());
    f.push(kind);
    f.extend_from_slice(&id.to_be_bytes());
    f.extend_from_slice(payload);
    f
}

impl Mux {
    /// `accept`: this is the host end, and streams the other end opens are
    /// sent there. Without it, this is the home end and refuses them.
    pub fn new(accept: Option<mpsc::UnboundedSender<DuplexStream>>) -> (Self, mpsc::UnboundedReceiver<Vec<u8>>) {
        Self::with_raw(accept, None)
    }

    /// The host end, taking raw streams too: they go to `raw` with their
    /// kind.
    pub fn with_raw(
        accept: Option<mpsc::UnboundedSender<DuplexStream>>,
        raw: Option<mpsc::UnboundedSender<(Vec<u8>, DuplexStream)>>,
    ) -> (Self, mpsc::UnboundedReceiver<Vec<u8>>) {
        let (out, rx) = mpsc::unbounded_channel();
        let inner = Inner { out, streams: Mutex::new(HashMap::new()), next: AtomicU32::new(1), accept, raw };
        (Self { inner: Arc::new(inner) }, rx)
    }

    /// Open a stream to the other end (the home end only).
    pub fn open(&self) -> std::io::Result<DuplexStream> {
        self.open_kind(&[])
    }

    /// Open a raw stream of `kind` (not empty) to the other end: bytes as
    /// they are, not Noise messages.
    pub fn open_raw(&self, kind: &[u8]) -> std::io::Result<DuplexStream> {
        assert!(!kind.is_empty(), "a raw stream has a kind");
        self.open_kind(kind)
    }

    fn open_kind(&self, kind: &[u8]) -> std::io::Result<DuplexStream> {
        if self.inner.out.is_closed() {
            return Err(std::io::Error::new(std::io::ErrorKind::NotConnected, "the tunnel is closed"));
        }
        let id = self.inner.next.fetch_add(1, Ordering::Relaxed);
        let user = self.stream(id);
        self.send(OPEN, id, kind);
        Ok(user)
    }

    /// One frame from the socket. An error means the peer broke the
    /// protocol, and the tunnel should be dropped.
    pub fn handle(&self, f: &[u8]) -> Result<(), String> {
        if f.len() < 5 {
            return Err("short frame".into());
        }
        let (kind, id, payload) = (f[0], u32::from_be_bytes([f[1], f[2], f[3], f[4]]), &f[5..]);
        match kind {
            OPEN => match &self.inner.accept {
                Some(accept) => {
                    if self.inner.streams.lock().unwrap().contains_key(&id) {
                        return Err(format!("stream {id} opened twice"));
                    }
                    if payload.is_empty() {
                        let user = self.stream(id);
                        if accept.send(user).is_err() {
                            self.reset(id);
                        }
                    } else if let Some(raw) = &self.inner.raw {
                        let user = self.stream(id);
                        if raw.send((payload.to_vec(), user)).is_err() {
                            self.reset(id);
                        }
                    } else {
                        // A kind this end doesn't take.
                        self.send(RESET, id, &[]);
                    }
                }
                // Not a hub: a host can't open anything here.
                None => self.send(RESET, id, &[]),
            },
            DATA => {
                if payload.len() > WINDOW as usize {
                    return Err("frame larger than the window".into());
                }
                // (None: data that crossed a reset on the wire.)
                let streams = self.inner.streams.lock().unwrap();
                if let Some(tx) = streams.get(&id).and_then(|s| s.incoming.as_ref()) {
                    let _ = tx.send(payload.to_vec());
                }
            }
            FIN => {
                if let Some(s) = self.inner.streams.lock().unwrap().get_mut(&id) {
                    s.incoming = None;
                }
            }
            RESET => self.forget(id),
            GRANT => {
                let n = payload.get(..4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]])).unwrap_or(0);
                if let Some(s) = self.inner.streams.lock().unwrap().get(&id) {
                    // Bounded, so a peer can't overflow the semaphore.
                    if s.credit.available_permits() + n as usize <= 4 * WINDOW as usize {
                        s.credit.add_permits(n as usize);
                    }
                }
            }
            k => return Err(format!("unknown frame kind {k}")),
        }
        Ok(())
    }

    /// The socket is gone: every stream ends.
    pub fn close(&self) {
        let ids: Vec<u32> = self.inner.streams.lock().unwrap().keys().copied().collect();
        for id in ids {
            self.forget(id);
        }
    }

    /// Streams open now.
    pub fn streams(&self) -> usize {
        self.inner.streams.lock().unwrap().len()
    }

    fn send(&self, kind: u8, id: u32, payload: &[u8]) {
        let _ = self.inner.out.send(frame(kind, id, payload));
    }

    fn reset(&self, id: u32) {
        self.forget(id);
        self.send(RESET, id, &[]);
    }

    fn forget(&self, id: u32) {
        if let Some(s) = self.inner.streams.lock().unwrap().remove(&id) {
            s.credit.close();
        }
    }

    /// Set up stream `id`: the user's end of a pipe, and two tasks moving
    /// bytes between the pipe and frames.
    fn stream(&self, id: u32) -> DuplexStream {
        let (user, ours) = tokio::io::duplex(PIPE);
        let (mut rd, mut wr) = tokio::io::split(ours);
        let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let credit = Arc::new(Semaphore::new(WINDOW as usize));
        self.inner.streams.lock().unwrap().insert(id, Slot { incoming: Some(tx), credit: credit.clone() });

        // Incoming: into the pipe, granting credit back as it goes.
        let me = self.clone();
        let inbound = tokio::spawn(async move {
            while let Some(chunk) = rx.recv().await {
                if wr.write_all(&chunk).await.is_err() {
                    // The user went away.
                    me.reset(id);
                    return;
                }
                me.send(GRANT, id, &(chunk.len() as u32).to_be_bytes());
            }
            let _ = wr.shutdown().await;
        });
        // Outgoing: from the pipe, as far as our credit goes.
        let me = self.clone();
        let outbound = tokio::spawn(async move {
            let mut buf = vec![0u8; CHUNK];
            loop {
                let n = match rd.read(&mut buf).await {
                    Ok(0) | Err(_) => {
                        me.send(FIN, id, &[]);
                        return;
                    }
                    Ok(n) => n,
                };
                match credit.acquire_many(n as u32).await {
                    Ok(p) => p.forget(),
                    Err(_) => return, // reset
                }
                me.send(DATA, id, &buf[..n]);
            }
        });
        let me = self.clone();
        tokio::spawn(async move {
            let _ = tokio::join!(inbound, outbound);
            me.forget(id);
        });
        user
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two ends wired together in memory.
    fn pair() -> (Mux, Mux, mpsc::UnboundedReceiver<DuplexStream>) {
        let (accept_tx, accept_rx) = mpsc::unbounded_channel();
        let (home, mut home_out) = Mux::new(None);
        let (host, mut host_out) = Mux::new(Some(accept_tx));
        let (h, s) = (home.clone(), host.clone());
        tokio::spawn(async move {
            while let Some(f) = home_out.recv().await {
                s.handle(&f).unwrap();
            }
        });
        tokio::spawn(async move {
            while let Some(f) = host_out.recv().await {
                h.handle(&f).unwrap();
            }
        });
        (home, host, accept_rx)
    }

    #[tokio::test]
    async fn streams_carry_bytes_both_ways_and_end() {
        let (home, _host, mut accept) = pair();
        let mut a = home.open().unwrap();
        let mut b = accept.recv().await.unwrap();
        // More than the window, so credit has to come back.
        let big: Vec<u8> = (0..(3 * WINDOW as usize)).map(|i| (i % 251) as u8).collect();
        let sent = big.clone();
        let writer = tokio::spawn(async move {
            a.write_all(&sent).await.unwrap();
            a.shutdown().await.unwrap();
            let mut back = String::new();
            a.read_to_string(&mut back).await.unwrap();
            back
        });
        let mut got = Vec::new();
        b.read_to_end(&mut got).await.unwrap();
        assert_eq!(got, big);
        b.write_all(b"thanks").await.unwrap();
        drop(b);
        assert_eq!(writer.await.unwrap(), "thanks");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(home.streams(), 0, "both directions ended: forgotten");
    }

    #[tokio::test]
    async fn a_slow_reader_holds_up_only_its_own_stream() {
        let (home, _host, mut accept) = pair();
        let mut slow = home.open().unwrap();
        let _slow_far = accept.recv().await.unwrap();
        let mut fast = home.open().unwrap();
        let mut fast_far = accept.recv().await.unwrap();
        // Nobody reads `slow`'s far end: its writer stalls once the window
        // and the pipes are full...
        let stalled = tokio::spawn(async move { slow.write_all(&vec![0u8; 4 * WINDOW as usize]).await });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(!stalled.is_finished());
        // ...while another stream still flows.
        fast.write_all(b"ping").await.unwrap();
        let mut buf = [0u8; 4];
        fast_far.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping");
        stalled.abort();
    }

    #[tokio::test]
    async fn the_host_end_cannot_open_streams() {
        let (home, mut home_out) = Mux::new(None);
        home.handle(&frame(OPEN, 9, &[])).unwrap();
        assert_eq!(home_out.recv().await.unwrap(), frame(RESET, 9, &[]));
        assert_eq!(home.streams(), 0);
        assert!(home.handle(&[1, 2]).is_err(), "short");
        assert!(home.handle(&frame(77, 1, &[])).is_err(), "unknown kind");
    }

    #[tokio::test]
    async fn raw_streams_go_apart_and_an_end_without_them_resets_them() {
        let (accept_tx, mut accept) = mpsc::unbounded_channel();
        let (raw_tx, mut raw) = mpsc::unbounded_channel();
        let (home, mut home_out) = Mux::new(None);
        let (host, mut host_out) = Mux::with_raw(Some(accept_tx), Some(raw_tx));
        let (h, s) = (home.clone(), host.clone());
        tokio::spawn(async move {
            while let Some(f) = home_out.recv().await {
                s.handle(&f).unwrap();
            }
        });
        tokio::spawn(async move {
            while let Some(f) = host_out.recv().await {
                h.handle(&f).unwrap();
            }
        });
        let mut a = home.open_raw(b"ssh-guest").unwrap();
        let (kind, mut b) = raw.recv().await.unwrap();
        assert_eq!(kind, b"ssh-guest");
        a.write_all(b"SSH-2.0-x\r\n").await.unwrap();
        let mut buf = [0u8; 11];
        b.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"SSH-2.0-x\r\n");
        // Noise streams still go to `accept`.
        let _n = home.open().unwrap();
        assert!(accept.recv().await.is_some());
        assert!(raw.try_recv().is_err());

        // An end that takes no raw streams resets them.
        let (accept_tx, _accept) = mpsc::unbounded_channel();
        let (old, mut old_out) = Mux::new(Some(accept_tx));
        old.handle(&frame(OPEN, 3, b"ssh-guest")).unwrap();
        assert_eq!(old_out.recv().await.unwrap(), frame(RESET, 3, &[]));
        assert_eq!(old.streams(), 0);
    }

    #[tokio::test]
    async fn closing_the_tunnel_ends_every_stream() {
        let (home, _host, mut accept) = pair();
        let mut a = home.open().unwrap();
        let _b = accept.recv().await.unwrap();
        home.close();
        let mut buf = Vec::new();
        // Reads end, and writes stop rather than buffer forever.
        let r = tokio::time::timeout(std::time::Duration::from_secs(2), a.read_to_end(&mut buf)).await;
        assert!(r.is_ok());
    }
}
