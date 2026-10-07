//! Block sites: each browser block on a port gets an origin of its own,
//! served by the daemon's block listener and proxied to that port.
//!
//! **Why an origin per block.** A dev server in a machine runs code an agent
//! wrote. Served from the app's origin, any script in it could drive every
//! terminal. Served from one shared origin, blocks could read each other.
//! So block 42 is `b-42.<domain>`, at `/`, and dev servers need no base path.
//!
//! **Two schemes:**
//!
//! - **Tailnet** (`--block-domain`): `https://b-42.<domain>:<port>`. A
//!   wildcard DNS record points `*.<domain>` at this host's tailnet address,
//!   so only the tailnet reaches it; the daemon terminates TLS itself with a
//!   wildcard certificate (`tls.rs`), and asks tailscaled who is connecting
//!   (`tailscale whois`): only the owner gets through.
//! - **Dev** (no domain): `http://b-42-<key>.localhost:<port>`, on loopback
//!   only, for tests and local use. Browsers resolve `*.localhost` to
//!   loopback themselves. There is no identity here, so the name carries a
//!   random key, kept in the block's config: only the app (which learns it
//!   over its own authenticated socket) and the CLI know the name.
//!
//! **The proxy speaks HTTP, not raw bytes,** so that dev servers work with
//! no config and their own guards are replaced by ours:
//!
//! - it rewrites `Host` (and `Origin`, `Referer`) to `localhost:<port>`, so
//!   Vite's host check and Next's dev-origin check pass;
//! - which turns those guards off, so it enforces its own: a request whose
//!   `Origin` is anything but the block's own origin is refused (exact match,
//!   scheme and port included; `null` too), and so is any cross-site request
//!   that isn't a page navigation (`Sec-Fetch-Site`/`Sec-Fetch-Mode`);
//! - it strips `Tailscale-*`, `X-Forwarded-*` and `Forwarded`, so code in
//!   the machine never learns who you are;
//! - only the app may frame a block (`frame-ancestors`, plus the block's
//!   own pages: VS Code frames itself), and the dev server's own
//!   `X-Frame-Options` is dropped;
//! - a site may have a script of Arugula's put first in its pages
//!   (`Site::set_head_script`; editor blocks' storage, #69);
//! - it carries WebSocket upgrades, so hot reload works;
//! - it never proxies to the daemon's own ports.
//!
//! The app's side of the rule is in `access.rs`: its WebSocket and API
//! refuse every origin but the app's own, so a block's page can't reach them.

use std::{
    collections::HashMap,
    convert::Infallible,
    net::{IpAddr, SocketAddr},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use arugula_proto::PaneId;
use http_body_util::{BodyExt, Full, Limited, combinators::BoxBody};
use hyper::{
    HeaderMap, Method, Request, Response, StatusCode,
    body::{Bytes, Incoming},
    client::conn::http1::SendRequest,
    header::{self, HeaderName, HeaderValue},
    service::service_fn,
};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tracing::{debug, info, warn};

use crate::ports::Target;

type Body = BoxBody<Bytes, hyper::Error>;

/// How long a `tailscale whois` answer is trusted.
const WHOIS_FOR: Duration = Duration::from_secs(60);
/// At most this much of a page is read for its title.
const PROBE_LIMIT: usize = 512 * 1024;
/// Where a site's head script is served, on the site itself (so the page's
/// own `script-src 'self'` allows it).
pub const HEAD_SCRIPT: &str = "/.arugula/head.js";
/// At most this much of a page is read to put the head script in.
const PAGE_LIMIT: usize = 4 * 1024 * 1024;

/// How block sites are named and reached.
#[derive(Debug, Clone, PartialEq)]
pub enum Scheme {
    /// `http://b-<id>-<key>.localhost:<port>`, loopback only.
    Dev { port: u16 },
    /// `https://b-<id>.<domain>[:<port>]`, the owner only (by WhoIs).
    Tailnet { domain: String, port: u16 },
}

pub struct Settings {
    pub scheme: Scheme,
    /// The tailnet login allowed in (tailnet scheme).
    pub owner: Option<String>,
    /// The app's own origins: the only pages that may frame a block.
    pub app_origins: Vec<String>,
    /// This host's ports that are never proxied (the daemon's own).
    pub reserved: Vec<u16>,
}

/// What the proxy tells a block about its site.
#[derive(Debug, Clone, PartialEq)]
pub enum Report {
    /// The frame navigated to this path (and query).
    Navigated(String),
    /// The port stopped answering.
    Unreachable(String),
    /// It answers again.
    Reached,
}

/// One block's site.
pub struct Site {
    pub id: PaneId,
    /// Its hostname, e.g. `b-42.arugula.example.com`.
    pub host: String,
    /// Its origin, e.g. `https://b-42.arugula.example.com:7443`.
    pub origin: String,
    target: Mutex<Option<Target>>,
    down: AtomicBool,
    report: Box<dyn Fn(Report) + Send + Sync>,
    head: OnceLock<&'static str>,
}

impl Site {
    /// The page at `path` (starting with `/`), as the browser loads it.
    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.origin)
    }

    /// Point the site at a port.
    pub fn set_target(&self, t: Target) {
        *self.target.lock().unwrap() = Some(t);
        self.down.store(false, Ordering::Relaxed);
    }

    pub fn target(&self) -> Option<Target> {
        self.target.lock().unwrap().clone()
    }

    /// Put `js` first in this site's pages (HTML navigations), served from
    /// [`HEAD_SCRIPT`]. Once per site.
    pub fn set_head_script(&self, js: &'static str) {
        let _ = self.head.set(js);
    }

    fn reached(&self, ok: Result<(), &str>) {
        match ok {
            Ok(()) if self.down.swap(false, Ordering::Relaxed) => (self.report)(Report::Reached),
            Err(e) if !self.down.swap(true, Ordering::Relaxed) => (self.report)(Report::Unreachable(e.to_owned())),
            _ => {}
        }
    }
}

pub struct Sites {
    settings: Settings,
    sites: Mutex<HashMap<PaneId, Arc<Site>>>,
    whois: Mutex<HashMap<IpAddr, (Instant, Option<String>)>>,
}

static SITES: OnceLock<Arc<Sites>> = OnceLock::new();

/// The origins of Arugula control's page, while this daemon is enrolled:
/// pages that may frame blocks too, as the app's own may. Every URL control
/// answers at while it moves (#507). Control sets them whenever its
/// enrollment changes, before or after sites are installed.
static CONTROL_ORIGINS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Let control's pages (`origins`, or none) frame this daemon's blocks.
pub fn set_control_origins(origins: Vec<String>) {
    *CONTROL_ORIGINS.lock().unwrap() = origins;
}

/// Block sites, if this daemon serves them (`--block-listen`).
pub fn get() -> Option<Arc<Sites>> {
    SITES.get().cloned()
}

/// Turn block sites on (once per process); `serve` then serves them.
pub fn install(settings: Settings) -> Arc<Sites> {
    SITES.get_or_init(|| Arc::new(Sites { settings, sites: Mutex::default(), whois: Mutex::default() })).clone()
}

impl Sites {
    /// A site for block `id`. `key` is the block's own random key, part of
    /// its name in the dev scheme.
    pub fn open(&self, id: PaneId, key: &str, report: impl Fn(Report) + Send + Sync + 'static) -> Arc<Site> {
        let (host, origin) = match &self.settings.scheme {
            Scheme::Dev { port } => {
                let host = format!("b-{id}-{key}.localhost");
                let origin = format!("http://{host}:{port}");
                (host, origin)
            }
            Scheme::Tailnet { domain, port } => {
                let host = format!("b-{id}.{domain}");
                let origin = if *port == 443 { format!("https://{host}") } else { format!("https://{host}:{port}") };
                (host, origin)
            }
        };
        let site = Arc::new(Site {
            id,
            host,
            origin,
            target: Mutex::new(None),
            down: AtomicBool::new(false),
            report: Box::new(report),
            head: OnceLock::new(),
        });
        self.sites.lock().unwrap().insert(id, site.clone());
        site
    }

    pub fn close(&self, id: PaneId) {
        self.sites.lock().unwrap().remove(&id);
    }

    /// Whether a port of this host may be proxied.
    pub fn allowed(&self, t: &Target) -> Result<(), String> {
        match t {
            Target::Local(p) if self.settings.reserved.contains(p) => {
                Err(format!("port {p} is Arugula's own; it can't be shown in a block"))
            }
            _ => Ok(()),
        }
    }

    /// The site a `Host` header names, if it's one of ours.
    fn site_for(&self, host: &str) -> Option<Arc<Site>> {
        let host = host.to_ascii_lowercase();
        let (name, port) = match host.rsplit_once(':') {
            Some((n, p)) => (n, p.parse::<u16>().ok()?),
            None => (host.as_str(), 443),
        };
        let (suffix, want_port) = match &self.settings.scheme {
            Scheme::Dev { port } => (".localhost".to_owned(), *port),
            Scheme::Tailnet { domain, port } => (format!(".{domain}"), *port),
        };
        if port != want_port {
            return None;
        }
        let label = name.strip_suffix(&suffix)?;
        let id: PaneId = label.strip_prefix("b-")?.split('-').next()?.parse().ok()?;
        let site = self.sites.lock().unwrap().get(&id).cloned()?;
        // The whole name, key and all, must match.
        same(site.host.as_bytes(), name.as_bytes()).then_some(site)
    }

    /// Who may come in on this connection.
    async fn admit(&self, peer: SocketAddr) -> Result<(), String> {
        match &self.settings.scheme {
            Scheme::Dev { .. } if peer.ip().is_loopback() => Ok(()),
            Scheme::Dev { .. } => Err(format!("{} is not this host", peer.ip())),
            Scheme::Tailnet { .. } => {
                let login = self.whois(peer.ip()).await;
                match (&self.settings.owner, login) {
                    (Some(owner), Some(login)) if owner.eq_ignore_ascii_case(&login) => Ok(()),
                    (_, Some(login)) => Err(format!("{login} is not the owner")),
                    (_, None) => Err(format!("{} is not a person on this tailnet", peer.ip())),
                }
            }
        }
    }

    async fn whois(&self, ip: IpAddr) -> Option<String> {
        if let Some((at, login)) = self.whois.lock().unwrap().get(&ip)
            && at.elapsed() < WHOIS_FOR
        {
            return login.clone();
        }
        let login = whois(ip).await;
        self.whois.lock().unwrap().insert(ip, (Instant::now(), login.clone()));
        login
    }
}

/// Compares secrets without stopping at the first difference.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The login of the person whose device has `ip`, by asking tailscaled.
/// Tagged devices (servers) aren't people, so they get `None`.
async fn whois(ip: IpAddr) -> Option<String> {
    let out = tokio::process::Command::new("tailscale").args(["whois", "--json", &ip.to_string()]).output().await;
    let out = out.ok().filter(|o| o.status.success())?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    if v["Node"]["Tags"].as_array().is_some_and(|t| !t.is_empty()) {
        return None;
    }
    v["UserProfile"]["LoginName"].as_str().map(str::to_owned)
}

/// Serve block sites on `listener`: over TLS with `tls` (tailnet scheme),
/// else plain HTTP.
pub async fn serve(sites: Arc<Sites>, listener: TcpListener, tls: Option<Arc<tokio_rustls::rustls::ServerConfig>>) {
    let acceptor = tls.map(tokio_rustls::TlsAcceptor::from);
    loop {
        let (tcp, peer) = match listener.accept().await {
            Ok(c) => c,
            Err(e) => {
                warn!(error = %e, "block listener");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let (sites, acceptor) = (sites.clone(), acceptor.clone());
        tokio::spawn(async move {
            let _ = tcp.set_nodelay(true);
            let admitted = sites.admit(peer).await;
            if let Err(why) = &admitted {
                info!(%peer, why, "refusing block site connection");
            }
            let conn = Arc::new(Upstream::default());
            let svc = service_fn(move |req| {
                let (sites, admitted, conn) = (sites.clone(), admitted.clone(), conn.clone());
                async move { Ok::<_, Infallible>(handle(&sites, admitted, &conn, req).await) }
            });
            let http = hyper::server::conn::http1::Builder::new();
            let served = match acceptor {
                Some(a) => match a.accept(tcp).await {
                    Ok(tls) => http.serve_connection(TokioIo::new(tls), svc).with_upgrades().await,
                    Err(e) => {
                        debug!(%peer, error = %e, "TLS handshake");
                        return;
                    }
                },
                None => http.serve_connection(TokioIo::new(tcp), svc).with_upgrades().await,
            };
            if let Err(e) = served {
                debug!(%peer, error = %e, "block site connection");
            }
        });
    }
}

/// The connection to the port that one browser connection's requests reuse
/// (one Sprites proxy socket per browser connection, not per request).
#[derive(Default)]
struct Upstream(tokio::sync::Mutex<Option<(PaneId, Target, SendRequest<Body>)>>);

/// Why a request didn't get an answer.
enum Failed {
    /// Couldn't reach the port.
    Dial(std::io::Error),
    /// Reached it, but the exchange broke.
    Http(hyper::Error),
}

impl Upstream {
    /// Send on the kept connection, or a new one. A kept connection the
    /// port has meanwhile closed gets the request back unsent, and it goes
    /// out once more on a new one.
    async fn send(&self, site: &Site, target: &Target, req: Request<Body>) -> Result<Response<Incoming>, Failed> {
        let mut slot = self.0.lock().await;
        let kept = match slot.take() {
            Some((id, t, mut s)) if id == site.id && t == *target => s.ready().await.is_ok().then_some(s),
            _ => None,
        };
        let mut req = Some(req);
        if let Some(mut s) = kept {
            let sent = s.try_send_request(req.take().expect("request"));
            *slot = Some((site.id, target.clone(), s));
            drop(slot);
            match sent.await {
                Ok(r) => return Ok(r),
                Err(mut e) => match e.take_message() {
                    Some(unsent) => req = Some(unsent),
                    None => return Err(Failed::Http(e.into_error())),
                },
            }
            slot = self.0.lock().await;
        }
        let mut s = connect(target).await.map_err(Failed::Dial)?;
        let sent = s.send_request(req.take().expect("request"));
        *slot = Some((site.id, target.clone(), s));
        drop(slot);
        sent.await.map_err(Failed::Http)
    }
}

async fn connect(target: &Target) -> std::io::Result<SendRequest<Body>> {
    let io = target.dial().await?;
    let (sender, conn) =
        hyper::client::conn::http1::handshake(TokioIo::new(io)).await.map_err(std::io::Error::other)?;
    tokio::spawn(async move {
        let _ = conn.with_upgrades().await;
    });
    Ok(sender)
}

fn full(status: StatusCode, text: impl Into<String>) -> Response<Body> {
    let mut r = Response::new(Full::new(Bytes::from(text.into())).map_err(|e| match e {}).boxed());
    *r.status_mut() = status;
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain; charset=utf-8"));
    r
}

/// Why a request is refused, if it is (the checks that don't depend on the
/// port: who, which site, from where).
fn check<B>(sites: &Sites, admitted: Result<(), String>, req: &Request<B>) -> Result<Arc<Site>, (StatusCode, String)> {
    if let Err(why) = admitted {
        return Err((StatusCode::FORBIDDEN, why));
    }
    let h = req.headers();
    let host = text(h, header::HOST.as_str()).or_else(|| req.uri().authority().map(|a| a.as_str())).unwrap_or("");
    let site = sites.site_for(host).ok_or((StatusCode::NOT_FOUND, "no such block".into()))?;
    if let Some(origin) = text(h, header::ORIGIN.as_str())
        && origin != site.origin
    {
        return Err((StatusCode::FORBIDDEN, format!("origin {origin} may not use this block")));
    }
    let fetch_site = text(h, "sec-fetch-site").unwrap_or("");
    let navigating = text(h, "sec-fetch-mode") == Some("navigate");
    if matches!(fetch_site, "cross-site" | "same-site") && !navigating {
        return Err((StatusCode::FORBIDDEN, "other sites can't fetch from this block".into()));
    }
    Ok(site)
}

/// A refusal, for finding out why a page in a block doesn't work (#69).
fn refused(req: &Request<Incoming>, status: StatusCode, why: &str) {
    let h = req.headers();
    debug!(
        status = status.as_u16(),
        why,
        method = %req.method(),
        host = text(h, header::HOST.as_str()).unwrap_or(""),
        path = req.uri().path(),
        origin = text(h, header::ORIGIN.as_str()).unwrap_or(""),
        sec_fetch_site = text(h, "sec-fetch-site").unwrap_or(""),
        sec_fetch_mode = text(h, "sec-fetch-mode").unwrap_or(""),
        sec_fetch_dest = text(h, "sec-fetch-dest").unwrap_or(""),
        "refusing block site request"
    );
}

fn text<'h>(h: &'h HeaderMap, name: &str) -> Option<&'h str> {
    h.get(name).and_then(|v| v.to_str().ok())
}

/// Hop-by-hop headers: for one connection, never forwarded.
const HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-connection",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

fn is_upgrade(h: &HeaderMap) -> bool {
    text(h, header::CONNECTION.as_str()).is_some_and(|c| c.to_ascii_lowercase().contains("upgrade"))
        && h.contains_key(header::UPGRADE)
}

/// The request as the port sees it: from `localhost:<port>`, with nothing
/// that says who you are.
fn rewrite_request(h: &mut HeaderMap, site: &Site, local: &str) {
    let upgrade = is_upgrade(h);
    let local_origin = format!("http://{local}");
    for name in HOP {
        if !(upgrade && (*name == "connection" || *name == "upgrade")) {
            h.remove(*name);
        }
    }
    let identity: Vec<HeaderName> = h
        .keys()
        .filter(|k| {
            let k = k.as_str();
            k.starts_with("tailscale-") || k.starts_with("x-forwarded-") || k == "forwarded" || k == "x-real-ip"
        })
        .cloned()
        .collect();
    for k in identity {
        h.remove(k);
    }
    if upgrade {
        h.insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
    }
    h.insert(header::HOST, HeaderValue::from_str(local).expect("host"));
    // `check` let through only our own origin.
    if h.contains_key(header::ORIGIN) {
        h.insert(header::ORIGIN, HeaderValue::from_str(&local_origin).expect("origin"));
    }
    if let Some(r) = text(h, header::REFERER.as_str()).and_then(|r| r.strip_prefix(&site.origin))
        && let Ok(v) = HeaderValue::from_str(&format!("{local_origin}{r}"))
    {
        h.insert(header::REFERER, v);
    }
}

/// The answer as the browser sees it: from the block's origin, framed only
/// by the app.
fn rewrite_response(h: &mut HeaderMap, site: &Site, local: &str, frame_ancestors: &HeaderValue, upgraded: bool) {
    for name in HOP {
        if !(upgraded && (*name == "connection" || *name == "upgrade")) {
            h.remove(*name);
        }
    }
    if let Some(loc) = text(h, header::LOCATION.as_str()) {
        let ip = local.replacen("localhost", "127.0.0.1", 1);
        let local = [format!("http://{local}"), format!("http://{ip}")];
        if let Some(rest) = local.iter().find_map(|l| loc.strip_prefix(l.as_str()))
            && let Ok(v) = HeaderValue::from_str(&format!("{}{rest}", site.origin))
        {
            h.insert(header::LOCATION, v);
        }
    }
    h.remove(header::X_FRAME_OPTIONS);
    // A second policy: browsers enforce both, so the server's own stays.
    h.append(header::CONTENT_SECURITY_POLICY, frame_ancestors.clone());
}

async fn handle(
    sites: &Sites,
    admitted: Result<(), String>,
    up: &Upstream,
    mut req: Request<Incoming>,
) -> Response<Body> {
    let site = match check(sites, admitted, &req) {
        Ok(s) => s,
        Err((status, why)) => {
            refused(&req, status, &why);
            return full(status, why);
        }
    };
    let Some(target) = site.target() else {
        return full(StatusCode::SERVICE_UNAVAILABLE, "this block isn't showing a port");
    };
    if let Err(why) = sites.allowed(&target) {
        refused(&req, StatusCode::FORBIDDEN, &why);
        return full(StatusCode::FORBIDDEN, why);
    }
    let head = site.head.get().copied();
    if let Some(js) = head
        && req.uri().path() == HEAD_SCRIPT
    {
        let mut r = full(StatusCode::OK, js);
        r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("text/javascript; charset=utf-8"));
        r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
        return r;
    }
    let local = target.authority();
    let what = target.what();
    let h = req.headers();
    let navigating = req.method() == Method::GET && text(h, "sec-fetch-mode") == Some("navigate");
    if navigating && text(h, "sec-fetch-dest") == Some("iframe") {
        let path = req.uri().path_and_query().map(|p| p.as_str()).unwrap_or("/").to_owned();
        (site.report)(Report::Navigated(path));
    }
    // A page that gets the head script comes uncompressed, to be edited.
    let edit_page = head.is_some() && navigating;
    let upgrade = is_upgrade(req.headers());
    let client_upgrade = upgrade.then(|| hyper::upgrade::on(&mut req));
    let (mut parts, body) = req.into_parts();
    rewrite_request(&mut parts.headers, &site, &local);
    if edit_page {
        parts.headers.remove(header::ACCEPT_ENCODING);
    }
    // Origin-form only: the port's server sees a request to itself.
    parts.uri = parts.uri.path_and_query().map(|p| p.as_str()).unwrap_or("/").parse().unwrap_or_default();
    let out = Request::from_parts(parts, body.boxed());
    // An upgraded connection is used up, so it gets one of its own.
    let sent = if upgrade {
        match connect(&target).await {
            Ok(mut s) => s.send_request(out).await.map_err(Failed::Http),
            Err(e) => Err(Failed::Dial(e)),
        }
    } else {
        up.send(&site, &target, out).await
    };
    let mut res = match sent {
        Ok(r) => {
            site.reached(Ok(()));
            r
        }
        Err(Failed::Dial(e)) => {
            let why = format!("nothing is answering on {what}: {e}");
            site.reached(Err(&why));
            return full(StatusCode::BAD_GATEWAY, why);
        }
        Err(Failed::Http(e)) => return full(StatusCode::BAD_GATEWAY, format!("{what}: {e}")),
    };
    let upgraded = res.status() == StatusCode::SWITCHING_PROTOCOLS;
    let ancestors = frame_ancestors(&pages(&sites.settings.app_origins));
    rewrite_response(res.headers_mut(), &site, &local, &ancestors, upgraded);
    let html = text(res.headers(), header::CONTENT_TYPE.as_str()).is_some_and(|t| t.starts_with("text/html"));
    if edit_page && html && !res.headers().contains_key(header::CONTENT_ENCODING) {
        let (mut parts, body) = res.into_parts();
        let page = match Limited::new(body, PAGE_LIMIT).collect().await {
            Ok(b) => with_head_script(&b.to_bytes()),
            Err(e) => return full(StatusCode::BAD_GATEWAY, format!("{what}: {e}")),
        };
        parts.headers.remove(header::CONTENT_LENGTH);
        return Response::from_parts(parts, Full::new(Bytes::from(page)).map_err(|e| match e {}).boxed());
    }
    if upgraded && let Some(client) = client_upgrade {
        let server = hyper::upgrade::on(&mut res);
        tokio::spawn(async move {
            let (Ok(client), Ok(server)) = tokio::join!(client, server) else { return };
            let (mut a, mut b) = (TokioIo::new(client), TokioIo::new(server));
            let _ = tokio::io::copy_bidirectional(&mut a, &mut b).await;
        });
    }
    res.map(|b| b.boxed())
}

/// The app's pages may frame a block, and so may the block's own (VS Code
/// puts its web worker extension host in a frame of its own origin).
/// Every ancestor must match, so a block inside a block is still only ever
/// inside the app. IPv6 literals are left out: CSP has no syntax for them.
/// The pages that may frame a block: the app's own, and control's while
/// this daemon is enrolled (it shows the same blocks, #69).
fn pages(app: &[String]) -> Vec<String> {
    let mut pages = app.to_vec();
    pages.extend(CONTROL_ORIGINS.lock().unwrap().iter().cloned());
    pages
}

fn frame_ancestors(app: &[String]) -> HeaderValue {
    let mut list = vec!["'self'"];
    list.extend(app.iter().map(String::as_str).filter(|o| !o.contains("://[")));
    HeaderValue::from_str(&format!("frame-ancestors {}", list.join(" ")))
        .unwrap_or(HeaderValue::from_static("frame-ancestors 'self'"))
}

/// `html` with a `<script>` for [`HEAD_SCRIPT`] first in its `<head>`
/// (or first of all, without one).
fn with_head_script(html: &[u8]) -> Vec<u8> {
    let tag = format!("<script src=\"{HEAD_SCRIPT}\"></script>");
    let lower = html.to_ascii_lowercase();
    let at = (0..lower.len())
        .find(|&i| {
            lower[i..].starts_with(b"<head") && matches!(lower.get(i + 5), Some(b'>' | b' ' | b'\t' | b'\n' | b'\r'))
        })
        .and_then(|i| lower[i..].iter().position(|&c| c == b'>').map(|j| i + j + 1))
        .unwrap_or(0);
    let mut out = Vec::with_capacity(html.len() + tag.len());
    out.extend_from_slice(&html[..at]);
    out.extend_from_slice(tag.as_bytes());
    out.extend_from_slice(&html[at..]);
    out
}

/// `GET path` on a port, as the proxy would send it: (status, up to
/// `PROBE_LIMIT` of the body). For a block's title and whether it answers.
pub async fn probe(target: &Target, path: &str) -> Result<(u16, String), String> {
    let what = target.what();
    let mut sender = connect(target).await.map_err(|e| format!("nothing is answering on {what}: {e}"))?;
    let req = Request::get(path)
        .header(header::HOST, target.authority())
        .header(header::ACCEPT, "text/html,*/*")
        .body(Full::new(Bytes::new()).map_err(|e| match e {}).boxed())
        .map_err(|e| e.to_string())?;
    let res = tokio::time::timeout(Duration::from_secs(15), sender.send_request(req))
        .await
        .map_err(|_| format!("{what} didn't answer in time"))?
        .map_err(|e| format!("{what}: {e}"))?;
    let status = res.status().as_u16();
    let body = match tokio::time::timeout(Duration::from_secs(10), Limited::new(res.into_body(), PROBE_LIMIT).collect())
        .await
    {
        Ok(Ok(b)) => String::from_utf8_lossy(&b.to_bytes()).into_owned(),
        _ => String::new(),
    };
    Ok((status, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sites(scheme: Scheme) -> Sites {
        Sites {
            settings: Settings {
                scheme,
                owner: Some("me@x.com".into()),
                app_origins: vec!["http://127.0.0.1:7681".into()],
                reserved: vec![7681, 7690],
            },
            sites: Mutex::default(),
            whois: Mutex::default(),
        }
    }

    #[test]
    fn dev_names_carry_the_key() {
        let s = sites(Scheme::Dev { port: 7690 });
        let site = s.open(42, "k3y", |_| {});
        assert_eq!(site.origin, "http://b-42-k3y.localhost:7690");
        assert_eq!(site.url("/a?b"), "http://b-42-k3y.localhost:7690/a?b");
        assert!(s.site_for("b-42-k3y.localhost:7690").is_some());
        assert!(s.site_for("B-42-K3Y.localhost:7690").is_some());
        // Without the key, with a wrong one, another port, another block.
        assert!(s.site_for("b-42.localhost:7690").is_none());
        assert!(s.site_for("b-42-k3z.localhost:7690").is_none());
        assert!(s.site_for("b-42-k3y.localhost:7691").is_none());
        assert!(s.site_for("b-42-k3y.localhost").is_none());
        assert!(s.site_for("b-43-k3y.localhost:7690").is_none());
        assert!(s.site_for("127.0.0.1:7690").is_none());
        s.close(42);
        assert!(s.site_for("b-42-k3y.localhost:7690").is_none());
    }

    #[test]
    fn tailnet_names() {
        let s = sites(Scheme::Tailnet { domain: "arugula.example.com".into(), port: 7443 });
        let site = s.open(7, "ignored", |_| {});
        assert_eq!(site.origin, "https://b-7.arugula.example.com:7443");
        assert!(s.site_for("b-7.arugula.example.com:7443").is_some());
        assert!(s.site_for("b-7.arugula.example.com").is_none());
        assert!(s.site_for("b-7.arugula.example.com.evil.com:7443").is_none());
        assert!(s.site_for("b-7.evil.com:7443").is_none());
        let s = sites(Scheme::Tailnet { domain: "arugula.example.com".into(), port: 443 });
        let site = s.open(7, "", |_| {});
        assert_eq!(site.origin, "https://b-7.arugula.example.com");
        assert!(s.site_for("b-7.arugula.example.com").is_some());
        assert!(s.site_for("b-7.arugula.example.com:443").is_some());
    }

    #[tokio::test]
    async fn who_may_connect() {
        let dev = sites(Scheme::Dev { port: 7690 });
        assert!(dev.admit("127.0.0.1:5000".parse().unwrap()).await.is_ok());
        assert!(dev.admit("[::1]:5000".parse().unwrap()).await.is_ok());
        assert!(dev.admit("100.64.0.9:5000".parse().unwrap()).await.is_err());
        let tailnet = sites(Scheme::Tailnet { domain: "d.example".into(), port: 7443 });
        let ip: IpAddr = "100.64.0.9".parse().unwrap();
        tailnet.whois.lock().unwrap().insert(ip, (Instant::now(), Some("ME@x.com".into())));
        assert!(tailnet.admit(SocketAddr::new(ip, 1)).await.is_ok());
        let friend: IpAddr = "100.64.0.10".parse().unwrap();
        tailnet.whois.lock().unwrap().insert(friend, (Instant::now(), Some("friend@x.com".into())));
        assert!(tailnet.admit(SocketAddr::new(friend, 1)).await.is_err());
        let server: IpAddr = "100.64.0.11".parse().unwrap();
        tailnet.whois.lock().unwrap().insert(server, (Instant::now(), None));
        assert!(tailnet.admit(SocketAddr::new(server, 1)).await.is_err());
    }

    #[test]
    fn own_ports_are_never_proxied() {
        let s = sites(Scheme::Dev { port: 7690 });
        assert!(s.allowed(&Target::Local(7681)).is_err());
        assert!(s.allowed(&Target::Local(7690)).is_err());
        assert!(s.allowed(&Target::Local(5173)).is_ok());
    }

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.append(HeaderName::from_bytes(k.as_bytes()).unwrap(), v.parse().unwrap());
        }
        h
    }

    #[test]
    fn requests_lose_identity_and_look_local() {
        let s = sites(Scheme::Tailnet { domain: "d.example".into(), port: 7443 });
        let site = s.open(3, "", |_| {});
        let mut h = headers(&[
            ("host", "b-3.d.example:7443"),
            ("origin", "https://b-3.d.example:7443"),
            ("referer", "https://b-3.d.example:7443/page?x=1"),
            ("tailscale-user-login", "me@x.com"),
            ("tailscale-user-name", "Me"),
            ("x-forwarded-for", "100.64.0.9"),
            ("forwarded", "for=100.64.0.9"),
            ("connection", "keep-alive"),
            ("keep-alive", "timeout=5"),
            ("cookie", "a=b"),
        ]);
        rewrite_request(&mut h, &site, "localhost:5173");
        assert_eq!(h["host"], "localhost:5173");
        assert_eq!(h["origin"], "http://localhost:5173");
        assert_eq!(h["referer"], "http://localhost:5173/page?x=1");
        assert_eq!(h["cookie"], "a=b");
        for gone in
            ["tailscale-user-login", "tailscale-user-name", "x-forwarded-for", "forwarded", "connection", "keep-alive"]
        {
            assert!(!h.contains_key(gone), "{gone} was forwarded");
        }
        // WebSocket upgrades keep what makes them upgrades.
        let mut h = headers(&[("connection", "keep-alive, Upgrade"), ("upgrade", "websocket"), ("host", "x")]);
        rewrite_request(&mut h, &site, "localhost:5173");
        assert_eq!(h["connection"], "upgrade");
        assert_eq!(h["upgrade"], "websocket");
    }

    #[test]
    fn answers_are_framed_only_by_the_app() {
        let s = sites(Scheme::Dev { port: 7690 });
        let site = s.open(3, "k", |_| {});
        let fa = frame_ancestors(&["http://127.0.0.1:7681".into(), "https://geek.example.ts.net".into()]);
        let mut h = headers(&[
            ("x-frame-options", "DENY"),
            ("location", "http://localhost:5173/next"),
            ("content-security-policy", "default-src 'self'"),
        ]);
        rewrite_response(&mut h, &site, "localhost:5173", &fa, false);
        assert!(!h.contains_key("x-frame-options"));
        assert_eq!(h["location"], "http://b-3-k.localhost:7690/next");
        let csp: Vec<_> = h.get_all("content-security-policy").iter().map(|v| v.to_str().unwrap()).collect();
        assert_eq!(
            csp,
            ["default-src 'self'", "frame-ancestors 'self' http://127.0.0.1:7681 https://geek.example.ts.net"]
        );
        let mut h = headers(&[("location", "https://elsewhere.example/")]);
        rewrite_response(&mut h, &site, "localhost:5173", &fa, false);
        assert_eq!(h["location"], "https://elsewhere.example/");
    }

    #[test]
    fn control_page_may_frame_blocks_while_enrolled() {
        let app = vec!["https://geek.example.ts.net".to_string()];
        set_control_origins(vec!["https://control.example.com".into(), "https://old.example.com".into()]);
        assert_eq!(
            frame_ancestors(&pages(&app)),
            "frame-ancestors 'self' https://geek.example.ts.net https://control.example.com https://old.example.com"
        );
        set_control_origins(vec![]);
        assert_eq!(frame_ancestors(&pages(&app)), "frame-ancestors 'self' https://geek.example.ts.net");
    }

    #[test]
    fn frame_ancestors_leave_out_what_csp_cant_say() {
        let fa = frame_ancestors(&["http://127.0.0.1:7681".into(), "http://[::1]:7681".into()]);
        assert_eq!(fa, "frame-ancestors 'self' http://127.0.0.1:7681");
        assert_eq!(frame_ancestors(&[]), "frame-ancestors 'self'");
    }

    #[test]
    fn the_head_script_goes_first_in_the_head() {
        let tag = format!("<script src=\"{HEAD_SCRIPT}\"></script>");
        let page = |s: &str| String::from_utf8(with_head_script(s.as_bytes())).unwrap();
        assert_eq!(
            page("<!-- c --><html><HEAD lang=x><script>a</script></head></html>"),
            format!("<!-- c --><html><HEAD lang=x>{tag}<script>a</script></head></html>")
        );
        assert_eq!(page("<header><head>x"), format!("<header><head>{tag}x"));
        assert_eq!(page("<p>no head"), format!("{tag}<p>no head"));
    }

    #[test]
    fn refusals() {
        let s = sites(Scheme::Dev { port: 7690 });
        let site = s.open(5, "k", |_| {});
        let req = |pairs: &[(&str, &str)]| {
            let mut b = Request::get("/");
            for (k, v) in pairs {
                b = b.header(*k, *v);
            }
            b.body(()).unwrap()
        };
        let ok = |r: &Request<()>| check(&s, Ok(()), r).map(|s| s.id).map_err(|(st, _)| st);
        let host = ("host", "b-5-k.localhost:7690");
        // The block's own page, and a frame navigating to it from the app.
        assert_eq!(ok(&req(&[host, ("sec-fetch-site", "same-origin"), ("origin", &site.origin)])), Ok(5));
        assert_eq!(ok(&req(&[host, ("sec-fetch-site", "cross-site"), ("sec-fetch-mode", "navigate")])), Ok(5));
        // Another site fetching from it, or claiming another origin.
        assert_eq!(
            ok(&req(&[host, ("sec-fetch-site", "cross-site"), ("sec-fetch-mode", "cors")])),
            Err(StatusCode::FORBIDDEN)
        );
        assert_eq!(
            ok(&req(&[host, ("sec-fetch-site", "same-site"), ("sec-fetch-mode", "no-cors")])),
            Err(StatusCode::FORBIDDEN)
        );
        assert_eq!(ok(&req(&[host, ("origin", "http://127.0.0.1:7681")])), Err(StatusCode::FORBIDDEN));
        assert_eq!(ok(&req(&[host, ("origin", "null")])), Err(StatusCode::FORBIDDEN));
        assert_eq!(ok(&req(&[("host", "b-5-x.localhost:7690")])), Err(StatusCode::NOT_FOUND));
        assert_eq!(
            check(&s, Err("no".into()), &req(&[host])).map(|s| s.id).map_err(|(st, _)| st),
            Err(StatusCode::FORBIDDEN)
        );
    }

    #[test]
    fn reports_only_changes() {
        let got = Arc::new(Mutex::new(vec![]));
        let s = sites(Scheme::Dev { port: 7690 });
        let g = got.clone();
        let site = s.open(1, "k", move |r| g.lock().unwrap().push(r));
        site.reached(Ok(()));
        site.reached(Err("down"));
        site.reached(Err("still down"));
        site.reached(Ok(()));
        site.reached(Ok(()));
        assert_eq!(*got.lock().unwrap(), [Report::Unreachable("down".into()), Report::Reached]);
    }
}
