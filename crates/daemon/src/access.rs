//! Who may talk to the daemon.
//!
//! The daemon usually listens on loopback and the tailnet reaches it
//! through `tailscale serve`, which adds `Tailscale-User-Login` and strips
//! any copy a client sends. It can also be reached directly: on a tailnet
//! address, or (in a sandbox, where tailscaled runs in userspace) through
//! the netstack forwarding a tailnet connection to loopback. For those,
//! tailscaled says who is connecting (WhoIs, `tailscale.rs`). So:
//!
//! - **Host** must be a loopback name or a tailnet name. This stops DNS
//!   rebinding: a web page that rebinds its own name to our address still
//!   sends its own name as Host.
//! - **Origin**, when a browser sends one, must be exactly one of ours: the
//!   page's own origin (`http://` for loopback, `https://` for the tailnet
//!   name, which serve terminates TLS for), or one given with
//!   `--allow-origin`. That is how another daemon's page (the home daemon,
//!   whose host list this daemon is on) may connect here: by exact origin,
//!   scheme and port included, never a pattern. This stops any other page
//!   in a browser from opening a WebSocket to us (cross-site WebSocket
//!   hijacking) or posting to the API.
//! - **Identity**: a connection tailscaled knows about must come from the
//!   owner's login; tagged nodes (sandboxes, servers) have no login and are
//!   refused. Otherwise the connection is from this machine: a
//!   `Tailscale-User-Login` header (added by serve) must be the owner, and a
//!   request for a tailnet name without one came through serve from a node
//!   with no user (a tagged node, or Funnel), so it is refused too. Local
//!   processes could forge the header, but they could equally use the Unix
//!   socket; its job is keeping other tailnet users and nodes out.
//! - **A resident daemon in a sandbox** (M4b) is reached through its
//!   provider's proxy, which arrives on loopback like a local process.
//!   There, everything not identified by tailscaled must carry the token
//!   the home daemon minted for this host (`Authorization: Bearer …`): the
//!   home daemon checked the owner on its side, and the token says so. Only
//!   its SHA-256 is kept here, so a process in the sandbox reading our
//!   arguments or files learns nothing it can use.

use std::{collections::HashSet, net::SocketAddr};

use axum::http::{HeaderMap, StatusCode, header};
use sha2::{Digest, Sha256};

/// Who is on the other end of a TCP connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Peer {
    /// A process on this machine.
    Local,
    /// A tailnet node, per tailscaled. `login` is its user's; `None` for a
    /// tagged node, which no user owns.
    Tailnet { login: Option<String> },
    /// Neither: not loopback, and tailscaled doesn't know it.
    Other,
}

type Refusal = (StatusCode, String);

#[derive(Debug, Clone)]
pub struct Access {
    hosts: HashSet<String>,
    /// Tailnet names (no port): requests for these come through serve.
    public: HashSet<String>,
    /// Exact `scheme://host[:port]` values accepted as WebSocket origins.
    origins: HashSet<String>,
    owner: Option<String>,
    /// SHA-256 of the tunnel token, when loopback connections need it.
    tunnel: Option<[u8; 32]>,
}

impl Access {
    /// `direct`: names and addresses this daemon is also reachable at
    /// without serve, on its own port (it listens on one, or tailscaled's
    /// netstack forwards the port to loopback). Their pages are plain http.
    pub fn new(
        port: u16,
        public_hosts: &[String],
        direct: &[String],
        extra_origins: &[String],
        owner: Option<String>,
    ) -> Self {
        let loopback: Vec<String> = ["127.0.0.1", "localhost", "[::1]"].iter().map(|h| format!("{h}:{port}")).collect();
        let public: Vec<String> = public_hosts.iter().map(|h| h.to_ascii_lowercase()).collect();
        // With a port: a direct connection (not through serve) names one.
        let direct: Vec<String> = direct.iter().map(|h| format!("{}:{port}", h.to_ascii_lowercase())).collect();
        let origins = loopback
            .iter()
            .chain(&direct)
            .map(|h| format!("http://{h}"))
            .chain(public.iter().map(|h| format!("https://{h}")))
            .chain(extra_origins.iter().map(|o| o.trim_end_matches('/').to_ascii_lowercase()))
            .collect();
        let hosts = loopback.into_iter().chain(public.iter().cloned()).chain(direct).collect();
        Self { hosts, public: public.into_iter().collect(), origins, owner, tunnel: None }
    }

    /// From now on, connections from this machine need the token whose
    /// SHA-256 this is (hex): a resident daemon reached through its
    /// provider's tunnel.
    pub fn require_tunnel_token(mut self, sha256_hex: &str) -> anyhow::Result<Self> {
        let hex = sha256_hex.trim();
        if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            anyhow::bail!("a tunnel token's SHA-256 is 64 hex digits");
        }
        let mut d = [0u8; 32];
        for (i, b) in d.iter_mut().enumerate() {
            *b = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16)?;
        }
        self.tunnel = Some(d);
        Ok(self)
    }

    /// The app's own origins (the pages allowed to frame a block).
    pub fn origins(&self) -> Vec<String> {
        let mut o: Vec<String> = self.origins.iter().cloned().collect();
        o.sort();
        o
    }

    /// Every request: the Host check and the identity check (the server
    /// runs them separately, for the join exception).
    #[cfg(test)]
    pub fn check(&self, headers: &HeaderMap, peer: &Peer) -> Result<(), Refusal> {
        self.check_host(headers)?;
        self.check_identity(headers, peer)
    }

    pub fn check_host(&self, headers: &HeaderMap) -> Result<(), Refusal> {
        let host = host(headers);
        if !self.hosts.contains(&host) {
            return Err((StatusCode::MISDIRECTED_REQUEST, format!("unknown host {host:?}")));
        }
        Ok(())
    }

    pub fn check_identity(&self, headers: &HeaderMap, peer: &Peer) -> Result<(), Refusal> {
        match peer {
            Peer::Tailnet { login: Some(login) } => self.must_be_owner(login),
            Peer::Tailnet { login: None } => {
                Err((StatusCode::FORBIDDEN, "tagged tailnet nodes have no user identity".into()))
            }
            Peer::Other => Err((StatusCode::FORBIDDEN, "not from this machine or the tailnet".into())),
            Peer::Local if self.tunnel.is_some() => self.check_tunnel_token(headers),
            Peer::Local => match header_str(headers, "tailscale-user-login") {
                Some(login) => self.must_be_owner(login),
                None if self.public.contains(&host(headers)) => {
                    Err((StatusCode::FORBIDDEN, "tailnet request without a user identity (from a tagged node?)".into()))
                }
                None => Ok(()),
            },
        }
    }

    fn check_tunnel_token(&self, headers: &HeaderMap) -> Result<(), Refusal> {
        let given = header_str(headers, header::AUTHORIZATION.as_str()).and_then(|v| v.strip_prefix("Bearer "));
        let (Some(want), Some(given)) = (&self.tunnel, given) else {
            return Err((StatusCode::UNAUTHORIZED, "this daemon is reached through its home daemon's tunnel".into()));
        };
        let got: [u8; 32] = Sha256::digest(given.trim().as_bytes()).into();
        // Constant time: no early exit on the first differing byte.
        if got.iter().zip(want).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0 {
            Ok(())
        } else {
            Err((StatusCode::UNAUTHORIZED, "wrong tunnel token".into()))
        }
    }

    fn must_be_owner(&self, login: &str) -> Result<(), Refusal> {
        match &self.owner {
            Some(owner) if owner.eq_ignore_ascii_case(login) => Ok(()),
            Some(_) => Err((StatusCode::FORBIDDEN, format!("{login} is not the owner"))),
            None => Err((
                StatusCode::FORBIDDEN,
                "tailnet request but no owner configured; start illogicald with --owner".into(),
            )),
        }
    }

    /// The login let in from the tailnet.
    pub fn owner(&self) -> Option<&str> {
        self.owner.as_deref()
    }

    /// Whether a browser on `origin` may use us (exactly one of ours).
    pub fn origin_allowed(&self, origin: &str) -> bool {
        self.origins.contains(&origin.trim_end_matches('/').to_ascii_lowercase())
    }

    /// Extra check for WebSocket upgrades and API calls.
    pub fn check_origin(&self, headers: &HeaderMap) -> Result<(), Refusal> {
        let Some(origin) = header_str(headers, header::ORIGIN.as_str()) else {
            // Not a browser (CLI, tests). Browsers always send Origin on WS.
            return Ok(());
        };
        if self.origin_allowed(origin) {
            Ok(())
        } else {
            Err((StatusCode::FORBIDDEN, format!("origin {} not allowed", origin.to_ascii_lowercase())))
        }
    }
}

fn host(headers: &HeaderMap) -> String {
    header_str(headers, header::HOST.as_str()).unwrap_or_default().to_ascii_lowercase()
}

fn header_str<'h>(headers: &'h HeaderMap, name: &str) -> Option<&'h str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// The listen address as a Host value, when it is reachable from elsewhere
/// (not loopback, not "every address").
pub fn direct_address(listen: SocketAddr) -> Option<String> {
    let ip = listen.ip();
    (!ip.is_loopback() && !ip.is_unspecified()).then(|| host_name(ip))
}

/// An address as it appears in a Host header (IPv6 in brackets).
pub fn host_name(ip: std::net::IpAddr) -> String {
    match ip {
        std::net::IpAddr::V4(v4) => v4.to_string(),
        std::net::IpAddr::V6(v6) => format!("[{v6}]"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(), v.parse().unwrap());
        }
        h
    }

    fn access() -> Access {
        Access::new(
            7681,
            &["geek.example.ts.net".into()],
            &[],
            &["http://localhost:5173".into(), "https://home.example.ts.net".into()],
            Some("me@x.com".into()),
        )
    }

    fn check(a: &Access, pairs: &[(&str, &str)]) -> Result<(), Refusal> {
        a.check(&headers(pairs), &Peer::Local)
    }

    #[test]
    fn host_must_be_known() {
        let a = access();
        let me = ("tailscale-user-login", "me@x.com");
        assert!(check(&a, &[("host", "127.0.0.1:7681")]).is_ok());
        assert!(check(&a, &[("host", "geek.example.ts.net"), me]).is_ok());
        assert!(check(&a, &[("host", "evil.com")]).is_err());
        assert!(check(&a, &[("host", "127.0.0.1:9999")]).is_err());
        assert!(check(&a, &[]).is_err());
    }

    #[test]
    fn tailnet_identity_must_be_owner() {
        let a = access();
        let ok = [("host", "geek.example.ts.net"), ("tailscale-user-login", "ME@x.com")];
        assert!(check(&a, &ok).is_ok());
        let other = [("host", "geek.example.ts.net"), ("tailscale-user-login", "friend@x.com")];
        assert_eq!(check(&a, &other).unwrap_err().0, StatusCode::FORBIDDEN);
        let no_owner = Access::new(7681, &["geek.example.ts.net".into()], &[], &[], None);
        assert!(check(&no_owner, &ok).is_err());
    }

    #[test]
    fn serve_requests_without_a_user_are_refused() {
        // Tagged nodes (and Funnel) come through serve with no login header.
        let a = access();
        let tagged = [("host", "geek.example.ts.net")];
        assert_eq!(check(&a, &tagged).unwrap_err().0, StatusCode::FORBIDDEN);
        // Loopback names are local requests: no identity needed.
        assert!(check(&a, &[("host", "localhost:7681")]).is_ok());
    }

    #[test]
    fn direct_tailnet_peers_need_the_owners_login() {
        // Reachable on its own port (a sandbox's netstack forwards it).
        let a = Access::new(
            7681,
            &["geek.example.ts.net".into()],
            &["geek.example.ts.net".into()],
            &[],
            Some("me@x.com".into()),
        );
        let h = headers(&[("host", "geek.example.ts.net:7681")]);
        let me = Peer::Tailnet { login: Some("me@x.com".into()) };
        assert!(a.check(&h, &me).is_ok());
        let friend = Peer::Tailnet { login: Some("friend@x.com".into()) };
        assert_eq!(a.check(&h, &friend).unwrap_err().0, StatusCode::FORBIDDEN);
        assert_eq!(a.check(&h, &Peer::Tailnet { login: None }).unwrap_err().0, StatusCode::FORBIDDEN);
        assert_eq!(a.check(&h, &Peer::Other).unwrap_err().0, StatusCode::FORBIDDEN);
        // A forged header can't stand in for what tailscaled says.
        let forged = headers(&[("host", "geek.example.ts.net:7681"), ("tailscale-user-login", "me@x.com")]);
        assert!(a.check(&forged, &Peer::Tailnet { login: None }).is_err());
        assert!(a.check(&forged, &friend).is_err());
        // Nor a loopback Host (a netstack-forwarded connection can claim one).
        let loopback = headers(&[("host", "127.0.0.1:7681")]);
        assert!(a.check(&loopback, &Peer::Tailnet { login: None }).is_err());
        assert!(a.check(&loopback, &Peer::Other).is_err());
        // Even the owner needs a Host of ours (DNS rebinding).
        assert!(a.check(&headers(&[("host", "evil.com:7681")]), &me).is_err());
    }

    #[test]
    fn direct_addresses_are_hosts_and_their_pages_origins() {
        let direct = ["box.example.ts.net".into(), "100.1.2.3".into()];
        let a = Access::new(7681, &["box.example.ts.net".into()], &direct, &[], Some("me@x.com".into()));
        let me = Peer::Tailnet { login: Some("me@x.com".into()) };
        assert!(a.check(&headers(&[("host", "100.1.2.3:7681")]), &me).is_ok());
        assert!(a.check(&headers(&[("host", "box.example.ts.net:7681")]), &me).is_ok());
        assert!(a.check(&headers(&[("host", "100.1.2.3")]), &me).is_err());
        assert!(a.check(&headers(&[("host", "100.1.2.4:7681")]), &me).is_err());
        assert!(a.origin_allowed("http://100.1.2.3:7681"));
        assert!(a.origin_allowed("http://box.example.ts.net:7681"));
        assert!(!a.origin_allowed("https://100.1.2.3:7681"));
        assert!(!a.origin_allowed("http://box.example.ts.net"));
        // Without direct reach (serve only), none of that is ours.
        let served = access();
        assert!(served.check(&headers(&[("host", "geek.example.ts.net:7681")]), &me).is_err());
        assert!(!served.origin_allowed("http://geek.example.ts.net:7681"));
        assert_eq!(direct_address("100.1.2.3:7681".parse().unwrap()).as_deref(), Some("100.1.2.3"));
        assert_eq!(direct_address("127.0.0.1:7681".parse().unwrap()), None);
        assert_eq!(direct_address("0.0.0.0:7681".parse().unwrap()), None);
    }

    #[test]
    fn tunnelled_connections_need_the_token() {
        let token = "ilt_secret";
        let digest: String = Sha256::digest(token.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
        let a = Access::new(7681, &[], &[], &[], None).require_tunnel_token(&digest).unwrap();
        let local = [("host", "127.0.0.1:7681")];
        assert_eq!(check(&a, &local).unwrap_err().0, StatusCode::UNAUTHORIZED, "no token");
        let bearer = format!("Bearer {token}");
        assert!(check(&a, &[local[0], ("authorization", &bearer)]).is_ok());
        assert!(check(&a, &[local[0], ("authorization", "Bearer ilt_wrong")]).is_err());
        assert!(check(&a, &[local[0], ("authorization", token)]).is_err(), "not a bearer");
        // A forged serve header is no substitute.
        assert!(check(&a, &[local[0], ("tailscale-user-login", "me@x.com")]).is_err());
        // The Host check still applies.
        assert!(check(&a, &[("host", "evil.com"), ("authorization", &bearer)]).is_err());
        assert!(Access::new(7681, &[], &[], &[], None).require_tunnel_token("abc").is_err());
    }

    #[test]
    fn websocket_origin() {
        let a = access();
        assert!(a.check_origin(&headers(&[])).is_ok());
        assert!(a.check_origin(&headers(&[("origin", "https://geek.example.ts.net")])).is_ok());
        assert!(a.check_origin(&headers(&[("origin", "http://127.0.0.1:7681")])).is_ok());
        assert!(a.check_origin(&headers(&[("origin", "http://localhost:5173")])).is_ok());
        assert!(a.check_origin(&headers(&[("origin", "https://evil.com")])).is_err());
        assert!(a.check_origin(&headers(&[("origin", "null")])).is_err());
    }

    #[test]
    fn websocket_origin_scheme_and_port_must_match() {
        let a = access();
        // serve terminates TLS, so the tailnet page is only ever https.
        assert!(a.check_origin(&headers(&[("origin", "http://geek.example.ts.net")])).is_err());
        // Loopback is plain http.
        assert!(a.check_origin(&headers(&[("origin", "https://127.0.0.1:7681")])).is_err());
        // Another port on the same name is another origin (e.g. a proxied dev server).
        assert!(a.check_origin(&headers(&[("origin", "https://geek.example.ts.net:10000")])).is_err());
        assert!(a.check_origin(&headers(&[("origin", "https://localhost:5173")])).is_err());
    }

    #[test]
    fn the_home_daemons_origin_is_exact() {
        let a = access();
        assert!(a.origin_allowed("https://home.example.ts.net"));
        assert!(a.origin_allowed("https://home.example.ts.net/"));
        assert!(!a.origin_allowed("http://home.example.ts.net"));
        assert!(!a.origin_allowed("https://home.example.ts.net:8443"));
        assert!(!a.origin_allowed("https://evil.home.example.ts.net"));
        assert!(!a.origin_allowed("https://home.example.ts.net.evil.com"));
    }
}
