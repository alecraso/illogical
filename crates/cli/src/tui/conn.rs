//! The TUI's link to the daemon: one WebSocket (the web client's protocol)
//! read without blocking from the main loop, and HTTP API calls made on
//! their own threads so a slow answer never stalls drawing.

use std::{io::ErrorKind, sync::mpsc, thread};

use anyhow::Context;
use arugula_proto::{ClientMsg, Frame, FrameKind, PaneId, ServerMsg};
use serde_json::Value;
use tungstenite::{Message, WebSocket};

use crate::http::{Stream, Target, request};

pub use crate::wake::{Wake, Waker};

/// What came in from the daemon.
pub enum In {
    Msg(Box<ServerMsg>),
    Frame(Frame),
    Closed,
}

pub struct Conn {
    ws: WebSocket<Box<dyn Stream>>,
    target: Target,
    /// API answers that went wrong, to show.
    errors: mpsc::Sender<String>,
    waker: Waker,
}

impl Conn {
    /// Connect and wait for the hello.
    pub fn open(target: &Target, errors: mpsc::Sender<String>, waker: Waker) -> anyhow::Result<(Self, ServerMsg)> {
        let stream = target.connect()?;
        let (mut ws, _) = tungstenite::client(target.ws_request()?, stream).map_err(|e| match e {
            tungstenite::HandshakeError::Failure(e) => anyhow::Error::from(e).context("websocket handshake"),
            tungstenite::HandshakeError::Interrupted(_) => anyhow::anyhow!("websocket handshake interrupted"),
        })?;
        let hello = loop {
            match ws.read().context("waiting for the daemon's hello")? {
                Message::Text(t) => {
                    if let Ok(m @ ServerMsg::Hello { .. }) = serde_json::from_str(&t) {
                        break m;
                    }
                }
                Message::Close(_) => anyhow::bail!("the daemon closed the connection"),
                _ => {}
            }
        };
        ws.get_mut().set_nonblocking(true)?;
        Ok((Self { ws, target: target.clone(), errors, waker }, hello))
    }

    /// The connection, for [`Wake::wait`].
    pub fn stream(&self) -> &dyn Stream {
        self.ws.get_ref().as_ref()
    }

    pub fn send(&mut self, msg: &ClientMsg) {
        let text = serde_json::to_string(msg).expect("client messages serialize");
        self.write(Message::Text(text.into()));
    }

    pub fn input(&mut self, pane: PaneId, data: Vec<u8>) {
        if data.is_empty() {
            return;
        }
        let f = Frame { kind: FrameKind::Input, pane, offset: 0, data };
        self.write(Message::Binary(f.encode().into()));
    }

    fn write(&mut self, m: Message) {
        // Non-blocking: what doesn't go now stays queued for `flush`.
        match self.ws.send(m) {
            Ok(()) => {}
            Err(tungstenite::Error::Io(e)) if e.kind() == ErrorKind::WouldBlock => {}
            Err(e) => {
                let _ = self.errors.send(format!("connection: {e}"));
            }
        }
    }

    pub fn flush(&mut self) -> anyhow::Result<()> {
        match self.ws.flush() {
            Err(tungstenite::Error::Io(e)) if e.kind() == ErrorKind::WouldBlock => Ok(()),
            r => Ok(r?),
        }
    }

    /// Everything that has arrived, without waiting.
    pub fn read(&mut self, mut f: impl FnMut(In)) -> anyhow::Result<()> {
        loop {
            match self.ws.read() {
                Ok(Message::Binary(b)) => {
                    if let Ok(frame) = Frame::decode(&b) {
                        f(In::Frame(frame));
                    }
                }
                Ok(Message::Text(t)) => match serde_json::from_str::<ServerMsg>(&t) {
                    Ok(m) => f(In::Msg(Box::new(m))),
                    Err(_) => continue,
                },
                Ok(Message::Close(_)) => {
                    f(In::Closed);
                    return Ok(());
                }
                Ok(_) => {}
                Err(tungstenite::Error::Io(e)) if e.kind() == ErrorKind::WouldBlock => return Ok(()),
                Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                    f(In::Closed);
                    return Ok(());
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// `GET` from the daemon's API in the background: `done` has the body
    /// (or what went wrong), and the main loop is woken after it.
    pub fn get(&self, path: String, done: impl FnOnce(anyhow::Result<Vec<u8>>) + Send + 'static) {
        let (target, waker) = (self.target.clone(), self.waker.clone());
        thread::spawn(move || {
            done(request(&target, "GET", &path, None).and_then(|r| r.ok()).and_then(|r| r.bytes()));
            waker.wake();
        });
    }

    /// `POST` to the daemon's API in the background; a failure comes back as
    /// a message to show, saying what didn't work (`what`).
    pub fn api(&self, path: String, body: Value, what: &'static str) {
        let (target, errors, waker) = (self.target.clone(), self.errors.clone(), self.waker.clone());
        thread::spawn(move || {
            let r = request(&target, "POST", &path, Some(&body)).and_then(|r| r.ok());
            if let Err(e) = r {
                let _ = errors.send(format!("couldn't {what}: {e:#}"));
                waker.wake();
            }
        });
    }
}
