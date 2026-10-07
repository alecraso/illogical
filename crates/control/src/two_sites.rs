//! Control at two URLs while it moves (#507): each browser is served as the
//! site it came in on, daemons are answered at either, and once everyone
//! has moved the old site's pages send browsers on.

use std::{net::SocketAddr, sync::Arc};

use reqwest::{StatusCode, header};
use serde_json::{Value, json};

use crate::{App, Github, Site, auth::hash};

/// Control listening on loopback, at `http://new.test` and also
/// `http://old.test` (a request says which with its Host header).
struct Control {
    app: Arc<App>,
    base: String,
    http: reqwest::Client,
}

const NEW: &str = "http://new.test";
const OLD: &str = "http://old.test";

async fn control(tweak: impl FnOnce(&mut App)) -> Control {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let mut app = App::for_tests(NEW);
    app.cfg.sites.push(Site::new(OLD).unwrap());
    tweak(&mut app);
    let app = Arc::new(app);
    let svc = crate::router(app.clone()).into_make_service_with_connect_info::<SocketAddr>();
    tokio::spawn(async move { axum::serve(l, svc).await.unwrap() });
    let http = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
    Control { app, base, http }
}

impl Control {
    /// A request to `path` as if at `site` (`NEW` or `OLD`).
    fn at(&self, site: &str, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let host = site.trim_start_matches("http://");
        self.http.request(method, format!("{}{path}", self.base)).header(header::HOST, host)
    }

    fn session(&self, account: &str) -> String {
        let t = crate::auth::token();
        self.app.db.add_session(&hash(&t), account, arugula_e2e::now_ms(), arugula_e2e::now_ms() + 3_600_000).unwrap();
        format!("{}={t}", crate::auth::SESSION_COOKIE)
    }
}

#[test]
fn a_site_is_found_by_its_host() {
    let s = Site::new("https://Control.Example.com/").unwrap();
    assert_eq!(
        (s.url.as_str(), s.origin.as_str(), s.host()),
        ("https://Control.Example.com", "https://control.example.com", "control.example.com")
    );
    let p = Site::new("http://127.0.0.1:7690").unwrap();
    assert_eq!((p.authority.as_str(), p.host()), ("127.0.0.1:7690", "127.0.0.1"));

    let mut app = App::for_tests(NEW);
    app.cfg.sites.push(Site::new(OLD).unwrap());
    let with = |host: &str| {
        let mut h = axum::http::HeaderMap::new();
        h.insert(header::HOST, host.parse().unwrap());
        app.cfg.site(&h).url.clone()
    };
    assert_eq!(with("old.test"), OLD);
    assert_eq!(with("OLD.test"), OLD);
    assert_eq!(with("new.test"), NEW);
    // Anything else gets the first.
    assert_eq!(with("elsewhere.example"), NEW);
}

#[tokio::test]
async fn control_json_says_where_it_was_asked_and_where_it_is() {
    let c = control(|_| {}).await;
    for site in [NEW, OLD] {
        let j: Value = c.at(site, reqwest::Method::GET, "/control.json").send().await.unwrap().json().await.unwrap();
        assert_eq!(j["url"], site, "its own site's: a page's links and relay are there");
        assert_eq!(j["primary"], NEW);
        assert_eq!(j["urls"], json!([NEW, OLD]));
    }
}

#[tokio::test]
async fn a_browser_is_served_as_the_site_it_came_in_on() {
    let c = control(|_| {}).await;
    c.app.db.account_for("github", "gh-1", "sam", "a1", arugula_e2e::now_ms()).unwrap();
    let cookie = c.session("a1");
    let rename = |site: &'static str, origin: &'static str| {
        c.at(site, reqwest::Method::POST, "/api/me/name")
            .header(header::COOKIE, &cookie)
            .header(header::ORIGIN, origin)
            .json(&json!({ "name": "Sam" }))
            .send()
    };
    // Its own page, at either site.
    assert_eq!(rename(OLD, OLD).await.unwrap().status(), StatusCode::OK);
    assert_eq!(rename(NEW, NEW).await.unwrap().status(), StatusCode::OK);
    // One site's page doesn't act on the other.
    assert_eq!(rename(OLD, NEW).await.unwrap().status(), StatusCode::FORBIDDEN);
    assert_eq!(rename(NEW, OLD).await.unwrap().status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn signing_in_with_github_comes_back_to_its_site() {
    let c = control(|a| {
        a.cfg.github = Some(Github {
            client_id: "id".into(),
            client_secret: "secret".into(),
            url: "https://github.example".into(),
            api: "https://api.github.example".into(),
        })
    })
    .await;
    for site in [NEW, OLD] {
        let res = c.at(site, reqwest::Method::GET, "/auth/github").send().await.unwrap();
        let to = res.headers()[header::LOCATION].to_str().unwrap().to_owned();
        let to = url::Url::parse(&to).unwrap();
        let back = to.query_pairs().find(|(k, _)| k == "redirect_uri").unwrap().1.into_owned();
        assert_eq!(back, format!("{site}/auth/github/callback"));
    }
}

#[tokio::test]
async fn once_moved_the_old_sites_pages_send_browsers_on() {
    let c = control(|a| a.cfg.also_redirect = true).await;
    let get = |site: &'static str, path: &'static str| c.at(site, reqwest::Method::GET, path).send();
    let res = get(OLD, "/settings?x=1").await.unwrap();
    assert_eq!(res.status(), StatusCode::PERMANENT_REDIRECT);
    assert_eq!(res.headers()[header::LOCATION], format!("{NEW}/settings?x=1"));
    // The new site's own pages aren't moved.
    assert_ne!(get(NEW, "/").await.unwrap().status(), StatusCode::PERMANENT_REDIRECT);
    // Daemons, open pages and sign-ins under way are still answered at
    // the old site.
    for path in ["/control.json", "/api/me", "/auth/github/callback"] {
        assert_ne!(get(OLD, path).await.unwrap().status(), StatusCode::PERMANENT_REDIRECT, "{path}");
    }
    let posted = c.at(OLD, reqwest::Method::POST, "/api/me/name").json(&json!({})).send().await.unwrap();
    assert_ne!(posted.status(), StatusCode::PERMANENT_REDIRECT);
}
