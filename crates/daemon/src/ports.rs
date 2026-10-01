//! Reaching a TCP port on a machine: one of this host's, directly, or one
//! of a sprite's, through the Sprites proxy.
//!
//! A machine's port is dialed through its provider (`Provider::dial`; for
//! sprites, one Sprites proxy WebSocket per TCP connection, which wakes the
//! sprite and keeps it awake while open).

use std::{
    io,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};

pub use crate::provider::Conn;
use crate::provider::Provider;

/// Opening a connection takes at most this long (a sprite may be waking).
const DIAL_TIMEOUT: Duration = Duration::from_secs(30);

/// A port on a machine.
#[derive(Clone)]
pub enum Target {
    /// This host's, on loopback.
    Local(u16),
    /// A machine's, through its provider.
    Sprite { provider: Arc<dyn Provider>, sprite: String, port: u16 },
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
            Self::Sprite { provider, sprite, port } => provider.dial(sprite, *port).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

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
