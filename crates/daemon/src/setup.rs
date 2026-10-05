//! Getting started's buttons: what the welcome screens used to tell you to
//! type, run by the daemon for its owner.
//!
//! - `POST /api/setup/tailscale`: `tailscale serve --bg --https=443` in
//!   front of this daemon, so the phone reaches it on the tailnet.
//! - `POST /api/setup/control`: ask illogical control to add this machine
//!   (`illogicald join`); the answer is the code and where to approve it,
//!   and the daemon waits for the approval in the background. Once
//!   approved, the page shows the account's fingerprint to check against
//!   the approving device, and
//!   `POST /api/setup/control/confirm` (`{"same": true}`) saves the join;
//!   `false` drops it.
//! - `POST /api/setup/claude`: `claude mcp add illogical -- illogical mcp`.
//!
//! `GET /api/setup` says how far along each one is. When a step needs
//! something only a person can do (a one-time sudo, a switch in
//! Tailscale's admin console, a sign-in), the error says so, with the
//! command or the link.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Json, Router,
    extract::{Query, State},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::server::App;

type AppState = State<Arc<App>>;

pub const CONTROL: &str = "https://control.illogical.widgets.wtf";
const TAILSCALE_ADMIN_DNS: &str = "https://login.tailscale.com/admin/dns";
const TAILSCALE_DOWNLOAD: &str = "https://tailscale.com/download";

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/setup", get(status))
        .route("/api/setup/tailscale", post(tailscale_serve))
        .route("/api/setup/control", post(control_join))
        .route("/api/setup/control/confirm", post(control_confirm))
        .route("/api/setup/claude", post(claude_mcp))
}

/// What a button did: done, or why not and what fixes it.
#[derive(Serialize, Default)]
struct Outcome {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    /// A command for the person to run once (it needs sudo, say).
    #[serde(skip_serializing_if = "Option::is_none")]
    fix: Option<String>,
    /// A page for the person to open (Tailscale's admin console, say).
    #[serde(skip_serializing_if = "Option::is_none")]
    link: Option<Link>,
}

#[derive(Serialize, Clone)]
struct Link {
    label: String,
    url: String,
}

impl Outcome {
    fn ok() -> Self {
        Self { ok: true, ..Default::default() }
    }
    fn err(e: impl Into<String>) -> Self {
        Self { error: Some(e.into()), ..Default::default() }
    }
    fn fix(mut self, cmd: impl Into<String>) -> Self {
        self.fix = Some(cmd.into());
        self
    }
    fn link(mut self, label: &str, url: impl Into<String>) -> Self {
        self.link = Some(Link { label: label.into(), url: url.into() });
        self
    }
}

#[derive(Serialize)]
struct Status {
    tailscale: TailscaleStatus,
    control: ControlStatus,
    claude: ClaudeStatus,
}

#[derive(Serialize, Default)]
struct TailscaleStatus {
    /// `missing` (no Tailscale here), `stopped`, `needs-login`, `running`.
    state: &'static str,
    /// This machine's MagicDNS name.
    #[serde(skip_serializing_if = "Option::is_none")]
    host: Option<String>,
    /// HTTPS certificates are on for the tailnet (serve needs them).
    https: bool,
    /// `tailscale serve` sends the tailnet's HTTPS to this daemon.
    serving: bool,
    /// Where the phone opens it.
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    /// A request has come in over the tailnet.
    seen: bool,
}

#[derive(Serialize, Default)]
struct ControlStatus {
    #[serde(skip_serializing_if = "Option::is_none")]
    joined: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    team: Option<String>,
    /// A join waiting for someone to approve it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pending: Option<Pending>,
    /// An approved join waiting for the person to check the account.
    #[serde(skip_serializing_if = "Option::is_none")]
    confirm: Option<Confirm>,
    /// The last join that didn't work, and why.
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    /// Control's address, for the button.
    url: &'static str,
}

#[derive(Serialize, Clone)]
struct Pending {
    code: String,
    approve: String,
    /// When the code stops working (ms since the epoch).
    expires_ms: u64,
}

/// What the page shows to check before the join is saved.
#[derive(Serialize, Clone)]
struct Confirm {
    /// The account's fingerprint.
    account: String,
    /// The device that approved it, by name.
    approver: String,
    /// "your account" or "the team X".
    place: String,
}

#[derive(Serialize, Default)]
struct ClaudeStatus {
    installed: bool,
    /// `claude mcp get illogical` finds it.
    tools: bool,
}

/// The join in flight, if any, and the last one's failure.
static JOIN: Mutex<(Option<Pending>, Option<String>)> = Mutex::new((None, None));
/// An approved join, until the person says the account is theirs.
static APPROVED: Mutex<Option<crate::control::Approved>> = Mutex::new(None);

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// The daemon's port, for what serve points at.
fn port(app: &App) -> u16 {
    app.access.port()
}

/// `name` on the owner's shell's PATH (which a service's own PATH may lack),
/// then in the usual places.
async fn find(app: &App, name: &str, also: &[&str]) -> Option<PathBuf> {
    let shell = app.mux.shell_env.local().await;
    let path = shell.get("PATH").map(str::to_owned).or_else(|| std::env::var("PATH").ok()).unwrap_or_default();
    std::env::split_paths(&path).map(|d| d.join(name)).chain(also.iter().map(PathBuf::from)).find(|p| p.is_file())
}

/// Runs a command with the shell's PATH: its exit, stdout and stderr.
async fn run(app: &App, bin: &Path, args: &[&str], timeout: Duration) -> Result<(bool, String, String), String> {
    let shell = app.mux.shell_env.local().await;
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(args).kill_on_drop(true).stdin(std::process::Stdio::null());
    if let Some(p) = shell.get("PATH") {
        cmd.env("PATH", p);
    }
    if bin.to_string_lossy().contains("Tailscale.app") {
        // The macOS app's binary is the CLI only when told so: started
        // from a service, it tries to open the GUI and fails.
        cmd.env("TAILSCALE_BE_CLI", "1");
    }
    match tokio::time::timeout(timeout, cmd.output()).await {
        Err(_) => Err(format!("{} didn't finish in {} s", bin.display(), timeout.as_secs())),
        Ok(Err(e)) => Err(format!("{}: {e}", bin.display())),
        Ok(Ok(o)) => Ok((
            o.status.success(),
            String::from_utf8_lossy(&o.stdout).into_owned(),
            String::from_utf8_lossy(&o.stderr).trim().to_owned(),
        )),
    }
}

// ---- tailscale

const TAILSCALE_ELSEWHERE: [&str; 4] = [
    "/usr/bin/tailscale",
    "/usr/local/bin/tailscale",
    "/opt/homebrew/bin/tailscale",
    "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
];

async fn tailscale(app: &App) -> Option<PathBuf> {
    find(app, "tailscale", &TAILSCALE_ELSEWHERE).await
}

async fn tailscale_status(app: &App) -> TailscaleStatus {
    let seen = app.tailnet_seen.load(std::sync::atomic::Ordering::Relaxed);
    let Some(ts) = tailscale(app).await else {
        return TailscaleStatus { state: "missing", seen, ..Default::default() };
    };
    let Ok((true, out, _)) = run(app, &ts, &["status", "--json"], Duration::from_secs(5)).await else {
        return TailscaleStatus { state: "stopped", seen, ..Default::default() };
    };
    let v: Value = serde_json::from_str(&out).unwrap_or_default();
    let state = match v["BackendState"].as_str() {
        Some("Running") => "running",
        Some("NeedsLogin" | "NeedsMachineAuth") => "needs-login",
        _ => "stopped",
    };
    let host = v["Self"]["DNSName"].as_str().map(|h| h.trim_end_matches('.').to_owned()).filter(|h| !h.is_empty());
    let https = v["CertDomains"].as_array().is_some_and(|d| !d.is_empty());
    let serving = match run(app, &ts, &["serve", "status", "--json"], Duration::from_secs(5)).await {
        Ok((true, out, _)) => serves(&out, port(app)),
        _ => false,
    };
    TailscaleStatus {
        state,
        url: host.as_ref().filter(|_| serving).map(|h| format!("https://{h}")),
        host,
        https,
        serving,
        seen,
    }
}

/// Whether `tailscale serve status --json` proxies something to this port.
fn serves(json: &str, port: u16) -> bool {
    let Ok(v) = serde_json::from_str::<Value>(json) else { return false };
    let targets = [format!("127.0.0.1:{port}"), format!("localhost:{port}")];
    v["Web"].as_object().is_some_and(|web| {
        web.values().any(|site| {
            site["Handlers"].as_object().is_some_and(|h| {
                h.values()
                    .any(|handler| handler["Proxy"].as_str().is_some_and(|p| targets.iter().any(|t| p.contains(t))))
            })
        })
    })
}

async fn tailscale_serve(State(app): AppState) -> Json<Outcome> {
    let Some(ts) = tailscale(&app).await else {
        return Json(
            Outcome::err("Tailscale isn't installed on this machine.").link("Get Tailscale", TAILSCALE_DOWNLOAD),
        );
    };
    let st = tailscale_status(&app).await;
    match st.state {
        "running" => {}
        "needs-login" => {
            return Json(
                Outcome::err("Tailscale is installed but not signed in.").fix(format!("{} up", tailscale_cmd(&ts))),
            );
        }
        _ => return Json(Outcome::err("Tailscale isn't running.").fix(format!("{} up", tailscale_cmd(&ts)))),
    }
    if st.serving {
        return Json(Outcome::ok());
    }
    if !st.https {
        return Json(
            Outcome::err("Turn on MagicDNS and HTTPS certificates for your tailnet first, then try again.")
                .link("Tailscale's DNS settings", TAILSCALE_ADMIN_DNS),
        );
    }
    let target = format!("http://127.0.0.1:{}", port(&app));
    // Serve may stop to ask for Serve to be turned on for the tailnet: it
    // prints the link and waits, so this gives up after a while and passes
    // the link on.
    match run(&app, &ts, &["serve", "--bg", "--yes", "--https=443", &target], Duration::from_secs(20)).await {
        Ok((true, _, _)) => Json(Outcome::ok()),
        Ok((false, out, err)) => Json(serve_failed(&ts, &format!("{out}\n{err}"))),
        Err(e) => Json(Outcome::err(e)),
    }
}

fn tailscale_cmd(ts: &Path) -> String {
    if ts.starts_with("/Applications") { ts.display().to_string() } else { "tailscale".into() }
}

fn serve_failed(ts: &Path, said: &str) -> Outcome {
    let said = said.trim();
    if let Some(url) = said.split_whitespace().find(|w| w.starts_with("https://login.tailscale.com/")) {
        return Outcome::err("Serve isn't turned on for your tailnet yet.").link("Turn on Serve", url);
    }
    if said.contains("Access denied") || said.contains("operator") || said.contains("permission") {
        // Linux: serve's config is tailscaled's; letting this user change
        // it takes root once.
        return Outcome::err("Tailscale needs to let you change serve without sudo, once.")
            .fix(format!("sudo {} set --operator=$USER", tailscale_cmd(ts)));
    }
    Outcome::err(if said.is_empty() { "tailscale serve failed".to_owned() } else { said.to_owned() })
}

// ---- control

#[derive(Deserialize, Default)]
#[serde(default)]
struct JoinReq {
    /// Control's address (illogical's cloud by default).
    url: Option<String>,
    /// A team to put it in ahead of time (its id).
    team: Option<String>,
}

fn control_status(app: &App) -> ControlStatus {
    let joined = app.control.enrolled();
    let saved = joined.as_ref().map(|e| &e.saved);
    let (pending, error) = JOIN.lock().unwrap().clone();
    ControlStatus {
        joined: saved.map(|s| s.url.clone()),
        team: saved
            .and_then(|s| s.roster.as_ref().map(|r| r.name.clone()).or_else(|| Some(s.team.as_ref()?.team.clone()))),
        pending: pending.filter(|p| p.expires_ms > now_ms() && saved.is_none()),
        confirm: APPROVED.lock().unwrap().as_ref().filter(|_| saved.is_none()).map(|a| Confirm {
            account: a.joined.account.clone(),
            approver: a.joined.approver.clone(),
            place: a.joined.place.clone(),
        }),
        error: error.filter(|_| saved.is_none()),
        url: CONTROL,
    }
}

async fn control_join(State(app): AppState, body: Option<Json<JoinReq>>) -> Json<Value> {
    let req = body.map(|Json(b)| b).unwrap_or_default();
    if app.control.enrolled().is_some() || APPROVED.lock().unwrap().is_some() {
        return Json(serde_json::to_value(control_status(&app)).unwrap());
    }
    // One at a time: asking again while a code is open returns that code.
    if let (Some(p), _) = &*JOIN.lock().unwrap()
        && p.expires_ms > now_ms()
    {
        return Json(serde_json::json!({ "pending": p }));
    }
    let url = req.url.unwrap_or_else(|| CONTROL.into());
    let name = app.hosts.name().to_owned();
    let dir = app.control.state_dir().to_owned();
    match crate::control::join_start(&url, &name, req.team.as_deref(), None, &dir).await {
        Ok(p) => {
            let pending = Pending {
                code: p.code.clone(),
                approve: p.approve_url(),
                expires_ms: now_ms() + p.expires_in_secs * 1000,
            };
            *JOIN.lock().unwrap() = (Some(pending.clone()), None);
            tokio::spawn(async move {
                let r = crate::control::join_finish(p).await;
                let mut j = JOIN.lock().unwrap();
                match r {
                    Ok(a) => {
                        *APPROVED.lock().unwrap() = Some(a);
                        *j = (None, None);
                    }
                    Err(e) => *j = (None, Some(e.to_string())),
                }
            });
            Json(serde_json::json!({ "pending": pending }))
        }
        Err(e) => {
            JOIN.lock().unwrap().1 = Some(e.to_string());
            Json(serde_json::json!({ "error": e.to_string() }))
        }
    }
}

#[derive(Deserialize)]
struct ConfirmReq {
    /// The fingerprint here is the one the approving device shows.
    same: bool,
}

/// The person compared the account's fingerprints: save the join (where the
/// running daemon picks it up), or drop it.
async fn control_confirm(State(app): AppState, Json(req): Json<ConfirmReq>) -> Json<Value> {
    let Some(a) = APPROVED.lock().unwrap().take() else {
        return Json(serde_json::json!({ "control": control_status(&app) }));
    };
    let error = if !req.same {
        let e = format!(
            "Not joined: control approved this machine into the account {}, which isn't the one your device shows. Don't add machines through this control.",
            a.joined.account
        );
        a.refuse(app.control.state_dir()).await;
        Some(e)
    } else {
        let r = a.save(app.control.state_dir());
        app.control.poke();
        r.err().map(|e| e.to_string())
    };
    JOIN.lock().unwrap().1 = error;
    Json(serde_json::json!({ "control": control_status(&app) }))
}

// ---- claude code

async fn claude(app: &App) -> Option<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_default();
    let local = [format!("{home}/.local/bin/claude"), format!("{home}/.claude/local/claude")];
    let also: Vec<&str> =
        local.iter().map(String::as_str).chain(["/opt/homebrew/bin/claude", "/usr/local/bin/claude"]).collect();
    find(app, "claude", &also).await
}

/// The `illogical` CLI that Claude Code should start: beside this daemon,
/// else on the PATH.
async fn cli(app: &App) -> Option<PathBuf> {
    let beside =
        std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join("illogical"))).filter(|p| p.is_file());
    match beside {
        Some(p) => Some(p),
        None => find(app, "illogical", &[]).await,
    }
}

async fn claude_status(app: &App) -> ClaudeStatus {
    let Some(c) = claude(app).await else { return ClaudeStatus::default() };
    let tools = matches!(run(app, &c, &["mcp", "get", "illogical"], Duration::from_secs(15)).await, Ok((true, _, _)));
    ClaudeStatus { installed: true, tools }
}

async fn claude_mcp(State(app): AppState) -> Json<Outcome> {
    let Some(c) = claude(&app).await else {
        return Json(
            Outcome::err("Claude Code isn't installed on this machine.")
                .link("Install Claude Code", "https://docs.anthropic.com/en/docs/claude-code"),
        );
    };
    if claude_status(&app).await.tools {
        return Json(Outcome::ok());
    }
    let Some(cli) = cli(&app).await else {
        return Json(Outcome::err("Can't find the illogical CLI next to the daemon."));
    };
    let cli = cli.display().to_string();
    match run(&app, &c, &["mcp", "add", "--scope", "user", "illogical", "--", &cli, "mcp"], Duration::from_secs(20))
        .await
    {
        Ok((true, _, _)) => Json(Outcome::ok()),
        Ok((false, out, err)) => {
            Json(Outcome::err(format!("claude mcp add: {}", if err.is_empty() { out.trim().to_owned() } else { err })))
        }
        Err(e) => Json(Outcome::err(e)),
    }
}

// ---- status

#[derive(Deserialize, Default)]
#[serde(default)]
struct StatusQuery {
    /// `control`: only that (cheap, for polling while a join waits); the
    /// others run `tailscale` and `claude`.
    part: Option<String>,
}

async fn status(State(app): AppState, Query(q): Query<StatusQuery>) -> Json<Value> {
    if q.part.as_deref() == Some("control") {
        return Json(serde_json::json!({ "control": control_status(&app) }));
    }
    let (tailscale, claude) = tokio::join!(tailscale_status(&app), claude_status(&app));
    Json(serde_json::to_value(Status { tailscale, control: control_status(&app), claude }).unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serve_status_finds_this_port() {
        let json = r#"{"TCP":{"443":{"HTTPS":true}},"Web":{"geek.tail.ts.net:443":{"Handlers":{"/":{"Proxy":"http://127.0.0.1:7681"}}}}}"#;
        assert!(serves(json, 7681));
        assert!(!serves(json, 7682));
        assert!(!serves("{}", 7681));
    }

    #[test]
    fn serve_failures_say_what_to_do() {
        let ts = Path::new("/usr/bin/tailscale");
        let o = serve_failed(
            ts,
            "Serve is not enabled on your tailnet.\nTo enable, visit:\n\n https://login.tailscale.com/f/serve?node=abc",
        );
        assert_eq!(o.link.unwrap().url, "https://login.tailscale.com/f/serve?node=abc");
        let o = serve_failed(ts, "Access denied: serve config denied");
        assert_eq!(o.fix.as_deref(), Some("sudo tailscale set --operator=$USER"));
        let mac = Path::new("/Applications/Tailscale.app/Contents/MacOS/Tailscale");
        assert!(serve_failed(mac, "permission denied").fix.unwrap().starts_with("sudo /Applications/"));
    }
}
