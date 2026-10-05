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
//!   with no user (a tagged node, or Funnel), so it is refused too. The
//!   header is only believed from tailscaled's end of the connection
//!   (`localauth::serve_peer_ok`, checked by the server).
//! - **Everything else on loopback** must show the local token
//!   (`localauth.rs`): `Authorization: Bearer …`, or the cookie its
//!   sign-in link sets. Loopback is every account's and every program's on
//!   the machine, so being on it proves nothing.
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

use crate::acl::Principal;

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
    /// This daemon's page, for share links.
    page: String,
    /// This node's MagicDNS name, when tailscaled told us (#109): where
    /// `tailscale serve` puts the app.
    tailnet: Option<String>,
    /// The port it listens on (what `tailscale serve` points at).
    port: u16,
    /// The local token, when loopback callers need it (`localauth.rs`).
    local: Option<LocalToken>,
}

#[derive(Clone)]
struct LocalToken {
    /// Its SHA-256: compared without early exits.
    digest: [u8; 32],
    /// The page the sign-in link is on (`http://127.0.0.1:PORT`).
    page: String,
    token: String,
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
        let page = match public.first() {
            Some(name) => format!("https://{name}"),
            None => format!("http://127.0.0.1:{port}"),
        };
        let hosts = loopback.into_iter().chain(public.iter().cloned()).chain(direct).collect();
        Self {
            hosts,
            public: public.into_iter().collect(),
            origins,
            owner,
            page,
            tunnel: None,
            tailnet: None,
            port,
            local: None,
        }
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// This node's MagicDNS name, from tailscaled.
    pub fn with_tailnet_name(mut self, name: &str) -> Self {
        self.tailnet = Some(name.to_ascii_lowercase());
        self
    }

    /// The app's address on the tailnet once `tailscale serve` is on, for
    /// the phone (#109): `https://NAME.TAILNET.ts.net`.
    pub fn tailnet_url(&self) -> Option<String> {
        self.tailnet.as_ref().map(|n| format!("https://{n}"))
    }

    /// Whether this request came through `tailscale serve` (or straight
    /// from a tailnet node) rather than from this machine.
    pub fn via_tailnet(&self, headers: &HeaderMap, peer: &Peer) -> bool {
        matches!(peer, Peer::Tailnet { .. }) || header_str(headers, "tailscale-user-login").is_some()
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

    /// From now on, loopback callers that tailscaled didn't vouch for need
    /// `token` (`localauth.rs`). `listen`: where the sign-in link goes.
    pub fn require_local_token(mut self, token: &str, listen: SocketAddr) -> Self {
        let ip = listen.ip();
        let host = if ip.is_unspecified() || ip.is_loopback() {
            if ip.is_ipv6() && !ip.is_unspecified() { "[::1]".to_owned() } else { "127.0.0.1".to_owned() }
        } else {
            host_name(ip)
        };
        let page = format!("http://{host}:{}", listen.port());
        let token = token.trim().to_owned();
        self.local = Some(LocalToken { digest: Sha256::digest(token.as_bytes()).into(), page, token });
        self
    }

    /// Whether a resident's tunnel token is what loopback callers need.
    pub fn tunnelled(&self) -> bool {
        self.tunnel.is_some()
    }

    /// The cookie a sign-in link sets: one per port, so daemons side by
    /// side (each with its own token) don't sign each other out.
    pub fn cookie_name(&self) -> String {
        format!("illogical_{}", self.port)
    }

    /// The sign-in link for a browser on this machine.
    pub fn signin_link(&self) -> Option<String> {
        let l = self.local.as_ref()?;
        Some(format!("{}{}?token={}", l.page, crate::localauth::AUTH_PATH, l.token))
    }

    /// `given` is the local token.
    pub fn is_local_token(&self, given: &str) -> bool {
        self.local.as_ref().is_some_and(|l| same(&l.digest, given))
    }

    /// The request names the local token as its bearer (for `/mcp`, where a
    /// bearer otherwise means an MCP token).
    pub fn local_bearer(&self, headers: &HeaderMap) -> bool {
        bearer(headers).is_some_and(|t| self.is_local_token(t))
    }

    /// A loopback caller shows the local token: as its bearer, or as a
    /// cookie from a browser. A browser's cookie counts on our own pages,
    /// on page loads, and for one of our origins; not for what another
    /// site's page embeds (an image or a script, which carry no Origin).
    fn check_local(&self, headers: &HeaderMap) -> Result<(), Refusal> {
        let Some(l) = &self.local else { return Ok(()) };
        if bearer(headers).is_some_and(|t| same(&l.digest, t)) {
            return Ok(());
        }
        let site = header_str(headers, "sec-fetch-site").unwrap_or("none");
        let ours = matches!(site, "same-origin" | "none")
            || header_str(headers, "sec-fetch-mode") == Some("navigate")
            || header_str(headers, header::ORIGIN.as_str()).is_some_and(|o| self.origin_allowed(o));
        let cookie = headers
            .get_all(header::COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(';'))
            .filter_map(|c| c.split_once('=').map(|(_, v)| v.trim()))
            .any(|v| same(&l.digest, v));
        if ours && cookie {
            return Ok(());
        }
        Err((
            StatusCode::UNAUTHORIZED,
            "This machine's illogical needs you to sign in from it.\n\n\
             Run this in a terminal here, which opens the page signed in:\n\nillogical web\n\n\
             (Or open the desktop app. Programs send the token in local-token, in illogical's state \
             directory, as Authorization: Bearer.)"
                .into(),
        ))
    }

    /// Where this daemon's page is, for links to it: its tailnet name, else
    /// loopback.
    pub fn page_origin(&self) -> &str {
        &self.page
    }

    /// Who may open a read-only share link (the link itself is the
    /// credential): any tailnet user, not only the owner, or this machine.
    /// Never a tagged node (sandboxes run untrusted agents), Funnel, or
    /// anyone off the tailnet. The Host check applies as usual.
    pub fn check_viewer(&self, headers: &HeaderMap, peer: &Peer) -> Result<(), Refusal> {
        match peer {
            Peer::Tailnet { login: Some(_) } => Ok(()),
            Peer::Tailnet { login: None } => {
                Err((StatusCode::FORBIDDEN, "tagged tailnet nodes can't open share links".into()))
            }
            Peer::Other => Err((StatusCode::FORBIDDEN, "share links are for the tailnet only".into())),
            Peer::Local if self.tunnel.is_some() => self.check_tunnel_token(headers),
            Peer::Local => match header_str(headers, "tailscale-user-login") {
                // Through serve, from a tailnet user.
                Some(_) => Ok(()),
                None if self.public.contains(&host(headers)) => Err((
                    StatusCode::FORBIDDEN,
                    "tailnet request without a user identity (a tagged node, or Funnel)".into(),
                )),
                None => Ok(()),
            },
        }
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
        match self.check_identity(headers, peer)? {
            Principal::Owner => Ok(()),
            Principal::User { name, .. } => Err((StatusCode::FORBIDDEN, format!("{name} is not the owner"))),
        }
    }

    pub fn check_host(&self, headers: &HeaderMap) -> Result<(), Refusal> {
        let host = host(headers);
        if !self.hosts.contains(&host) {
            return Err((StatusCode::MISDIRECTED_REQUEST, format!("unknown host {host:?}")));
        }
        Ok(())
    }

    /// Who this is: the owner, or another tailnet user (M12: who gets in
    /// only to what's been shared with them). Tagged nodes and strangers
    /// are refused.
    pub fn check_identity(&self, headers: &HeaderMap, peer: &Peer) -> Result<Principal, Refusal> {
        match peer {
            Peer::Tailnet { login: Some(login) } => self.who(login),
            Peer::Tailnet { login: None } => {
                Err((StatusCode::FORBIDDEN, "tagged tailnet nodes have no user identity".into()))
            }
            Peer::Other => Err((StatusCode::FORBIDDEN, "not from this machine or the tailnet".into())),
            Peer::Local if self.tunnel.is_some() => self.check_tunnel_token(headers).map(|()| Principal::Owner),
            Peer::Local => match header_str(headers, "tailscale-user-login") {
                Some(login) => self.who(login),
                None if self.public.contains(&host(headers)) => {
                    Err((StatusCode::FORBIDDEN, "tailnet request without a user identity (from a tagged node?)".into()))
                }
                None => self.check_local(headers).map(|()| Principal::Owner),
            },
        }
    }

    fn who(&self, login: &str) -> Result<Principal, Refusal> {
        match self.must_be_owner(login) {
            Ok(()) => Ok(Principal::Owner),
            // With no owner configured, nobody from the tailnet gets in.
            Err(e) if self.owner.is_none() => Err(e),
            Err(_) => Ok(Principal::tailnet(&login.to_ascii_lowercase())),
        }
    }

    fn check_tunnel_token(&self, headers: &HeaderMap) -> Result<(), Refusal> {
        let (Some(want), Some(given)) = (&self.tunnel, bearer(headers)) else {
            return Err((StatusCode::UNAUTHORIZED, "this daemon is reached through its home daemon's tunnel".into()));
        };
        if same(want, given) { Ok(()) } else { Err((StatusCode::UNAUTHORIZED, "wrong tunnel token".into())) }
    }

    fn must_be_owner(&self, login: &str) -> Result<(), Refusal> {
        match &self.owner {
            Some(owner) if owner.eq_ignore_ascii_case(login) => Ok(()),
            Some(_) => Err((StatusCode::FORBIDDEN, self.not_yours(login))),
            None => Err((
                StatusCode::FORBIDDEN,
                format!(
                    "You're signed in to Tailscale as {login}, and this machine has no owner set, so nobody \
                     from the tailnet gets in.\n\nIf it's yours, run this on it:\n\n{}\n",
                    owner_fix(login)
                ),
            )),
        }
    }

    /// A tailnet user who isn't the owner (#109): who they are, whose
    /// machine it is, and how to make it theirs if it is.
    pub fn not_yours(&self, login: &str) -> String {
        let owner = self.owner.as_deref().unwrap_or("nobody");
        format!(
            "You're signed in to Tailscale as {login}. This machine's owner is {owner}, and nothing on it \
             is shared with you.\n\nIf it's yours, run this on it:\n\n{}\n",
            owner_fix(login)
        )
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

impl std::fmt::Debug for LocalToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalToken").field("page", &self.page).finish_non_exhaustive()
    }
}

/// The lasting fix for the owner (#109): installed, so it survives restarts.
pub fn owner_fix(login: &str) -> String {
    format!("illogicald install -- --owner {login}")
}

/// A refusal as a page, for a browser (#109): the fix's command copyable.
pub fn refusal_page(status: StatusCode, why: &str) -> String {
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;");
    let body: String = why
        .split("\n\n")
        .filter(|p| !p.trim().is_empty())
        .map(|p| {
            let p = p.trim();
            if p.starts_with("illogicald ") || p == "illogical web" {
                format!(
                    "<p class=cmd><code id=fix>{}</code> <button onclick=\"navigator.clipboard.writeText(\
                     document.getElementById('fix').textContent).then(()=>this.textContent='Copied',()=>\
                     getSelection().selectAllChildren(document.getElementById('fix')))\">Copy</button></p>",
                    esc(p)
                )
            } else {
                format!("<p>{}</p>", esc(p))
            }
        })
        .collect();
    format!(
        "<!doctype html><meta charset=utf-8><meta name=viewport content=\"width=device-width,initial-scale=1\">\
         <title>illogical: {}</title><style>body{{font:16px/1.5 system-ui,sans-serif;max-width:36em;\
         margin:3em auto;padding:0 16px;color:#222;background:#fff}}@media(prefers-color-scheme:dark){{\
         body{{color:#ddd;background:#111}}}}code{{font:14px ui-monospace,monospace;word-break:break-all}}\
         .cmd{{display:flex;gap:.5em;align-items:center}}</style>{body}",
        status.as_u16()
    )
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    header_str(headers, header::AUTHORIZATION.as_str()).and_then(|v| v.strip_prefix("Bearer ")).map(str::trim)
}

/// `given`'s SHA-256 is `want`, compared in constant time (no early exit
/// on the first differing byte).
fn same(want: &[u8; 32], given: &str) -> bool {
    let got: [u8; 32] = Sha256::digest(given.trim().as_bytes()).into();
    got.iter().zip(want).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
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
    fn owner_refusals_say_who_and_the_fix() {
        let none = Access::new(7681, &["geek.example.ts.net".into()], &[], &[], None);
        let h = headers(&[("host", "geek.example.ts.net"), ("tailscale-user-login", "me@x.com")]);
        let (code, why) = none.check_identity(&h, &Peer::Local).unwrap_err();
        assert_eq!(code, StatusCode::FORBIDDEN);
        assert!(why.contains("as me@x.com") && why.contains("no owner set"), "{why}");
        assert!(why.contains("\n\nillogicald install -- --owner me@x.com\n"), "{why}");
        let a = access();
        let why = a.not_yours("friend@x.com");
        assert!(why.contains("as friend@x.com") && why.contains("owner is me@x.com"), "{why}");
        assert!(why.contains("illogicald install -- --owner friend@x.com"), "{why}");
        // As a page: escaped, the command on its own with a Copy button.
        let page = refusal_page(StatusCode::FORBIDDEN, &a.not_yours("<b>@x.com"));
        assert!(page.contains("&lt;b&gt;@x.com") && !page.contains("<b>@"), "{page}");
        assert!(page.contains("<code id=fix>illogicald install -- --owner &lt;b&gt;@x.com</code>"), "{page}");
        assert!(page.contains(">Copy</button>"));
    }

    #[test]
    fn the_tailnet_url_and_requests_from_it() {
        let a = access();
        assert_eq!(a.tailnet_url(), None);
        let a = a.with_tailnet_name("Geek.example.ts.net");
        assert_eq!(a.tailnet_url().as_deref(), Some("https://geek.example.ts.net"));
        let served = headers(&[("host", "geek.example.ts.net"), ("tailscale-user-login", "me@x.com")]);
        assert!(a.via_tailnet(&served, &Peer::Local));
        assert!(!a.via_tailnet(&headers(&[("host", "127.0.0.1:7681")]), &Peer::Local));
        assert!(a.via_tailnet(&headers(&[]), &Peer::Tailnet { login: Some("me@x.com".into()) }));
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
    fn share_viewers_are_tailnet_users_or_local() {
        let a = access();
        let public = headers(&[("host", "geek.example.ts.net")]);
        let friend = headers(&[("host", "geek.example.ts.net"), ("tailscale-user-login", "friend@x.com")]);
        // A tailnet user who isn't the owner: may view; is named, not the
        // owner, for the app (which lets them in only to what's shared, M12).
        assert!(a.check_viewer(&friend, &Peer::Local).is_ok());
        assert_eq!(a.check_identity(&friend, &Peer::Local).unwrap(), Principal::tailnet("friend@x.com"));
        assert!(a.check(&friend, &Peer::Local).is_err());
        let direct = Peer::Tailnet { login: Some("friend@x.com".into()) };
        assert!(a.check_viewer(&public, &direct).is_ok());
        // Tagged nodes, Funnel and the internet: never.
        assert!(a.check_viewer(&public, &Peer::Local).is_err(), "serve without a user");
        assert!(a.check_viewer(&public, &Peer::Tailnet { login: None }).is_err());
        assert!(a.check_viewer(&public, &Peer::Other).is_err());
        assert!(a.check_viewer(&friend, &Peer::Other).is_err(), "a forged header changes nothing");
        // This machine.
        assert!(a.check_viewer(&headers(&[("host", "127.0.0.1:7681")]), &Peer::Local).is_ok());
        assert_eq!(a.page_origin(), "https://geek.example.ts.net");
        assert_eq!(Access::new(7681, &[], &[], &[], None).page_origin(), "http://127.0.0.1:7681");
    }

    #[test]
    fn tunnelled_connections_need_the_token() {
        let token = "ilp_secret";
        let digest: String = Sha256::digest(token.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
        let a = Access::new(7681, &[], &[], &[], None).require_tunnel_token(&digest).unwrap();
        let local = [("host", "127.0.0.1:7681")];
        assert_eq!(check(&a, &local).unwrap_err().0, StatusCode::UNAUTHORIZED, "no token");
        let bearer = format!("Bearer {token}");
        assert!(check(&a, &[local[0], ("authorization", &bearer)]).is_ok());
        assert!(check(&a, &[local[0], ("authorization", "Bearer ilp_wrong")]).is_err());
        assert!(check(&a, &[local[0], ("authorization", token)]).is_err(), "not a bearer");
        // A forged serve header is no substitute.
        assert!(check(&a, &[local[0], ("tailscale-user-login", "me@x.com")]).is_err());
        // Nor for share links.
        assert!(a.check_viewer(&headers(&local), &Peer::Local).is_err());
        // The Host check still applies.
        assert!(check(&a, &[("host", "evil.com"), ("authorization", &bearer)]).is_err());
        assert!(Access::new(7681, &[], &[], &[], None).require_tunnel_token("abc").is_err());
    }

    #[test]
    fn loopback_callers_show_the_local_token() {
        let token = "ilt_a-test-token-for-the-access-checks";
        let a = access().require_local_token(token, "127.0.0.1:7681".parse().unwrap());
        let host = ("host", "127.0.0.1:7681");
        // Being on loopback is not enough.
        assert_eq!(check(&a, &[host]).unwrap_err().0, StatusCode::UNAUTHORIZED);
        let why = check(&a, &[host]).unwrap_err().1;
        assert!(why.contains("\n\nillogical web\n\n"), "{why}");
        // A program: its bearer.
        let bearer = format!("Bearer {token}");
        assert!(check(&a, &[host, ("authorization", &bearer)]).is_ok());
        assert!(check(&a, &[host, ("authorization", "Bearer ilt_wrong")]).is_err());
        assert!(check(&a, &[host, ("authorization", token)]).is_err(), "not a bearer");
        assert!(a.local_bearer(&headers(&[("authorization", &bearer)])));
        // A browser: the cookie, under any name, among others.
        let cookie = format!("other=1; illogical_7681={token}");
        assert!(check(&a, &[host, ("cookie", &cookie)]).is_ok());
        assert!(check(&a, &[host, ("cookie", "illogical_7681=ilt_wrong")]).is_err());
        let same = ("sec-fetch-site", "same-origin");
        assert!(check(&a, &[host, ("cookie", &cookie), same]).is_ok());
        // Typed in the address bar, or a link from elsewhere: a page load.
        let nav = [("sec-fetch-site", "cross-site"), ("sec-fetch-mode", "navigate")];
        assert!(check(&a, &[host, ("cookie", &cookie), nav[0], nav[1]]).is_ok());
        // What another site's page embeds (another port of 127.0.0.1 is
        // the same site) carries the cookie but isn't ours.
        for site in ["same-site", "cross-site"] {
            let embed = [host, ("cookie", &cookie), ("sec-fetch-site", site), ("sec-fetch-mode", "no-cors")];
            assert_eq!(check(&a, &embed).unwrap_err().0, StatusCode::UNAUTHORIZED, "{site}");
        }
        // One of our origins (another daemon's page, by --allow-origin).
        let home = [host, ("cookie", &cookie), ("sec-fetch-site", "same-site"), ("origin", "http://localhost:5173")];
        assert!(check(&a, &home).is_ok());
        // The tailnet's identity is still the tailnet's.
        let served = [("host", "geek.example.ts.net"), ("tailscale-user-login", "me@x.com")];
        assert!(check(&a, &served).is_ok());
        assert!(
            a.check(
                &headers(&[("host", "geek.example.ts.net:7681")]),
                &Peer::Tailnet { login: Some("me@x.com".into()) }
            )
            .is_err()
        );
        // Share links carry their own credential.
        assert!(a.check_viewer(&headers(&[host]), &Peer::Local).is_ok());
        // The sign-in link, and the token never in Debug.
        assert_eq!(a.signin_link().unwrap(), format!("http://127.0.0.1:7681/auth?token={token}"));
        assert!(a.is_local_token(token) && !a.is_local_token("ilt_x"));
        assert!(!format!("{a:?}").contains(token));
        assert_eq!(a.cookie_name(), "illogical_7681");
        let any = Access::new(7681, &[], &[], &[], None).require_local_token(token, "0.0.0.0:7681".parse().unwrap());
        assert!(any.signin_link().unwrap().starts_with("http://127.0.0.1:7681/auth?"));
        let none = Access::new(7681, &[], &[], &[], None);
        assert!(none.signin_link().is_none() && !none.is_local_token(token));
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
