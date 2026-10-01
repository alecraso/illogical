//! Browser blocks (M6a): a web page beside your terminals.
//!
//! This is the part for ordinary pages (`{ "url": … }`): the client frames
//! the page if the site allows it and shows a card with "open in new tab"
//! if it doesn't. Pages on a machine's ports, through the daemon's own
//! proxy and per-block hostnames, build on this.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use futures_util::future::BoxFuture;
use illogical_proto::{Attention, BlockType};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::info;

use crate::block::{Block, BlockCtx, no_method};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Config {
    url: String,
}

#[derive(Debug, Clone, Default, Serialize)]
struct State {
    url: String,
    title: Option<String>,
    /// Whether the site lets itself be framed (`None` while checking).
    framable: Option<bool>,
    /// Why it couldn't be loaded, if it couldn't.
    error: Option<String>,
    loading: bool,
    /// Bumped by `reload`, so the client reloads the frame.
    reloads: u64,
    /// Where `back` goes.
    back: Vec<String>,
}

pub struct Browser {
    ctx: BlockCtx,
    state: Arc<Mutex<State>>,
    http: reqwest::Client,
}

impl Browser {
    pub fn create(ctx: BlockCtx, config: Value) -> Result<Arc<dyn Block>, String> {
        let config: Config = serde_json::from_value(config).map_err(|e| format!("browser config: {e}"))?;
        let url = normalize(&config.url)?;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .map_err(|e| e.to_string())?;
        let b = Arc::new(Self { ctx, state: Arc::new(Mutex::new(State::default())), http });
        b.go(url, false);
        Ok(b)
    }

    /// Show `url`, then find out whether it can be framed and what it's called.
    fn go(&self, url: String, push_back: bool) {
        {
            let mut st = self.state.lock().unwrap();
            if push_back && !st.url.is_empty() {
                let old = std::mem::take(&mut st.url);
                st.back.push(old);
            }
            st.url = url.clone();
            st.title = None;
            st.framable = None;
            st.error = None;
            st.loading = true;
        }
        self.log(&json!({ "e": "navigate", "url": url }));
        self.ctx.attention(Attention::Working, "loading");
        self.ctx.changed();
        let (state, ctx, http) = (self.state.clone(), self.ctx.clone(), self.http.clone());
        self.ctx.rt.spawn(async move {
            let probe = probe(&http, &url).await;
            let mut st = state.lock().unwrap();
            if st.url != url {
                return; // navigated away meanwhile
            }
            st.loading = false;
            match probe {
                Ok((framable, title)) => {
                    st.framable = Some(framable);
                    st.title = title;
                    drop(st);
                    ctx.attention(Attention::Idle, "loaded");
                }
                Err(e) => {
                    info!(block = ctx.id, url, error = %e, "page didn't load");
                    st.error = Some(e.clone());
                    drop(st);
                    ctx.attention(Attention::NeedsInput, format!("couldn't load {url}: {e}"));
                }
            }
            ctx.changed();
        });
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
        json!({ "url": self.state.lock().unwrap().url })
    }

    fn state(&self) -> Value {
        serde_json::to_value(&*self.state.lock().unwrap()).unwrap_or_default()
    }

    fn text(&self) -> String {
        let st = self.state.lock().unwrap();
        match &st.title {
            Some(t) => format!("{t}\n{}\n", st.url),
            None => format!("{}\n", st.url),
        }
    }

    fn call(&self, method: &str, args: Value) -> BoxFuture<'static, Result<Value, String>> {
        let result = match method {
            "navigate" => match args["url"].as_str().map(normalize) {
                Some(Ok(url)) => {
                    self.go(url, true);
                    Ok(json!({}))
                }
                Some(Err(e)) => Err(e),
                None => Err("navigate needs {\"url\": …}".into()),
            },
            "reload" => {
                let url = {
                    let mut st = self.state.lock().unwrap();
                    st.reloads += 1;
                    st.url.clone()
                };
                self.go(url, false);
                Ok(json!({}))
            }
            "back" => {
                let prev = self.state.lock().unwrap().back.pop();
                match prev {
                    Some(url) => {
                        self.go(url, false);
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

    fn close(&self) {}
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
}
