//! illogical control (`illogical-control`): accounts, devices, the
//! directory and the relay, for people who don't run a tailnet and for
//! teams. Anyone can run it; the hosted one runs this code.
//!
//! It holds metadata only. Terminal bytes travel end to end between a
//! client device and a daemon (docs/control-e2e.md): control introduces
//! them and relays opaque messages, and the keys it distributes are signed
//! by the account's own devices, so it can refuse service but can't read.

mod api;
mod auth;
mod db;
mod limit;
mod passkey;
mod push;
mod relay;
mod teams;

use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use axum::{
    Json, Router,
    body::Body,
    http::{HeaderValue, StatusCode, Uri, header},
    response::{IntoResponse, Response},
    routing::{get, post},
    serve::ListenerExt,
};
use clap::Parser;
use rust_embed::Embed;
use serde_json::json;
use tracing::info;

#[derive(Parser, Debug)]
#[command(version, about = "illogical control: accounts, devices, the directory and the relay")]
struct Args {
    /// Address to listen on (put TLS in front: Caddy, Fly, `tailscale serve`).
    #[arg(long, default_value = "127.0.0.1:7690", env = "ILLOGICAL_CONTROL_LISTEN")]
    listen: SocketAddr,

    /// The URL people and daemons reach this at, without a trailing slash
    /// (`https://control.example.com`).
    #[arg(long, default_value = "http://127.0.0.1:7690", env = "ILLOGICAL_CONTROL_URL")]
    public_url: String,

    /// The SQLite database.
    #[arg(long, default_value = "control.db", env = "ILLOGICAL_CONTROL_DB")]
    db: PathBuf,

    /// GitHub App (or OAuth app) client id, for signing in with GitHub.
    #[arg(long, env = "GITHUB_CLIENT_ID")]
    github_client_id: Option<String>,

    #[arg(long, env = "GITHUB_CLIENT_SECRET", hide_env_values = true)]
    github_client_secret: Option<String>,

    /// GitHub's web and API bases (tests point these at a fake).
    #[arg(long, default_value = "https://github.com", hide = true)]
    github_url: String,
    #[arg(long, default_value = "https://api.github.com", hide = true)]
    github_api: String,

    /// Push endpoints allowed besides the browsers' push services, as
    /// host:port (tests).
    #[arg(long = "push-host", hide = true)]
    push_hosts: Vec<String>,

    /// Behind a proxy that puts the client's IP in a header (Fly:
    /// `Fly-Client-IP`), use it for rate limits. Only set this when every
    /// request comes through that proxy.
    #[arg(long, env = "ILLOGICAL_CONTROL_PROXY_HEADER")]
    trust_proxy_header: Option<String>,

    /// Serve the web client from this directory instead of the built-in
    /// copy (development).
    #[arg(long)]
    static_dir: Option<PathBuf>,
}

pub struct Github {
    pub client_id: String,
    pub client_secret: String,
    pub url: String,
    pub api: String,
}

pub struct Config {
    pub push_hosts: Vec<String>,
    pub public_url: String,
    /// `public_url`'s origin, as browsers send it.
    pub origin: String,
    pub github: Option<Github>,
    pub static_dir: Option<PathBuf>,
}

pub struct App {
    pub cfg: Config,
    pub db: db::Db,
    pub http: reqwest::Client,
    pub relay: relay::Relay,
    pub passkeys: passkey::Challenges,
    pub limits: limit::Limits,
    pub vapid: push::Vapid,
}

/// An API error: `{"error": "..."}` with a status.
#[derive(Debug)]
pub struct ApiError(StatusCode, String);

pub fn err(status: StatusCode, msg: &str) -> ApiError {
    ApiError(status, msg.to_owned())
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        tracing::warn!(error = %e, "internal error");
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, "something went wrong".into())
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(e: serde_json::Error) -> Self {
        anyhow::Error::from(e).into()
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

/// `YYYY-MM-DD` (UTC) for a time in ms, for daily meters.
pub fn day(ms: u64) -> String {
    let days = (ms / 86_400_000) as i64;
    // Civil from days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/control.json", get(control_json))
        .route("/auth/github", get(auth::github_start))
        .route("/auth/github/callback", get(auth::github_callback))
        .route("/auth/logout", post(auth::logout))
        .route("/auth/passkey/register", post(passkey::register_start))
        .route("/auth/passkey/register/finish", post(passkey::register_finish))
        .route("/auth/passkey/login", post(passkey::login_start))
        .route("/auth/passkey/login/finish", post(passkey::login_finish))
        .route("/api/me", get(api::me))
        .route("/api/devices", get(api::devices).post(api::enroll))
        .route("/api/devices/{id}", get(api::device))
        .route("/api/devices/{id}/approve", post(api::approve))
        .route("/api/devices/{id}/reject", post(api::reject))
        .route("/api/revocations", post(api::revoke))
        .route("/api/recovery", post(api::add_recovery))
        .route("/api/join", post(api::join))
        .route("/api/join/{code}", get(api::join_poll))
        .route("/api/joins/{code}", get(api::join_show))
        .route("/api/joins/{code}/approve", post(api::join_approve))
        .route("/api/daemon/trust", get(api::daemon_trust))
        .route("/api/daemon/leave", post(api::daemon_leave))
        .route("/api/directory", get(api::directory))
        .route("/api/people", get(teams::person))
        .route("/api/teams", get(teams::list).post(teams::create))
        .route("/api/teams/{id}/roster", post(teams::set_roster))
        .route("/api/teams/{id}/invites", post(teams::invite))
        .route("/api/teams/{id}/requests/{account}/reject", post(teams::reject))
        .route("/api/teams/{id}/lock", post(teams::lock))
        .route("/api/invites/{team}/{code}", get(teams::show_invite))
        .route("/api/invites/{team}/{code}/accept", post(teams::accept_invite))
        .route("/api/daemon/team", get(teams::daemon_team))
        .route("/api/daemon/peers", get(teams::daemon_peers))
        .route("/api/daemon/access", post(teams::daemon_access))
        .route("/api/relay/link/{id}", get(relay::link))
        .route("/api/push/subscribe", post(push::subscribe))
        .route("/api/push/unsubscribe", post(push::unsubscribe))
        .route("/api/daemon/push-subs", get(push::daemon_subs))
        .route("/api/daemon/push", post(push::daemon_send))
        .route("/api/relay/dial", get(relay::dial))
        .route("/api/relay/c/{id}", get(relay::client))
        .fallback(asset)
        .layer(axum::middleware::map_response(headers))
        .with_state(app)
}

async fn control_json(axum::extract::State(app): axum::extract::State<Arc<App>>) -> Json<serde_json::Value> {
    // Passkeys need a domain name: WebAuthn refuses IP addresses.
    let passkeys = url::Url::parse(&app.cfg.public_url).is_ok_and(|u| matches!(u.host(), Some(url::Host::Domain(_))));
    Json(json!({
        "control": true, "url": app.cfg.public_url, "github": app.cfg.github.is_some(), "passkeys": passkeys,
        "vapid": app.vapid.public(),
    }))
}

/// Nothing frames control's pages, and nothing on them comes from elsewhere
/// except the daemons the page connects to (any WebSocket).
async fn headers(mut res: Response) -> Response {
    let h = res.headers_mut();
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    h.entry(header::CONTENT_SECURITY_POLICY).or_insert(HeaderValue::from_static(
        "default-src 'self'; connect-src 'self' wss: ws: https:; img-src 'self' data: https:; style-src 'self' 'unsafe-inline'; frame-ancestors 'none'",
    ));
    res
}

#[derive(Embed)]
#[folder = "../../web/dist"]
#[allow_missing = true]
struct Assets;

async fn asset(axum::extract::State(app): axum::extract::State<Arc<App>>, uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    if path.split('/').any(|s| s == ".." || s.starts_with('.')) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let data = match &app.cfg.static_dir {
        Some(dir) => std::fs::read(dir.join(path)).ok().map(|d| (d, mime(path).to_owned())),
        None => Assets::get(path).map(|f| (f.data.into_owned(), f.metadata.mimetype().to_owned())),
    };
    match data {
        Some((data, mime)) => {
            let cache = if path.starts_with("assets/") { "public, max-age=31536000, immutable" } else { "no-cache" };
            Response::builder()
                .header(header::CONTENT_TYPE, mime)
                .header(header::CACHE_CONTROL, cache)
                .body(Body::from(data))
                .unwrap()
        }
        None if path == "index.html" => (StatusCode::NOT_FOUND, "web client not built: run `just web`").into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

fn mime(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript",
        Some("css") => "text/css",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("json" | "webmanifest") => "application/json",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

pub fn origin_of(url: &str) -> anyhow::Result<String> {
    let u = url::Url::parse(url)?;
    Ok(u.origin().ascii_serialization())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "illogical_control=info".into()),
        )
        .init();
    let a = Args::parse();
    let public_url = a.public_url.trim_end_matches('/').to_owned();
    let set = |v: Option<String>| v.filter(|s| !s.is_empty());
    let github = match (set(a.github_client_id), set(a.github_client_secret)) {
        (Some(client_id), Some(client_secret)) => {
            Some(Github { client_id, client_secret, url: a.github_url, api: a.github_api })
        }
        _ => None,
    };
    if github.is_none() {
        tracing::warn!("no GITHUB_CLIENT_ID/GITHUB_CLIENT_SECRET: GitHub sign-in is off");
    }
    let db = db::Db::open(&a.db)?;
    let vapid = push::Vapid::load(&db)?;
    let app = Arc::new(App {
        cfg: Config {
            push_hosts: a.push_hosts,
            origin: origin_of(&public_url)?,
            public_url,
            github,
            static_dir: a.static_dir,
        },
        db,
        http: reqwest::Client::builder().timeout(std::time::Duration::from_secs(15)).build()?,
        relay: Default::default(),
        passkeys: Default::default(),
        limits: limit::Limits::new(a.trust_proxy_header),
        vapid,
    });
    // Nagle off: the relay's mux writes frames back to back (S15).
    let l = tokio::net::TcpListener::bind(a.listen).await?.tap_io(|t| {
        let _ = t.set_nodelay(true);
    });
    info!(listen = %a.listen, url = %app.cfg.public_url, "illogical control");
    axum::serve(l, router(app).into_make_service_with_connect_info::<SocketAddr>()).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn days() {
        assert_eq!(super::day(0), "1970-01-01");
        assert_eq!(super::day(1_790_000_000_000), "2026-09-21");
        assert_eq!(super::day(951_782_400_000), "2000-02-29");
    }
}
