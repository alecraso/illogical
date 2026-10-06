//! illogical control (`illogical-control`): accounts, devices, the
//! directory and the relay, for people who don't run a tailnet and for
//! teams. Anyone can run it; the hosted one runs this code.
//!
//! It holds metadata only. Terminal bytes travel end to end between a
//! client device and a daemon (docs/control-e2e.md): control introduces
//! them and relays opaque messages, and the keys it distributes are signed
//! by the account's own devices, so it can refuse service but can't read.

mod account;
mod api;
mod app_login;
mod auth;
mod backup;
mod billing;
mod db;
mod forge;
#[cfg(test)]
mod forge_wire;
mod guest_jump;
mod limit;
mod passkey;
mod push;
#[cfg(test)]
mod push_notices;
mod relay;
mod roots;
#[cfg(test)]
mod routing_wire;
mod sandboxes;
mod sprites;
mod teams;
mod turn;

use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use anyhow::Context as _;
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
    /// Port 0 picks a free one, recorded in `listen` beside the database.
    #[arg(long, default_value = "127.0.0.1:7690", env = "ILLOGICAL_CONTROL_LISTEN")]
    listen: SocketAddr,

    /// The URL people and daemons reach this at, without a trailing slash
    /// (`https://control.example.com`). Port 0 is the port --listen got.
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

    /// The GitHub App (M40) whose webhooks come to /github/webhook and
    /// whose installation tokens let hosted boxes read: its id, slug,
    /// webhook secret and private key (a PEM file, or the PEM itself in
    /// GITHUB_APP_PRIVATE_KEY, as a Fly secret). Its client id and secret
    /// sign people in when GITHUB_CLIENT_ID/SECRET aren't set. Off without
    /// an id and a key.
    #[arg(long, env = "GITHUB_APP_ID")]
    github_app_id: Option<String>,
    #[arg(long, env = "GITHUB_APP_SLUG", default_value = "illogical")]
    github_app_slug: String,
    #[arg(long, env = "GITHUB_APP_CLIENT_ID")]
    github_app_client_id: Option<String>,
    #[arg(long, env = "GITHUB_APP_CLIENT_SECRET", hide_env_values = true)]
    github_app_client_secret: Option<String>,
    #[arg(long, env = "GITHUB_APP_WEBHOOK_SECRET", hide_env_values = true)]
    github_app_webhook_secret: Option<String>,
    #[arg(long, env = "GITHUB_APP_PRIVATE_KEY_FILE")]
    github_app_private_key_file: Option<PathBuf>,
    #[arg(long, env = "GITHUB_APP_PRIVATE_KEY", hide_env_values = true)]
    github_app_private_key: Option<String>,
    /// How often subscribed daemons hear the webhook path is up (tests).
    #[arg(long, default_value_t = 60, hide = true)]
    forge_heartbeat_secs: u64,

    /// Hosted sandboxes (M20): the Sprites API they're made with, and its
    /// token (SPRITES_TOKEN). Off without a token.
    #[arg(long, default_value = "https://api.sprites.dev", env = "ILLOGICAL_SPRITES_URL")]
    sprites_url: String,
    #[arg(long, env = "SPRITES_TOKEN", hide_env_values = true)]
    sprites_token: Option<String>,

    /// TURN for huddles (M63): a Cloudflare TURN key's id and API token.
    /// Without them, daemons get public STUN only.
    #[arg(long, env = "CLOUDFLARE_TURN_KEY_ID", hide_env_values = true)]
    turn_key_id: Option<String>,
    #[arg(long, env = "CLOUDFLARE_TURN_API_TOKEN", hide_env_values = true)]
    turn_api_token: Option<String>,
    #[arg(long, default_value = "https://rtc.live.cloudflare.com", env = "ILLOGICAL_TURN_API", hide = true)]
    turn_api: String,
    /// The static daemon (x86_64 musl) to put in them.
    #[arg(long, default_value = "/illogicald", env = "ILLOGICAL_SANDBOX_BINARY")]
    sandbox_binary: PathBuf,
    /// Accounts that may make them, comma-separated (`*`: everyone).
    #[arg(long, env = "ILLOGICAL_SANDBOX_ACCOUNTS", value_delimiter = ',')]
    sandbox_accounts: Vec<String>,
    /// How many each may have at once.
    #[arg(long, default_value_t = 2, env = "ILLOGICAL_SANDBOX_QUOTA")]
    sandbox_quota: usize,

    /// Billing (M22): a Stripe secret key turns it on (STRIPE_SECRET_KEY);
    /// webhooks are checked with STRIPE_WEBHOOK_SECRET.
    #[arg(long, env = "STRIPE_SECRET_KEY", hide_env_values = true)]
    stripe_secret: Option<String>,
    #[arg(long, env = "STRIPE_WEBHOOK_SECRET", hide_env_values = true)]
    stripe_webhook_secret: Option<String>,
    #[arg(long, default_value = "https://api.stripe.com", env = "ILLOGICAL_STRIPE_API")]
    stripe_api: String,
    /// The per-seat price, and the metered price and meter event for
    /// sandbox minutes.
    #[arg(long, env = "ILLOGICAL_STRIPE_SEAT_PRICE", default_value = "")]
    stripe_seat_price: String,
    #[arg(long, env = "ILLOGICAL_STRIPE_MINUTES_PRICE", default_value = "")]
    stripe_minutes_price: String,
    #[arg(long, env = "ILLOGICAL_STRIPE_MINUTES_EVENT", default_value = "sandbox_minutes")]
    stripe_minutes_event: String,
    /// Free relay traffic per account per month, in MB (with billing on).
    #[arg(long, default_value_t = 10_000, env = "ILLOGICAL_RELAY_FREE_MB")]
    relay_free_mb: u64,

    /// Relay limits per account (#174), 0 for none: client sockets at
    /// once, machines dialed in at once, and (while billing is off) MB a
    /// day before its relayed traffic slows down.
    #[arg(long, default_value_t = 32, env = "ILLOGICAL_RELAY_MAX_SOCKETS")]
    relay_max_sockets: usize,
    #[arg(long, default_value_t = 50, env = "ILLOGICAL_RELAY_MAX_MACHINES")]
    relay_max_machines: usize,
    #[arg(long, default_value_t = 2_000, env = "ILLOGICAL_RELAY_DAILY_MB")]
    relay_daily_mb: u64,
    /// Relay sockets at once, every account's together (#344), 0 for no
    /// limit. Keep it below the proxy's connection limit (Fly's
    /// `soft_limit`), so a full relay still leaves room for pages and
    /// sign-ins.
    #[arg(long, default_value_t = 5_000, env = "ILLOGICAL_RELAY_MAX_TOTAL")]
    relay_max_total: usize,

    /// Off-site backup with Litestream (#174).
    #[command(flatten)]
    backup: backup::Litestream,

    /// Push endpoints allowed besides the browsers' push services, as
    /// host:port (tests).
    #[arg(long = "push-host", hide = true)]
    push_hosts: Vec<String>,

    /// Behind a proxy that puts the client's IP in a header (Fly:
    /// `Fly-Client-IP`), use it for rate limits. Only set this when every
    /// request comes through that proxy.
    #[arg(long, env = "ILLOGICAL_CONTROL_PROXY_HEADER")]
    trust_proxy_header: Option<String>,

    /// Refuse requests signed the way daemons before 0.17 sign them (no
    /// body, query or nonce in the signature); those daemons are told to
    /// update. Off for now, so machines joined with older releases keep
    /// working.
    #[arg(
        long,
        env = "ILLOGICAL_CONTROL_REFUSE_OLD_DAEMON_SIGNATURES",
        value_parser = clap::builder::BoolishValueParser::new(),
        action = clap::ArgAction::SetTrue
    )]
    refuse_old_daemon_signatures: bool,

    /// Serve the web client from this directory instead of the built-in
    /// copy (development).
    #[arg(long)]
    static_dir: Option<PathBuf>,

    /// An ssh jump host for guests of daemons behind NAT (M65: a daemon's
    /// `illogical share --guest` goes through it when the daemon has no
    /// address of its own), listening here. Off unless set.
    #[arg(long, env = "ILLOGICAL_CONTROL_GUEST_SSH")]
    guest_ssh: Option<SocketAddr>,

    /// The host[:port] guests dial for the jump host [default: the public
    /// URL's host, and --guest-ssh's port].
    #[arg(long, env = "ILLOGICAL_CONTROL_GUEST_SSH_HOST")]
    guest_ssh_host: Option<String>,
}

pub struct Github {
    pub client_id: String,
    pub client_secret: String,
    pub url: String,
    pub api: String,
}

pub struct Config {
    pub push_hosts: Vec<String>,
    pub relay_free_bytes: u64,
    pub public_url: String,
    /// `public_url`'s origin, as browsers send it.
    pub origin: String,
    pub github: Option<Github>,
    pub static_dir: Option<PathBuf>,
    /// Take requests signed as daemons before 0.17 sign them.
    pub old_daemon_signatures: bool,
}

pub struct App {
    pub cfg: Config,
    pub db: db::Db,
    pub http: reqwest::Client,
    pub relay: relay::Relay,
    pub passkeys: passkey::Challenges,
    pub limits: limit::Limits,
    pub vapid: push::Vapid,
    pub hosted: Option<sandboxes::Hosted>,
    pub stripe: Option<billing::Stripe>,
    /// The GitHub App (M40), and daemons' subscriptions to its webhooks.
    pub github_app: Option<forge::GithubApp>,
    pub forge: forge::Watches,
    /// The desktop app's sign-ins in progress (M48).
    pub app_logins: app_login::Tickets,
    /// Daemon signatures (and join proofs) already taken.
    pub daemon_sigs: auth::Replays,
    /// TURN credentials for huddles (M63).
    pub turn: Option<turn::Turn>,
    /// The ssh jump host for guests (M65), when it's on, and the routes
    /// daemons registered for it.
    pub jump: Option<guest_jump::Jump>,
    pub guest_routes: guest_jump::Routes,
}

#[cfg(test)]
impl App {
    /// An app for tests: an in-memory database, nothing configured.
    pub fn for_tests(public_url: &str) -> Self {
        let db = db::Db::memory();
        let vapid = push::Vapid::load(&db).unwrap();
        App {
            cfg: Config {
                push_hosts: vec![],
                relay_free_bytes: 0,
                public_url: public_url.into(),
                origin: origin_of(public_url).unwrap(),
                github: None,
                static_dir: None,
                old_daemon_signatures: true,
            },
            db,
            http: reqwest::Client::new(),
            relay: Default::default(),
            passkeys: Default::default(),
            limits: limit::Limits::new(None),
            vapid,
            hosted: None,
            stripe: None,
            github_app: None,
            forge: Default::default(),
            app_logins: Default::default(),
            daemon_sigs: Default::default(),
            turn: None,
            jump: None,
            guest_routes: Default::default(),
        }
    }
}

/// An API error: `{"error": "..."}` with a status, and a `reason` code
/// where a client acts on which refusal it was (#327).
#[derive(Debug)]
pub struct ApiError(StatusCode, String, Option<&'static str>);

pub fn err(status: StatusCode, msg: &str) -> ApiError {
    ApiError(status, msg.to_owned(), None)
}

/// An error with a reason code beside the sentence.
pub fn refusal(status: StatusCode, reason: &'static str, msg: &str) -> ApiError {
    ApiError(status, msg.to_owned(), Some(reason))
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        tracing::warn!(error = %e, "internal error");
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, "something went wrong".into(), None)
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(e: serde_json::Error) -> Self {
        anyhow::Error::from(e).into()
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self.2 {
            Some(reason) => (self.0, Json(json!({ "error": self.1, "reason": reason }))).into_response(),
            None => (self.0, Json(json!({ "error": self.1 }))).into_response(),
        }
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
        .route("/auth/app", post(app_login::ask))
        .route("/auth/app/{id}/redeem", post(app_login::redeem))
        .route("/api/app-login/{id}", get(app_login::show))
        .route("/api/app-login/{id}/allow", post(app_login::allow))
        .route("/auth/passkey/register", post(passkey::register_start))
        .route("/auth/passkey/register/finish", post(passkey::register_finish))
        .route("/auth/passkey/login", post(passkey::login_start))
        .route("/auth/passkey/login/finish", post(passkey::login_finish))
        .route("/api/me", get(api::me))
        .route("/api/me/name", post(api::set_name))
        .route("/api/me/sessions", get(account::sessions))
        .route("/api/me/sessions/end-all", post(account::end_all))
        .route("/api/me/sessions/{id}/end", post(account::end_session))
        .route("/api/me/passkeys", get(account::passkeys))
        .route("/api/me/passkeys/{id}/remove", post(account::remove_passkey))
        .route("/api/me/delete", get(account::preview).post(account::delete))
        .route("/api/me/notices/{id}/seen", post(teams::notice_seen))
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
        .route("/api/joins/{code}/reject", post(api::join_reject))
        .route("/api/daemons/{id}/team", post(api::move_daemon))
        .route("/api/daemon/trust", get(api::daemon_trust))
        .route("/api/daemon/leave", post(api::daemon_leave))
        .route("/api/directory", get(api::directory))
        .route("/api/shares/{daemon}", post(teams::answer_share))
        .route("/api/people", get(teams::person))
        .route("/api/teams", get(teams::list).post(teams::create))
        .route("/api/teams/{id}/roster", post(teams::set_roster))
        .route("/api/teams/{id}/invites", post(teams::invite))
        .route("/api/teams/{id}/presigned", get(teams::list_presigned))
        .route("/api/teams/{id}/presigned/{key}", axum::routing::delete(teams::cancel_presigned))
        .route("/api/teams/{id}/requests/{account}/reject", post(teams::reject))
        .route("/api/teams/{id}/lock", post(teams::lock))
        .route("/api/invites/{team}/{code}", get(teams::show_invite))
        .route("/api/invites/{team}/{code}/accept", post(teams::accept_invite))
        .route("/api/invites/{team}/{code}/preview", get(teams::preview_invite))
        .route("/api/presigned/{team}/{key}", get(teams::show_presigned))
        .route("/api/presigned/{team}/{key}/preview", get(teams::preview_presigned))
        .route("/api/daemon/team", get(teams::daemon_team))
        .route("/api/daemon/peers", get(teams::daemon_peers))
        .route("/api/daemon/turn", get(turn::daemon_turn))
        .route("/api/daemon/teams", get(teams::daemon_teams))
        .route("/api/daemon/access", post(teams::daemon_access))
        .route("/api/relay/link/{id}", get(relay::link))
        .route("/api/push/subscribe", post(push::subscribe))
        .route("/api/push/unsubscribe", post(push::unsubscribe))
        .route("/api/daemon/push-subs", get(push::daemon_subs))
        .route("/api/daemon/push", post(push::daemon_send))
        .route("/api/sandboxes", get(sandboxes::list).post(sandboxes::create))
        .route("/api/sandboxes/{id}", axum::routing::delete(sandboxes::delete))
        .route("/api/daemon/sandbox-done", post(sandboxes::done))
        .route("/api/billing", get(billing::status))
        .route("/api/billing/checkout", post(billing::checkout))
        .route("/api/billing/report", post(billing::report_now))
        .route("/api/stripe/webhook", post(billing::webhook))
        .route("/github/webhook", post(forge::webhook))
        .route("/api/daemon/github/token", post(forge::daemon_token))
        .route("/api/relay/dial", get(relay::dial))
        .route("/api/relay/c/{id}", get(relay::client))
        .route("/api/relay/m", get(relay::many))
        .fallback(asset)
        .layer(axum::middleware::from_fn_with_state(app.clone(), account::note_agent))
        .layer(axum::middleware::from_fn_with_state(app.clone(), auth::verify_daemon))
        .layer(axum::middleware::map_response(headers))
        .with_state(app)
}

async fn control_json(axum::extract::State(app): axum::extract::State<Arc<App>>) -> Json<serde_json::Value> {
    // Passkeys need a domain name: WebAuthn refuses IP addresses.
    let passkeys = url::Url::parse(&app.cfg.public_url).is_ok_and(|u| matches!(u.host(), Some(url::Host::Domain(_))));
    Json(json!({
        "control": true, "url": app.cfg.public_url, "github": app.cfg.github.is_some(), "passkeys": passkeys,
        "vapid": app.vapid.public(),
        "github_app": app.github_app.as_ref().map(|g| g.slug.clone()),
        // How daemons sign their requests here (auth.rs): 2 takes body
        // hashes and nonces.
        "daemon_auth": 2,
        // The CLI joins with a code and signs its requests (M49).
        "cli_join": 1,
        // The ssh jump host for guests of daemons behind NAT (M65).
        "guest_ssh": app.jump.as_ref().map(guest_jump::Jump::describe),
    }))
}

/// Nothing frames control's pages, and nothing on them comes from elsewhere
/// except the daemons the page connects to (any WebSocket) and the blocks it
/// shows: a block is a frame on its own site (a daemon's block domain, or a
/// web page a browser block opened), so frames may come from any https site.
async fn headers(mut res: Response) -> Response {
    let h = res.headers_mut();
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    // Browsers ignore it over plain HTTP (a local control), so always.
    h.insert(header::STRICT_TRANSPORT_SECURITY, HeaderValue::from_static("max-age=31536000"));
    h.entry(header::CONTENT_SECURITY_POLICY).or_insert(HeaderValue::from_static(
        "default-src 'self'; connect-src 'self' wss: ws: https:; img-src 'self' data: https:; style-src 'self' 'unsafe-inline'; frame-src 'self' https:; frame-ancestors 'none'",
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

/// Billing, if there's a Stripe key: never without the webhook secret,
/// which is all that tells Stripe's events from anyone's.
fn billing_from(
    secret: Option<String>,
    webhook_secret: Option<String>,
    api: String,
    seat_price: String,
    minutes_price: String,
    minutes_event: String,
) -> anyhow::Result<Option<billing::Stripe>> {
    let Some(secret) = secret else { return Ok(None) };
    let Some(webhook_secret) = webhook_secret else {
        anyhow::bail!("STRIPE_SECRET_KEY is set but STRIPE_WEBHOOK_SECRET isn't: billing needs both");
    };
    Ok(Some(billing::Stripe {
        api: api.trim_end_matches('/').to_owned(),
        secret,
        webhook_secret,
        seat_price,
        minutes_price,
        minutes_event,
    }))
}

/// The GitHub App from its id and key, if both are given.
fn github_app(a: &Args) -> anyhow::Result<Option<forge::GithubApp>> {
    let Some(id) = a.github_app_id.clone().filter(|s| !s.is_empty()) else {
        info!("no GITHUB_APP_ID: no GitHub webhooks or App tokens (forge blocks poll)");
        return Ok(None);
    };
    let pem = match (&a.github_app_private_key, &a.github_app_private_key_file) {
        (Some(k), _) if !k.is_empty() => k.clone(),
        (_, Some(f)) => std::fs::read_to_string(f)
            .with_context(|| format!("reading GITHUB_APP_PRIVATE_KEY_FILE {}", f.display()))?,
        _ => anyhow::bail!("GITHUB_APP_ID is set but there's no GITHUB_APP_PRIVATE_KEY or GITHUB_APP_PRIVATE_KEY_FILE"),
    };
    let key = forge::parse_pem(&pem)?;
    let secret = a.github_app_webhook_secret.clone().unwrap_or_default();
    if secret.is_empty() {
        tracing::warn!("no GITHUB_APP_WEBHOOK_SECRET: every GitHub webhook will be refused");
    }
    info!(app = id, slug = a.github_app_slug, "GitHub App");
    Ok(Some(forge::GithubApp::new(id, a.github_app_slug.clone(), secret, &a.github_api, key)))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "illogical_control=info".into()),
        )
        .init();
    let a = Args::parse();
    raise_open_files();
    // Bound first, so port 0 is known before the URL is (#67).
    let l = tokio::net::TcpListener::bind(a.listen).await?;
    let listen = l.local_addr()?;
    let mut public_url = a.public_url.trim_end_matches('/').to_owned();
    if a.listen.port() == 0 {
        let at =
            a.db.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(std::path::Path::new(".")).join("listen");
        // Whole or not at all: a reader never sees half a port.
        let tmp = at.with_extension("tmp");
        std::fs::write(&tmp, listen.to_string())
            .and_then(|_| std::fs::rename(&tmp, &at))
            .with_context(|| format!("recording the listen address in {}", at.display()))?;
    }
    if let Ok(mut u) = url::Url::parse(&public_url)
        && u.port() == Some(0)
    {
        let _ = u.set_port(Some(listen.port()));
        public_url = u.as_str().trim_end_matches('/').to_owned();
    }
    let set = |v: Option<String>| v.filter(|s| !s.is_empty());
    let github_app = github_app(&a)?;
    // Sign-in: an OAuth app's (or another App's) credentials, else the
    // GitHub App's own.
    let (client_id, client_secret) = match (set(a.github_client_id.clone()), set(a.github_client_secret.clone())) {
        (Some(i), Some(s)) => (Some(i), Some(s)),
        _ => (set(a.github_app_client_id.clone()), set(a.github_app_client_secret.clone())),
    };
    let github = match (client_id, client_secret) {
        (Some(client_id), Some(client_secret)) => {
            Some(Github { client_id, client_secret, url: a.github_url.clone(), api: a.github_api.clone() })
        }
        _ => None,
    };
    if github.is_none() {
        tracing::warn!("no GITHUB_CLIENT_ID/GITHUB_CLIENT_SECRET: GitHub sign-in is off");
    }
    let backup = backup::Backup::new(&a.backup, &a.db)?;
    if let Some(b) = &backup {
        b.restore_if_missing().await?;
    }
    let db = db::Db::open(&a.db)?;
    if let Some(b) = &backup {
        b.replicate();
    }
    let vapid = push::Vapid::load(&db)?;
    let hosted = match a.sprites_token.filter(|t| !t.is_empty()) {
        Some(t) => Some(sandboxes::Hosted {
            sprites: sprites::Sprites::new(&a.sprites_url, t)?,
            binary: a.sandbox_binary,
            allow: a.sandbox_accounts,
            quota: a.sandbox_quota,
        }),
        None => None,
    };
    let stripe = billing_from(
        set(a.stripe_secret),
        set(a.stripe_webhook_secret),
        a.stripe_api,
        a.stripe_seat_price,
        a.stripe_minutes_price,
        a.stripe_minutes_event,
    )?;
    let turn = match (set(a.turn_key_id), set(a.turn_api_token)) {
        (Some(key_id), Some(token)) => Some(turn::Turn { key_id, token, api: a.turn_api }),
        _ => {
            tracing::warn!("no CLOUDFLARE_TURN_KEY_ID/CLOUDFLARE_TURN_API_TOKEN: huddles get STUN only");
            None
        }
    };
    // The guests' jump host: bound now, so its port is known.
    let guest_ssh = match a.guest_ssh {
        Some(addr) => {
            let l = tokio::net::TcpListener::bind(addr)
                .await
                .with_context(|| format!("listening for guest ssh on {addr}"))?;
            let port = l.local_addr()?.port();
            // `host:port`, or a host (an IPv6 address has more than one colon).
            let (host, port) = match set(a.guest_ssh_host.clone()) {
                Some(h) => match h.split_once(':') {
                    Some((name, p)) if !p.contains(':') => {
                        (name.to_owned(), p.parse().context("--guest-ssh-host's port")?)
                    }
                    _ => (h, port),
                },
                None => (url::Url::parse(&public_url)?.host_str().unwrap_or("localhost").to_owned(), port),
            };
            Some((l, guest_jump::Jump::new(&guest_jump::key_path(&a.db), host, port)?))
        }
        None => None,
    };
    let (guest_listener, jump) = match guest_ssh {
        Some((l, j)) => (Some(l), Some(j)),
        None => (None, None),
    };
    let app = Arc::new(App {
        cfg: Config {
            push_hosts: a.push_hosts,
            relay_free_bytes: a.relay_free_mb * 1_000_000,
            origin: origin_of(&public_url)?,
            public_url,
            github,
            static_dir: a.static_dir,
            old_daemon_signatures: !a.refuse_old_daemon_signatures,
        },
        db,
        http: roots::client(std::time::Duration::from_secs(15))?,
        relay: relay::Relay::new(relay::Caps {
            sockets: a.relay_max_sockets,
            daemons: a.relay_max_machines,
            daily_bytes: a.relay_daily_mb * 1_000_000,
            total: a.relay_max_total,
        }),
        passkeys: Default::default(),
        limits: limit::Limits::new(a.trust_proxy_header),
        vapid,
        hosted,
        stripe,
        github_app,
        forge: Default::default(),
        app_logins: Default::default(),
        daemon_sigs: Default::default(),
        turn,
        jump,
        guest_routes: Default::default(),
    });
    if let (Some(l), Some(j)) = (guest_listener, &app.jump) {
        info!(addr = %l.local_addr()?, host = %j.host, port = j.port, "guest ssh jump host");
        tokio::spawn(guest_jump::serve(app.clone(), l));
    }
    tokio::spawn(relay::Relay::log_counts(app.clone()));
    if !app.cfg.old_daemon_signatures {
        info!("refusing daemons' pre-0.17 request signatures");
    }
    if app.github_app.is_some() {
        tokio::spawn(forge::heartbeat(app.clone(), std::time::Duration::from_secs(a.forge_heartbeat_secs.max(1))));
    }
    if app.stripe.is_some() {
        let a2 = app.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
                billing::report_usage(&a2).await;
            }
        });
    }
    // Expired sessions, join codes and invites go (#173).
    {
        let a2 = app.clone();
        tokio::spawn(async move {
            loop {
                match a2.db.prune(illogical_e2e::now_ms()) {
                    Ok(n) if n > 0 => info!(sessions = n, "pruned expired sessions"),
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "pruning"),
                }
                tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
            }
        });
    }
    // Nagle off: the relay's mux writes frames back to back (S15).
    let l = l.tap_io(|t| {
        let _ = t.set_nodelay(true);
    });
    info!(%listen, url = %app.cfg.public_url, "illogical control");
    let serve = axum::serve(l, router(app).into_make_service_with_connect_info::<SocketAddr>());
    tokio::select! {
        r = serve => r?,
        _ = stopping() => info!("stopping"),
    }
    // Litestream syncs what's left before control goes.
    if let Some(b) = &backup {
        b.stop().await;
    }
    Ok(())
}

/// Every relay connection is a socket: take all the open files the system
/// allows (#174), not the usual soft limit of 1024.
fn raise_open_files() {
    let mut r = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    // SAFETY: plain calls with a valid pointer to a local.
    unsafe {
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut r) != 0 {
            return;
        }
        if r.rlim_cur < r.rlim_max {
            let want = libc::rlimit { rlim_cur: r.rlim_max, rlim_max: r.rlim_max };
            if libc::setrlimit(libc::RLIMIT_NOFILE, &want) == 0 {
                r = want;
            }
        }
    }
    info!(open_files = r.rlim_cur, "open file limit");
}

/// SIGTERM (Fly stopping the machine) or Ctrl-C.
async fn stopping() {
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending().await,
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {},
        _ = term => {},
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn args() {
        use clap::CommandFactory;
        super::Args::command().debug_assert();
    }

    #[test]
    fn billing_needs_its_webhook_secret() {
        let b = |s: Option<&str>, w: Option<&str>| {
            super::billing_from(
                s.map(Into::into),
                w.map(Into::into),
                "https://api.stripe.com/".into(),
                String::new(),
                String::new(),
                String::new(),
            )
        };
        assert!(b(None, None).unwrap().is_none(), "off without a key");
        assert!(b(Some("sk_test"), None).is_err(), "a key alone doesn't start");
        let on = b(Some("sk_test"), Some("whsec")).unwrap().unwrap();
        assert_eq!((on.webhook_secret.as_str(), on.api.as_str()), ("whsec", "https://api.stripe.com"));
    }

    #[test]
    fn days() {
        assert_eq!(super::day(0), "1970-01-01");
        assert_eq!(super::day(1_790_000_000_000), "2026-09-21");
        assert_eq!(super::day(951_782_400_000), "2000-02-29");
    }
}
