//! Browser blocks (M6a): a web page beside your terminals.
//!
//! Two kinds of page:
//!
//! - **a port on the block's machine** (`{ "port": 5173, "path": "/" }`):
//!   usually a dev server. The block gets a site of its own (`sites.rs`),
//!   an origin that proxies to the port on its host: this host, or the
//!   sprite of the VM tab (or pane) it was opened from. The frame's own
//!   navigations come back from the proxy, so the block knows where it is;
//!   when the port stops answering it asks for you (`needs-input`) and
//!   comes back by itself when the server does.
//! - **any other page** (`{ "url": … }`): the client frames it if the site
//!   allows it and shows a card with "open in new tab" if it doesn't.
//!
//! Both are `working` while loading and `needs-input` when loading fails,
//! and emit `navigated` and `load_error` events.

use std::{
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use futures_util::future::BoxFuture;
use illogical_proto::{Attention, BlockType, EventKind};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::info;

use crate::{
    block::{Block, BlockCtx, no_method},
    ports::Target,
    sites::{self, Report, Site},
};

/// How often a port that stopped answering is tried again.
const RETRY: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Default, Deserialize)]
struct Config {
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    path: Option<String>,
    /// The random part of its site's name (dev scheme).
    #[serde(default)]
    key: Option<String>,
}

/// What a block shows.
#[derive(Debug, Clone, PartialEq)]
enum Page {
    /// A page elsewhere, framed as it is.
    Web(String),
    /// A port on the block's machine, through its own site.
    Port { port: u16, path: String },
}

impl Page {
    /// From what someone typed: `:5173/path`, `/path` (same port), or a URL.
    fn parse(s: &str, port_now: Option<u16>) -> Result<Self, String> {
        let s = s.trim();
        if let Some(rest) = s.strip_prefix(':') {
            let digits = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
            let port = rest[..digits].parse::<u16>().ok().filter(|p| *p > 0).ok_or(format!("not a port: {s}"))?;
            return Ok(Self::Port { port, path: path(&rest[digits..])? });
        }
        if s.starts_with('/')
            && let Some(port) = port_now
        {
            return Ok(Self::Port { port, path: path(s)? });
        }
        normalize(s).map(Self::Web)
    }

    fn show(&self) -> String {
        match self {
            Self::Web(u) => u.clone(),
            Self::Port { port, path } => format!(":{port}{path}"),
        }
    }
}

/// A path for a port's page: starts with `/`, may have a query.
fn path(p: &str) -> Result<String, String> {
    let p = if p.is_empty() { "/".to_owned() } else { p.to_owned() };
    if !p.starts_with('/') || p.contains(char::is_whitespace) {
        return Err(format!("not a path: {p}"));
    }
    Ok(p)
}

#[derive(Debug, Clone, Default, Serialize)]
struct State {
    /// Where the page is now, as a browser loads it.
    url: String,
    /// What the frame was last told to load (it moves on by itself).
    src: String,
    title: Option<String>,
    /// Whether the site lets itself be framed (`None` while checking).
    framable: Option<bool>,
    /// Why it couldn't be loaded, if it couldn't.
    error: Option<String>,
    loading: bool,
    /// Bumped by `reload`, so the client reloads the frame.
    reloads: u64,
    /// Where `back` goes, as shown.
    back: Vec<String>,
    /// For a port: which, the path on it, and the machine it's on (none
    /// for this host).
    port: Option<u16>,
    path: Option<String>,
    machine: Option<String>,
    #[serde(skip)]
    back_pages: Vec<Page>,
}

pub struct Browser {
    ctx: BlockCtx,
    me: Weak<Browser>,
    state: Mutex<State>,
    page: Mutex<Page>,
    key: String,
    site: Mutex<Option<Arc<Site>>>,
    http: reqwest::Client,
    /// Bumped by every navigation, so stale probes and retries stop.
    epoch: AtomicU64,
    retrying: AtomicBool,
    closed: AtomicBool,
}

impl Browser {
    pub fn create(ctx: BlockCtx, config: Value) -> Result<Arc<dyn Block>, String> {
        let config: Config = serde_json::from_value(config).map_err(|e| format!("browser config: {e}"))?;
        let page = match (&config.port, &config.url) {
            (Some(port), _) => Page::Port { port: *port, path: path(config.path.as_deref().unwrap_or("/"))? },
            (None, Some(url)) => Page::parse(url, None)?,
            (None, None) => return Err("a browser block needs a url or a port".into()),
        };
        if !ctx.restoring {
            usable(&page, ctx.sprite.is_some())?;
        }
        let http = crate::roots::http()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .map_err(|e| e.to_string())?;
        let key = config
            .key
            .filter(|k| k.len() >= 16 && k.chars().all(|c| c.is_ascii_alphanumeric()))
            .unwrap_or_else(new_key);
        let b = Arc::new_cyclic(|me| Self {
            ctx,
            me: me.clone(),
            state: Mutex::new(State::default()),
            page: Mutex::new(page.clone()),
            key,
            site: Mutex::new(None),
            http,
            epoch: AtomicU64::new(0),
            retrying: AtomicBool::new(false),
            closed: AtomicBool::new(false),
        });
        b.go(page, false);
        Ok(b)
    }

    /// Where the block's ports are.
    fn target(&self, port: u16) -> Result<Target, String> {
        match (&self.ctx.sprite, &self.ctx.provider) {
            (None, _) => Ok(Target::Local(port)),
            (Some(sprite), Some(provider)) => {
                Ok(Target::Sprite { provider: provider.clone(), sprite: sprite.clone(), port })
            }
            (Some(_), None) => Err("this block's machine can't be reached: VM panes aren't set up".into()),
        }
    }

    /// Its site, made the first time it shows a port.
    fn site(&self) -> Result<Arc<Site>, String> {
        let mut slot = self.site.lock().unwrap();
        if let Some(s) = &*slot {
            return Ok(s.clone());
        }
        let sites = sites::get().ok_or("browser blocks on ports are off: start illogicald with --block-listen")?;
        let me = self.me.clone();
        let site = sites.open(self.ctx.id, &self.key, move |r| {
            if let Some(b) = me.upgrade() {
                b.reported(r);
            }
        });
        *slot = Some(site.clone());
        Ok(site)
    }

    /// Show `page`, then find out whether it loads and what it's called.
    fn go(&self, page: Page, push_back: bool) {
        let epoch = self.epoch.fetch_add(1, Ordering::Relaxed) + 1;
        let old = std::mem::replace(&mut *self.page.lock().unwrap(), page.clone());
        // A port is reached through its site; set it up first.
        let routed = match &page {
            Page::Web(url) => Ok((url.clone(), None)),
            Page::Port { port, path } => self.target(*port).and_then(|t| {
                let site = self.site()?;
                sites::get().map(|s| s.allowed(&t)).transpose()?;
                site.set_target(t.clone());
                Ok((site.url(path), Some(t)))
            }),
        };
        {
            let mut st = self.state.lock().unwrap();
            if push_back && !st.url.is_empty() {
                st.back.push(old.show());
                st.back_pages.push(old);
            }
            let (url, src) = match &routed {
                Ok((url, _)) => (url.clone(), url.clone()),
                Err(_) => (page.show(), String::new()),
            };
            st.url = url;
            st.src = src;
            st.title = None;
            st.framable = None;
            st.error = None;
            st.loading = true;
            (st.port, st.path) = match &page {
                Page::Port { port, path } => (Some(*port), Some(path.clone())),
                Page::Web(_) => (None, None),
            };
            st.machine = self.ctx.sprite.clone().filter(|_| matches!(page, Page::Port { .. }));
        }
        self.log(&json!({ "e": "navigate", "to": page.show() }));
        self.ctx.attention(Attention::Working, "loading");
        self.ctx.changed();
        let (url, target) = match routed {
            Ok(r) => r,
            Err(e) => return self.failed(epoch, &page.show(), e),
        };
        let Some(b) = self.me.upgrade() else { return };
        self.ctx.rt.spawn(async move {
            let probed = match (&target, &page) {
                (Some(t), Page::Port { path, .. }) => sites::probe(t, path).await.map(|(_, body)| (true, title(&body))),
                _ => probe(&b.http, &url).await,
            };
            if b.epoch.load(Ordering::Relaxed) != epoch {
                return; // navigated away meanwhile
            }
            match probed {
                Ok((framable, title)) => {
                    {
                        let mut st = b.state.lock().unwrap();
                        st.loading = false;
                        st.framable = Some(framable);
                        st.title = title.clone();
                    }
                    b.ctx.attention(Attention::Idle, "loaded");
                    b.ctx.event(EventKind::Navigated { url, title });
                    b.ctx.changed();
                }
                Err(e) => b.failed(epoch, &url, e),
            }
        });
    }

    /// The page couldn't be loaded: ask for you, and for a port, keep trying.
    fn failed(&self, epoch: u64, url: &str, error: String) {
        info!(block = self.ctx.id, url, error, "page didn't load");
        {
            let mut st = self.state.lock().unwrap();
            st.loading = false;
            st.error = Some(error.clone());
        }
        self.log(&json!({ "e": "load_error", "url": url, "error": error }));
        self.ctx.attention(Attention::NeedsInput, format!("couldn't load {url}: {error}"));
        self.ctx.event(EventKind::LoadError { url: url.to_owned(), error });
        self.ctx.changed();
        if matches!(*self.page.lock().unwrap(), Page::Port { .. }) {
            self.retry(epoch);
        }
    }

    /// Try the port every few seconds until it answers (the dev server is
    /// starting, or was restarted), then show the page again.
    fn retry(&self, epoch: u64) {
        if self.retrying.swap(true, Ordering::Relaxed) {
            return;
        }
        let Some(b) = self.me.upgrade() else { return };
        self.ctx.rt.spawn(async move {
            loop {
                tokio::time::sleep(RETRY).await;
                let live = !b.closed.load(Ordering::Relaxed) && b.epoch.load(Ordering::Relaxed) == epoch;
                if !live || b.state.lock().unwrap().error.is_none() {
                    break;
                }
                let target = b.site.lock().unwrap().as_ref().and_then(|s| s.target());
                let path = b.state.lock().unwrap().path.clone().unwrap_or_else(|| "/".into());
                // No site (block sites are off): nothing to try.
                let Some(t) = target else { break };
                if let Ok((_, body)) = sites::probe(&t, &path).await {
                    b.recovered(Some(title(&body)));
                    break;
                }
            }
            b.retrying.store(false, Ordering::Relaxed);
        });
    }

    /// The port answers again: show the page where it was.
    fn recovered(&self, title: Option<Option<String>>) {
        let url = {
            let mut st = self.state.lock().unwrap();
            if st.error.is_none() {
                return;
            }
            st.error = None;
            st.loading = false;
            st.framable = Some(true);
            st.src = st.url.clone();
            st.reloads += 1;
            if let Some(t) = title {
                st.title = t;
            }
            st.url.clone()
        };
        info!(block = self.ctx.id, url, "page is back");
        self.ctx.attention(Attention::Idle, "loaded");
        self.ctx.changed();
    }

    /// What the proxy saw.
    fn reported(&self, r: Report) {
        let epoch = self.epoch.load(Ordering::Relaxed);
        match r {
            Report::Navigated(path) => {
                let (port, site) = {
                    let st = self.state.lock().unwrap();
                    if st.path.as_deref() == Some(path.as_str()) {
                        return;
                    }
                    (st.port, self.site.lock().unwrap().clone())
                };
                let (Some(port), Some(site)) = (port, site) else { return };
                let url = site.url(&path);
                {
                    let mut st = self.state.lock().unwrap();
                    st.path = Some(path.clone());
                    st.url = url.clone();
                    st.title = None;
                }
                *self.page.lock().unwrap() = Page::Port { port, path: path.clone() };
                self.log(&json!({ "e": "navigated", "to": format!(":{port}{path}") }));
                self.ctx.changed();
                // Its title, from the page itself.
                let Some(b) = self.me.upgrade() else { return };
                let Some(t) = site.target() else { return };
                self.ctx.rt.spawn(async move {
                    let title = sites::probe(&t, &path).await.ok().and_then(|(_, body)| title(&body));
                    if b.state.lock().unwrap().url != url {
                        return;
                    }
                    b.state.lock().unwrap().title = title.clone();
                    b.ctx.event(EventKind::Navigated { url, title });
                    b.ctx.changed();
                });
            }
            Report::Unreachable(e) => {
                let url = self.state.lock().unwrap().url.clone();
                if self.state.lock().unwrap().error.is_none() {
                    self.failed(epoch, &url, e);
                }
            }
            Report::Reached => self.recovered(None),
        }
    }

    fn log(&self, event: &Value) {
        if let Ok(mut log) = self.ctx.log() {
            let mut line = event.to_string().into_bytes();
            line.push(b'\n');
            let _ = log.append(&line);
        }
    }
}

impl Block for Browser {
    fn kind(&self) -> BlockType {
        BlockType::Browser
    }

    fn config(&self) -> Value {
        match &*self.page.lock().unwrap() {
            Page::Web(url) => json!({ "url": url }),
            Page::Port { port, path } => json!({ "port": port, "path": path, "key": self.key }),
        }
    }

    fn state(&self) -> Value {
        serde_json::to_value(&*self.state.lock().unwrap()).unwrap_or_default()
    }

    fn text(&self) -> String {
        let st = self.state.lock().unwrap();
        let shown = self.page.lock().unwrap().show();
        match &st.title {
            Some(t) => format!("{t}\n{shown}\n"),
            None => format!("{shown}\n"),
        }
    }

    fn call(&self, method: &str, args: Value) -> BoxFuture<'static, Result<Value, String>> {
        let result = match method {
            "navigate" => {
                let port_now = self.state.lock().unwrap().port;
                match args["url"].as_str().map(|u| Page::parse(u, port_now)) {
                    Some(Ok(page)) => usable(&page, self.ctx.sprite.is_some()).map(|()| {
                        self.go(page, true);
                        json!({})
                    }),
                    Some(Err(e)) => Err(e),
                    None => Err("navigate needs {\"url\": …}".into()),
                }
            }
            "reload" => {
                let page = self.page.lock().unwrap().clone();
                self.state.lock().unwrap().reloads += 1;
                self.go(page, false);
                Ok(json!({}))
            }
            "back" => {
                let prev = {
                    let mut st = self.state.lock().unwrap();
                    st.back.pop();
                    st.back_pages.pop()
                };
                match prev {
                    Some(page) => {
                        self.go(page, false);
                        Ok(json!({}))
                    }
                    None => Err("nothing to go back to".into()),
                }
            }
            "state" => Ok(self.state()),
            m => Err(no_method(BlockType::Browser, m)),
        };
        Box::pin(async move { result })
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Relaxed);
        if let (Some(sites), Some(_)) = (sites::get(), self.site.lock().unwrap().take()) {
            sites.close(self.ctx.id);
        }
    }
}

/// Whether a page can be shown at all: ports need block sites, and this
/// host's ports exclude the daemon's own.
fn usable(page: &Page, on_machine: bool) -> Result<(), String> {
    let Page::Port { port, .. } = page else { return Ok(()) };
    let sites = sites::get().ok_or("browser blocks on ports are off: start illogicald with --block-listen")?;
    if on_machine { Ok(()) } else { sites.allowed(&Target::Local(*port)) }
}

/// A random name part: 20 letters and digits (about 103 bits).
pub(crate) fn new_key() -> String {
    let b = crate::push::random::<20>();
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    b.iter().map(|x| ALPHABET[*x as usize % ALPHABET.len()] as char).collect()
}

/// `example.com` means `https://example.com`; only http(s) pages.
fn normalize(url: &str) -> Result<String, String> {
    let url = url.trim();
    let full = if url.contains("://") { url.to_owned() } else { format!("https://{url}") };
    match reqwest::Url::parse(&full) {
        Ok(u) if u.scheme() == "http" || u.scheme() == "https" => Ok(u.to_string()),
        Ok(u) => Err(format!("{} pages can't be shown", u.scheme())),
        Err(e) => Err(format!("not a URL: {e}")),
    }
}

/// Whether a page allows framing by other sites, and its title.
async fn probe(http: &reqwest::Client, url: &str) -> Result<(bool, Option<String>), String> {
    let res = http.get(url).send().await.map_err(|e| e.without_url().to_string())?;
    let h = res.headers();
    let xfo = h.get("x-frame-options").and_then(|v| v.to_str().ok()).unwrap_or("").to_ascii_lowercase();
    let csp = h.get("content-security-policy").and_then(|v| v.to_str().ok()).unwrap_or("").to_ascii_lowercase();
    let framable = framable(&xfo, &csp);
    let body = res.text().await.unwrap_or_default();
    Ok((framable, title(&body)))
}

fn framable(xfo: &str, csp: &str) -> bool {
    if xfo.contains("deny") || xfo.contains("sameorigin") {
        return false;
    }
    match csp.split(';').map(str::trim).find_map(|d| d.strip_prefix("frame-ancestors")) {
        Some(sources) => sources.split_whitespace().any(|s| s == "*" || s == "https:"),
        None => true,
    }
}

fn title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<title")?;
    let open = start + lower[start..].find('>')? + 1;
    let end = open + lower[open..].find("</title")?;
    let t = html[open..end].split_whitespace().collect::<Vec<_>>().join(" ");
    (!t.is_empty()).then_some(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framing_rules() {
        assert!(framable("", ""));
        assert!(!framable("deny", ""));
        assert!(!framable("sameorigin", ""));
        assert!(!framable("", "default-src 'self'; frame-ancestors 'self'"));
        assert!(!framable("", "frame-ancestors 'none'"));
        assert!(framable("", "frame-ancestors *"));
        assert!(framable("", "default-src 'self'"));
    }

    #[test]
    fn urls_and_titles() {
        assert_eq!(normalize("example.com").unwrap(), "https://example.com/");
        assert!(normalize("file:///etc/passwd").is_err());
        assert_eq!(title("<html><head><TITLE>\n  Hi  there </title>").as_deref(), Some("Hi there"));
        assert_eq!(title("<p>none</p>"), None);
    }

    #[test]
    fn what_people_type() {
        let port = |p: u16, path: &str| Page::Port { port: p, path: path.into() };
        assert_eq!(Page::parse(":5173", None).unwrap(), port(5173, "/"));
        assert_eq!(Page::parse(" :5173/a/b?c=1 ", None).unwrap(), port(5173, "/a/b?c=1"));
        assert_eq!(Page::parse("/about", Some(3000)).unwrap(), port(3000, "/about"));
        assert!(Page::parse(":0", None).is_err());
        assert!(Page::parse(":99999", None).is_err());
        assert!(Page::parse(":5173x", None).is_err());
        assert_eq!(Page::parse("example.com", Some(3000)).unwrap(), Page::Web("https://example.com/".into()));
        assert_eq!(port(5173, "/x").show(), ":5173/x");
        let k = new_key();
        assert_eq!(k.len(), 20);
        assert_ne!(k, new_key());
    }
}
