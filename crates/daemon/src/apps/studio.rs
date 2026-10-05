//! A studio (arugula-salad's): where a person's app boxes are listed, and
//! where the owner's way into a box is minted (M35).
//!
//! illogicald holds one studio token, saved with the daemon's secrets
//! (`studio.json`, mode 0600) and never sent to a client. It's set with
//! `illogical studio login <url>` and gone with `illogical studio logout`.
//! Beside it, per app, a hud follower link (`illogical studio follower
//! APP`), when the box's owner made one with `hud share --role follower`.
//!
//! studio's contract (arugula-salad/studio#292, personal tokens
//! `studio_pat_…`), both with `Authorization: Bearer <token>`:
//!
//! - `GET <studio>/api/apps`: `{apps: [{name, title, url, createdAt}]}`,
//!   `url` the box's. Read tolerantly: a bare array, and the box as
//!   `box.url` (studio's own `/api/me` shape), `boxUrl` or `host`, are
//!   taken too; `title` is the block's label, and `box.status` is shown
//!   when there.
//! - `POST <studio>/api/apps/:name/open`: `{url}` (or `{link}`), the
//!   owner's ten-minute `/__enter?e=…&k=…` link. A deep link adds `&to=`
//!   (one of hud's pages), which the box's door follows once inside.
//!
//! Entry links are handed straight on (to a frame, or to the follower) and
//! never kept: not here, not in a block's config, log or state.

use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::{info, warn};

/// What's saved: the studio, its token, and follower links by app.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Saved {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    token: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    followers: BTreeMap<String, String>,
}

/// One of the person's apps, as studio lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AppInfo {
    pub name: String,
    pub title: Option<String>,
    /// The box's origin (`https://name.studio.example`).
    pub url: String,
    pub status: Option<String>,
}

pub struct Studio {
    file: PathBuf,
    saved: Mutex<Saved>,
    http: reqwest::Client,
}

static STUDIO: OnceLock<Arc<Studio>> = OnceLock::new();

/// Set up the daemon's studio from its secrets file.
pub fn install(file: PathBuf) -> Arc<Studio> {
    STUDIO.get_or_init(|| Arc::new(Studio::open(file))).clone()
}

pub fn get() -> Option<Arc<Studio>> {
    STUDIO.get().cloned()
}

impl Studio {
    pub fn open(file: PathBuf) -> Self {
        let saved = match std::fs::read(&file) {
            Ok(b) => serde_json::from_slice(&b).unwrap_or_else(|e| {
                warn!(file = %file.display(), error = %e, "studio secrets unreadable; logged out");
                Saved::default()
            }),
            Err(_) => Saved::default(),
        };
        let http = crate::roots::http()
            .timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("an HTTP client");
        Self { file, saved: Mutex::new(saved), http }
    }

    fn save(&self, s: &Saved) -> Result<(), String> {
        if let Some(dir) = self.file.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("can't save the studio token: {e}"))?;
        }
        let bytes = serde_json::to_vec_pretty(s).map_err(|e| e.to_string())?;
        crate::store::write_atomic(&self.file, &bytes).map_err(|e| format!("can't save the studio token: {e}"))
    }

    /// What a client may know: which studio, whether there's a token, and
    /// which apps have a follower link. Never the token or a link.
    pub fn status(&self) -> Value {
        let s = self.saved.lock().unwrap();
        json!({
            "url": s.url,
            "logged_in": s.token.is_some(),
            "followers": s.followers.keys().collect::<Vec<_>>(),
        })
    }

    /// The studio it's logged in to, if any.
    pub fn url(&self) -> Option<String> {
        let s = self.saved.lock().unwrap();
        s.token.as_ref().and(s.url.clone())
    }

    fn creds(&self) -> Result<(String, String), String> {
        let s = self.saved.lock().unwrap();
        match (&s.url, &s.token) {
            (Some(u), Some(t)) => Ok((u.clone(), t.clone())),
            _ => Err("not logged in to a studio: `illogical studio login <url>`".into()),
        }
    }

    /// Keep `token` for `url`, once studio takes it (it lists the apps).
    pub async fn login(&self, url: &str, token: &str) -> Result<Vec<AppInfo>, String> {
        let url = studio_url(url)?;
        let token = token.trim();
        if token.is_empty() || token.contains(char::is_whitespace) {
            return Err("that isn't a token".into());
        }
        let apps = list(&self.http, &url, token).await?;
        let mut s = self.saved.lock().unwrap().clone();
        if s.url.as_deref() != Some(url.as_str()) {
            s.followers.clear();
        }
        s.url = Some(url.clone());
        s.token = Some(token.to_owned());
        self.save(&s)?;
        *self.saved.lock().unwrap() = s;
        info!(studio = url, apps = apps.len(), "logged in to a studio");
        Ok(apps)
    }

    /// Forget the token (and every follower link).
    pub fn logout(&self) -> Result<(), String> {
        let s = Saved { url: self.saved.lock().unwrap().url.clone(), ..Saved::default() };
        self.save(&s)?;
        *self.saved.lock().unwrap() = s;
        info!("logged out of the studio");
        Ok(())
    }

    /// The person's apps.
    pub async fn apps(&self) -> Result<Vec<AppInfo>, String> {
        let (url, token) = self.creds()?;
        list(&self.http, &url, &token).await
    }

    /// One app, by name.
    pub async fn app(&self, name: &str) -> Result<AppInfo, String> {
        self.apps().await?.into_iter().find(|a| a.name == name).ok_or_else(|| format!("no app {name} in the studio"))
    }

    /// A fresh entry link into `app`'s box from `studio` (the block's), to
    /// hud's page `to` if given. Used once and dropped.
    pub async fn enter_link(&self, studio: &str, app: &str, to: Option<&str>) -> Result<String, String> {
        let (url, token) = self.creds()?;
        if studio_url(studio)? != url {
            return Err(format!("logged in to {url}, not {studio}: `illogical studio login {studio}`"));
        }
        let res = self
            .http
            .post(format!("{url}/api/apps/{}/open", enc(app)))
            .bearer_auth(&token)
            .json(&json!({}))
            .send()
            .await
            .map_err(|e| format!("studio: {}", e.without_url()))?;
        let status = res.status();
        let body: Value = res.json().await.unwrap_or_default();
        if !status.is_success() {
            return Err(refused(status.as_u16(), &body));
        }
        let link = body["url"].as_str().or(body["link"].as_str()).ok_or("studio's answer has no link")?;
        let link = reqwest::Url::parse(link).map_err(|e| format!("studio's link: {e}"))?;
        if !matches!(link.scheme(), "http" | "https") {
            return Err("studio's link isn't http(s)".into());
        }
        Ok(with_to(link, to))
    }

    /// The follower link kept for `app`, if any.
    pub fn follower(&self, app: &str) -> Option<String> {
        self.saved.lock().unwrap().followers.get(app).cloned()
    }

    /// Keep (or forget, with `None`) a follower link for `app`.
    pub fn set_follower(&self, app: &str, link: Option<&str>) -> Result<(), String> {
        let mut s = self.saved.lock().unwrap().clone();
        match link.map(str::trim) {
            Some(l) => {
                let u = reqwest::Url::parse(l).map_err(|e| format!("not a link: {e}"))?;
                if !matches!(u.scheme(), "http" | "https") {
                    return Err("a follower link is http(s)".into());
                }
                s.followers.insert(app.to_owned(), l.to_owned());
            }
            None => {
                s.followers.remove(app);
            }
        }
        self.save(&s)?;
        *self.saved.lock().unwrap() = s;
        Ok(())
    }
}

/// A studio's base URL: `studio.example` means `https://studio.example`;
/// no path, query or trailing slash.
pub fn studio_url(s: &str) -> Result<String, String> {
    let s = s.trim();
    let full = if s.contains("://") { s.to_owned() } else { format!("https://{s}") };
    let u = reqwest::Url::parse(&full).map_err(|e| format!("not a studio URL: {e}"))?;
    if !matches!(u.scheme(), "http" | "https") || u.host_str().is_none() {
        return Err(format!("not a studio URL: {s}"));
    }
    Ok(u.as_str().trim_end_matches('/').split(['?', '#']).next().unwrap_or_default().to_owned())
}

/// A box's origin from what studio says of it (`https://x.example/`,
/// or a bare host).
pub fn box_origin(s: &str) -> Result<String, String> {
    let s = s.trim();
    let full = if s.contains("://") { s.to_owned() } else { format!("https://{s}") };
    let u = reqwest::Url::parse(&full).map_err(|e| format!("not a box URL: {e}"))?;
    if !matches!(u.scheme(), "http" | "https") || u.host_str().is_none() {
        return Err(format!("not a box URL: {s}"));
    }
    Ok(u.origin().ascii_serialization())
}

/// hud's pages a deep link may land on (the door follows only these).
pub fn hud_page(to: &str) -> bool {
    let Some(rest) = to.strip_prefix("/__hud/") else { return false };
    let page = rest.split(['/', '?', '#']).next().unwrap_or_default();
    matches!(page, "decisions" | "work" | "intent" | "sessions")
        && to.chars().all(|c| c.is_ascii_alphanumeric() || "/_-.=&%?#".contains(c))
}

fn with_to(mut link: reqwest::Url, to: Option<&str>) -> String {
    if let Some(to) = to
        && !link.query_pairs().any(|(k, _)| k == "to")
    {
        link.query_pairs_mut().append_pair("to", to);
    }
    link.to_string()
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn refused(status: u16, body: &Value) -> String {
    let said = body["error"].as_str().or(body["message"].as_str());
    match (status, said) {
        (401 | 403, _) => "studio refused the token: log in again (`illogical studio login`)".into(),
        (404, _) => "studio has no such app".into(),
        (_, Some(m)) => format!("studio: {m} ({status})"),
        _ => format!("studio answered {status}"),
    }
}

async fn list(http: &reqwest::Client, url: &str, token: &str) -> Result<Vec<AppInfo>, String> {
    let res = http
        .get(format!("{url}/api/apps"))
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("studio: {}", e.without_url()))?;
    let status = res.status();
    let body: Value = res.json().await.map_err(|_| format!("studio's app list isn't JSON ({status})"))?;
    if !status.is_success() {
        return Err(refused(status.as_u16(), &body));
    }
    Ok(parse_apps(&body))
}

/// Apps from studio's answer, tolerantly (see the module's contract);
/// entries without a name or a box are skipped.
pub fn parse_apps(body: &Value) -> Vec<AppInfo> {
    let list = body.get("apps").and_then(Value::as_array).or(body.as_array());
    list.into_iter()
        .flatten()
        .filter_map(|a| {
            let name = a["name"].as_str().filter(|n| !n.is_empty())?.to_owned();
            let url = ["url", "boxUrl", "box_url", "host"]
                .iter()
                .find_map(|k| a[*k].as_str())
                .or(a["box"]["url"].as_str())
                .or(a["box"].as_str())?;
            Some(AppInfo {
                title: a["title"].as_str().map(str::to_owned),
                url: box_origin(url).ok()?,
                status: a["box"]["status"].as_str().or(a["status"].as_str()).map(str::to_owned),
                name,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apps_in_every_shape() {
        let me = json!({ "apps": [
            { "name": "pinboard", "title": "Pinboard", "box": { "url": "https://pinboard.studio.example", "status": "running" } },
            { "name": "nobox" },
            { "title": "no name", "url": "https://x.example" },
        ]});
        assert_eq!(
            parse_apps(&me),
            vec![AppInfo {
                name: "pinboard".into(),
                title: Some("Pinboard".into()),
                url: "https://pinboard.studio.example".into(),
                status: Some("running".into()),
            }]
        );
        let bare = json!([{ "name": "a", "url": "http://127.0.0.1:9/x?y" }, { "name": "b", "host": "b.example" }]);
        let apps = parse_apps(&bare);
        assert_eq!(apps[0].url, "http://127.0.0.1:9");
        assert_eq!(apps[1].url, "https://b.example");
    }

    #[test]
    fn studios_token_list() {
        let v = json!({ "apps": [{ "name": "app-1a2b3c4d", "title": "Pinboard", "url": "https://app-1a2b3c4d.example.test", "createdAt": 1 }] });
        assert_eq!(
            parse_apps(&v),
            vec![AppInfo {
                name: "app-1a2b3c4d".into(),
                title: Some("Pinboard".into()),
                url: "https://app-1a2b3c4d.example.test".into(),
                status: None,
            }]
        );
    }

    #[test]
    fn urls_pages_and_deep_links() {
        assert_eq!(studio_url("studio.example/").unwrap(), "https://studio.example");
        assert_eq!(studio_url("http://127.0.0.1:18091").unwrap(), "http://127.0.0.1:18091");
        assert!(studio_url("ftp://x").is_err());
        assert!(hud_page("/__hud/work"));
        assert!(hud_page("/__hud/decisions/d-1?x=1"));
        assert!(!hud_page("/__hud/api/tabs"));
        assert!(!hud_page("/admin"));
        assert!(!hud_page("/__hud/work\"><script>"));
        let link = reqwest::Url::parse("https://b.example/__enter?e=1&k=2").unwrap();
        assert_eq!(with_to(link.clone(), Some("/__hud/work")), "https://b.example/__enter?e=1&k=2&to=%2F__hud%2Fwork");
        assert_eq!(with_to(link, None), "https://b.example/__enter?e=1&k=2");
    }

    #[test]
    fn the_token_is_kept_private_and_never_shown() {
        let dir = std::env::temp_dir().join(format!("ilg-studio-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("studio.json");
        let s = Studio::open(file.clone());
        let saved = Saved { url: Some("https://s.example".into()), token: Some("sekrit".into()), ..Saved::default() };
        s.save(&saved).unwrap();
        *s.saved.lock().unwrap() = saved;
        s.set_follower("pinboard", Some("https://p.example/__hud/join?t=f")).unwrap();
        let mode = std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&file).unwrap().permissions());
        assert_eq!(mode & 0o777, 0o600);
        let shown = s.status().to_string();
        assert!(!shown.contains("sekrit") && !shown.contains("join"), "{shown}");
        assert_eq!(s.status()["followers"], json!(["pinboard"]));
        let again = Studio::open(file.clone());
        assert_eq!(again.url().as_deref(), Some("https://s.example"));
        assert!(again.follower("pinboard").is_some());
        again.logout().unwrap();
        assert!(!std::fs::read_to_string(&file).unwrap().contains("sekrit"));
        assert!(again.follower("pinboard").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
