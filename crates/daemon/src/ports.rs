//! Reaching a TCP port on a machine: one of this host's, directly, or one
//! of a sprite's, through the Sprites proxy.
//!
//! The Sprites proxy (`GET /v1/sprites/{name}/proxy`, a WebSocket) carries
//! one TCP connection:
//!
//! 1. we send `{"host": "localhost", "port": N}` as a text frame;
//! 2. it dials that port inside the sprite (waking it if needed) and answers
//!    `{"status": "connected", "target": "localhost:N"}`, or
//!    `{"status": "error", "error": "…"}` and closes;
//! 3. then binary frames carry the bytes both ways, until either end closes.
//!
//! The socket keeps the sprite awake while it's open. This is the "dial a
//! port" half of M4b's `Provider`; [`Target`] is what a provider will hand
//! back.

use std::{
    io,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio_tungstenite::tungstenite::Message;

use crate::machine::Wisp;

/// Opening a connection takes at most this long (a sprite may be waking).
const DIAL_TIMEOUT: Duration = Duration::from_secs(20);

/// A byte stream to a port, whichever way it was reached.
pub trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

pub type Conn = Box<dyn Io>;

/// A port on a machine.
#[derive(Clone)]
pub enum Target {
    /// This host's, on loopback.
    Local(u16),
    /// A sprite's, through the Sprites proxy.
    Sprite { wisp: Arc<Wisp>, sprite: String, port: u16 },
}

impl std::fmt::Debug for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Local(p) => write!(f, "localhost:{p}"),
            Self::Sprite { sprite, port, .. } => write!(f, "{sprite}:{port}"),
        }
    }
}

impl PartialEq for Target {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Local(a), Self::Local(b)) => a == b,
            (Self::Sprite { sprite: a, port: p, .. }, Self::Sprite { sprite: b, port: q, .. }) => a == b && p == q,
            _ => false,
        }
    }
}

impl Target {
    pub fn port(&self) -> u16 {
        match self {
            Self::Local(p) | Self::Sprite { port: p, .. } => *p,
        }
    }

    /// Open a new connection to the port.
    pub async fn dial(&self) -> io::Result<Conn> {
        match tokio::time::timeout(DIAL_TIMEOUT, self.dial_now()).await {
            Ok(r) => r,
            Err(_) => Err(io::Error::new(io::ErrorKind::TimedOut, format!("{self:?} didn't answer"))),
        }
    }

    async fn dial_now(&self) -> io::Result<Conn> {
        match self {
            Self::Local(port) => {
                // Both loopback addresses in turn: a dev server listening
                // on "localhost" may be on either one only.
                let both =
                    [SocketAddr::from((Ipv4Addr::LOCALHOST, *port)), SocketAddr::from((Ipv6Addr::LOCALHOST, *port))];
                let s = tokio::net::TcpStream::connect(&both[..]).await?;
                s.set_nodelay(true)?;
                Ok(Box::new(s))
            }
            Self::Sprite { wisp, sprite, port } => dial_sprite(wisp, sprite, *port).await,
        }
    }
}

async fn dial_sprite(wisp: &Wisp, sprite: &str, port: u16) -> io::Result<Conn> {
    let req = wisp.proxy_request(sprite).map_err(io::Error::other)?;
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.map_err(|e| match e {
        tokio_tungstenite::tungstenite::Error::Http(r) if r.status() == 404 => {
            io::Error::new(io::ErrorKind::NotFound, format!("machine {sprite} is gone"))
        }
        e => io::Error::other(format!("Sprites proxy: {e}")),
    })?;
    let hello = serde_json::json!({ "host": "localhost", "port": port }).to_string();
    ws.send(Message::Text(hello.into())).await.map_err(io::Error::other)?;
    // The first answer says whether the port took the connection.
    loop {
        match ws.next().await {
            Some(Ok(Message::Text(t))) => {
                let v: serde_json::Value = serde_json::from_str(&t).unwrap_or_default();
                match v["status"].as_str() {
                    Some("connected") => break,
                    _ => {
                        let why = v["error"].as_str().unwrap_or("refused").to_owned();
                        let kind = if why.contains("refused") {
                            io::ErrorKind::ConnectionRefused
                        } else {
                            io::ErrorKind::Other
                        };
                        return Err(io::Error::new(kind, format!("nothing is answering on port {port}: {why}")));
                    }
                }
            }
            Some(Ok(Message::Close(_))) | None => {
                return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "the Sprites proxy closed"));
            }
            Some(Err(e)) => return Err(io::Error::other(e)),
            Some(Ok(_)) => {}
        }
    }
    // Hand back one end of a pipe; a task moves bytes between the other end
    // and the socket. The pipe's buffer is the backpressure.
    let (ours, theirs) = tokio::io::duplex(64 * 1024);
    tokio::spawn(async move {
        let (mut from_us, mut to_us) = tokio::io::split(theirs);
        let (mut sink, mut stream) = ws.split();
        let up = async {
            let mut buf = vec![0u8; 32 * 1024];
            loop {
                match from_us.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if sink.send(Message::Binary(buf[..n].to_vec().into())).await.is_err() {
                            break;
                        }
                    }
                }
            }
            let _ = sink.close().await;
        };
        let down = async {
            while let Some(m) = stream.next().await {
                match m {
                    Ok(Message::Binary(b)) => {
                        if to_us.write_all(&b).await.is_err() {
                            break;
                        }
                    }
                    Ok(Message::Close(_)) | Err(_) => break,
                    Ok(_) => {}
                }
            }
            let _ = to_us.shutdown().await;
        };
        tokio::pin!(up, down);
        tokio::select! {
            // We're done sending: let the answer finish, briefly.
            _ = &mut up => { let _ = tokio::time::timeout(Duration::from_secs(5), down).await; }
            // The port closed: so does our end.
            _ = &mut down => {}
        }
    });
    Ok(Box::new(ours))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_ports_dial_and_refuse() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            s.write_all(b"hi").await.unwrap();
        });
        let mut c = Target::Local(port).dial().await.unwrap();
        let mut got = String::new();
        c.read_to_string(&mut got).await.unwrap();
        assert_eq!(got, "hi");

        // Listening on ::1 only, as a dev server bound to "localhost" may.
        if let Ok(l6) = tokio::net::TcpListener::bind("[::1]:0").await {
            let port = l6.local_addr().unwrap().port();
            tokio::spawn(async move {
                let _ = l6.accept().await;
            });
            assert!(Target::Local(port).dial().await.is_ok(), "::1 only");
        }

        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let e = Target::Local(free).dial().await.err().unwrap();
        assert_eq!(e.kind(), io::ErrorKind::ConnectionRefused);
    }
}
