//! Reaching a TCP port on a machine: one of this host's, directly, or one
//! of a sprite's, through the Sprites proxy. Or a server Arugula runs
//! itself (an editor block's code-server, M27), which it starts if needed.
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

use futures_util::future::BoxFuture;

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
    /// A server of Arugula's own, reached however it says.
    Service(Arc<dyn Service>),
}

/// A server Arugula runs itself: dialing it starts it if it isn't
/// running (it stops itself when idle).
pub trait Service: Send + Sync {
    fn dial(&self) -> BoxFuture<'static, io::Result<Conn>>;
    /// What to call it in errors.
    fn name(&self) -> String;
}

impl std::fmt::Debug for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Local(p) => write!(f, "localhost:{p}"),
            Self::Sprite { sprite, port, .. } => write!(f, "{sprite}:{port}"),
            Self::Service(s) => write!(f, "{}", s.name()),
        }
    }
}

impl PartialEq for Target {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Local(a), Self::Local(b)) => a == b,
            (Self::Sprite { sprite: a, port: p, .. }, Self::Sprite { sprite: b, port: q, .. }) => a == b && p == q,
            (Self::Service(a), Self::Service(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

impl Target {
    /// The `Host` its server is asked for: `localhost:<port>`, or
    /// `localhost` for a service.
    pub fn authority(&self) -> String {
        match self {
            Self::Local(p) | Self::Sprite { port: p, .. } => format!("localhost:{p}"),
            Self::Service(_) => "localhost".into(),
        }
    }

    /// What to call it in errors: `port 5173`, or the service's name.
    pub fn what(&self) -> String {
        match self {
            Self::Local(p) | Self::Sprite { port: p, .. } => format!("port {p}"),
            Self::Service(s) => s.name(),
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
                let s = first(&both, tokio::net::TcpStream::connect).await?;
                s.set_nodelay(true)?;
                Ok(Box::new(s))
            }
            Self::Sprite { provider, sprite, port } => provider.dial(sprite, *port).await,
            Self::Service(s) => s.dial().await,
        }
    }
}

/// The first of `addrs` that `connect` reaches; if none, the first one's
/// error. Not the last's, as `TcpStream::connect(&[..])` gives: on macOS
/// `[::1]` for a free port sometimes fails with EINVAL where `127.0.0.1`
/// says refused (#133).
async fn first<T, F, Fut>(addrs: &[SocketAddr], connect: F) -> io::Result<T>
where
    F: Fn(SocketAddr) -> Fut,
    Fut: Future<Output = io::Result<T>>,
{
    let mut err = None;
    for a in addrs {
        match connect(*a).await {
            Ok(s) => return Ok(s),
            Err(e) => {
                err.get_or_insert(e);
            }
        }
    }
    Err(err.unwrap_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no addresses to dial")))
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

    /// Both fail: the v4 error, whatever v6 said (#133's EINVAL from `::1`).
    #[tokio::test]
    async fn when_both_fail_the_first_error_is_the_one() {
        let both = [SocketAddr::from((Ipv4Addr::LOCALHOST, 1)), SocketAddr::from((Ipv6Addr::LOCALHOST, 1))];
        let e = first(&both, |a| async move {
            Err::<(), _>(io::Error::from(if a.is_ipv4() {
                io::ErrorKind::ConnectionRefused
            } else {
                io::ErrorKind::InvalidInput
            }))
        })
        .await
        .unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::ConnectionRefused);

        // The second answering is enough.
        let got = first(&both, |a| async move {
            if a.is_ipv4() { Err(io::Error::from(io::ErrorKind::ConnectionRefused)) } else { Ok(6) }
        })
        .await
        .unwrap();
        assert_eq!(got, 6);
    }
}
