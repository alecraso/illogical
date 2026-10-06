//! M4a end to end: a home daemon's host list, the CLI's `--host`, and a
//! second daemon accepting the home daemon's page (its exact origin) and
//! nobody else's. Both daemons are real binaries on loopback; tailscaled is
//! kept out of it (`--tailscale-socket` points nowhere), and a fake tailnet
//! name stands in for serve.

// Over the daemon's Unix socket; Windows gets its named pipe in M56 (#219).
#![cfg(unix)]

use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::Duration,
};

use illogical_testkit::{Daemon, illogicald};
use serde_json::{Value, json};
use tokio_tungstenite::{connect_async, tungstenite::client::IntoClientRequest};

const PUBLIC: &str = "box.example.ts.net";
const OWNER: &str = "me@example.com";

/// One local token for every daemon here, and the CLI: two daemons on one
/// machine, as one person's (the CLI's `--host URL` shows it to both).
fn token_file() -> PathBuf {
    use std::os::unix::fs::OpenOptionsExt;
    let f = std::env::temp_dir().join(format!("ilg-hosts-token-{}", std::process::id()));
    // Made once, before any daemon starts (they'd race to make it).
    static MADE: std::sync::Once = std::sync::Once::new();
    MADE.call_once(|| {
        let mut w = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&f).unwrap();
        w.write_all(format!("ilt_hosts{:032x}", std::process::id() as u128 * 7919).as_bytes()).unwrap();
    });
    f
}

fn start(name: &str, extra: &[&str]) -> Daemon {
    illogicald!("hosts")
        .args(["--name", name])
        .no_tailscale()
        .args(["--public-host", PUBLIC, "--owner", OWNER])
        .args(extra)
        .env("PS1", "$ ")
        .env("ILLOGICAL_LOCAL_TOKEN_FILE", token_file())
        .start()
}

trait Http {
    fn http(&self, method: &str, path: &str, headers: &[(&str, &str)], body: Option<Value>) -> (u16, String, String);
}

impl Http for Daemon {
    /// One HTTP request over TCP with these headers; status, headers, body.
    fn http(&self, method: &str, path: &str, headers: &[(&str, &str)], body: Option<Value>) -> (u16, String, String) {
        let body = body.map(|b| b.to_string()).unwrap_or_default();
        let mut headers = headers.to_vec();
        // A program on this machine shows the local token (serve's
        // requests carry an identity instead, and hosts' paths a token of
        // their own).
        let own_credential = ["/api/sync/", "/api/hosts/join", "/api/dial"].iter().any(|p| path.starts_with(p));
        let bearer = self.bearer();
        if !own_credential
            && !self.token().is_empty()
            && !headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("authorization") || k.eq_ignore_ascii_case("tailscale-user-login"))
        {
            headers.push(("Authorization", &bearer));
        }
        if !body.is_empty() {
            headers.push(("Content-Type", "application/json"));
        }
        self.tcp(method, path, &headers, Some(&body))
    }
}

/// The CLI, built next to the daemon (cargo builds only this package's
/// binaries for its tests).
fn cli_bin() -> PathBuf {
    let bin = Path::new(env!("CARGO_BIN_EXE_illogicald")).with_file_name("illogical");
    let status = Command::new(env!("CARGO")).args(["build", "-q", "-p", "illogical"]).status().unwrap();
    assert!(status.success(), "building the CLI");
    bin
}

fn cli(home: &Daemon, args: &[&str]) -> Output {
    Command::new(cli_bin())
        .arg("--socket")
        .arg(home.sock())
        .args(args)
        .env_remove("ILLOGICAL_PANE")
        .env("ILLOGICAL_LOCAL_TOKEN_FILE", token_file())
        .output()
        .unwrap()
}

fn stdout(o: &Output) -> String {
    assert!(o.status.success(), "{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn wait_for(what: &str, f: impl FnMut() -> bool) {
    illogical_testkit::wait_for(what, Duration::from_secs(15), f);
}

/// The desktop app checks this number against the range it supports
/// (#390), next to the version it already read.
#[test]
fn the_host_says_its_protocol_for_the_app() {
    let d = start("proto", &[]);
    let host = d.get("/api/host");
    assert_eq!(host["protocol"], illogical_proto::PROTOCOL);
    assert_eq!(host["version"], env!("CARGO_PKG_VERSION"));
}

#[test]
fn the_home_list_and_host_flag_reach_another_daemon() {
    let home = start("home", &[]);
    let other = start("other", &["--allow-origin", &home.url()]);

    // Add it; the home daemon checks on it straight away.
    stdout(&cli(&home, &["hosts", "add", "other", &other.url()]));
    let list: Value = serde_json::from_str(&stdout(&cli(&home, &["--json", "hosts"]))).unwrap();
    assert_eq!(list["this"], "home");
    assert_eq!(list["hosts"][0]["name"], "other");
    assert_eq!(list["hosts"][0]["urls"][0], other.url());
    wait_for("the home daemon to see it", || {
        let v: Value = serde_json::from_str(&stdout(&cli(&home, &["--json", "hosts"]))).unwrap();
        v["hosts"][0]["last_seen_ms"].is_u64()
    });

    // `--host NAME` runs there, not here.
    let run = cli(&home, &["--host", "other", "run", "--wait", "--", "echo on-the-other; exit 4"]);
    assert_eq!(run.status.code(), Some(4), "{}", String::from_utf8_lossy(&run.stderr));
    let pane = String::from_utf8_lossy(&run.stdout).trim().trim_start_matches('%').to_owned();
    let tail = stdout(&cli(&home, &["--host", "other", "tail", &format!("%{pane}"), "--text"]));
    assert!(tail.contains("on-the-other"), "{tail}");
    let here: Value = serde_json::from_str(&stdout(&cli(&home, &["--json", "ls"]))).unwrap();
    assert_eq!(here.as_array().unwrap().len(), 1, "nothing new on the home daemon");
    // A URL works without the list; the home daemon's own name is the socket.
    let there: Value = serde_json::from_str(&stdout(&cli(&home, &["--host", &other.url(), "--json", "ls"]))).unwrap();
    assert_eq!(there.as_array().unwrap().len(), 2);
    let me: Value = serde_json::from_str(&stdout(&cli(&home, &["--host", "home", "--json", "ls"]))).unwrap();
    assert_eq!(me.as_array().unwrap().len(), 1);
    // Unknown names fail clearly.
    let o = cli(&home, &["--host", "nope", "ls"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("no host nope"));

    stdout(&cli(&home, &["hosts", "rm", "other"]));
    let list: Value = serde_json::from_str(&stdout(&cli(&home, &["--json", "hosts"]))).unwrap();
    assert_eq!(list["hosts"], json!([]));
}

#[tokio::test]
async fn another_daemon_accepts_the_home_page_and_nobody_else() {
    let home = start("home", &[]);
    let other = start("other", &["--allow-origin", &home.url()]);
    let ws = |origin: &str| {
        let mut req = format!("ws://127.0.0.1:{}/ws", other.port).into_client_request().unwrap();
        req.headers_mut().insert("origin", origin.parse().unwrap());
        req.headers_mut().insert("authorization", format!("Bearer {}", other.token()).parse().unwrap());
        connect_async(req)
    };
    assert!(ws(&home.url()).await.is_ok(), "the home daemon's page may connect");
    for bad in ["http://127.0.0.1:1", "https://evil.example", &home.url().replace("http:", "https:"), "null"] {
        assert!(ws(bad).await.is_err(), "{bad} must be refused");
    }
    // The home daemon itself doesn't accept the other's page.
    let mut req = format!("ws://127.0.0.1:{}/ws", home.port).into_client_request().unwrap();
    req.headers_mut().insert("origin", other.url().parse().unwrap());
    req.headers_mut().insert("authorization", format!("Bearer {}", home.token()).parse().unwrap());
    assert!(connect_async(req).await.is_err());

    let (other, home_url) = (std::sync::Arc::new(other), home.url());
    let o = other.clone();
    tokio::task::spawn_blocking(move || {
        // CORS: the API answers the home page, exactly, and no one else.
        let pre = [("origin", home_url.as_str()), ("access-control-request-method", "POST")];
        let (status, head, _) = o.http("OPTIONS", "/api/run", &pre, None);
        assert_eq!(status, 204, "{head}");
        assert!(head.contains(&format!("access-control-allow-origin: {home_url}")), "{head}");
        let (status, head, _) = o.http("GET", "/api/panes", &[("origin", &home_url)], None);
        assert_eq!(status, 200);
        assert!(head.contains(&format!("access-control-allow-origin: {home_url}")), "{head}");

        let evil = [("origin", "https://evil.example"), ("access-control-request-method", "POST")];
        let (_, head, _) = o.http("OPTIONS", "/api/run", &evil, None);
        assert!(!head.contains("access-control-allow-origin"), "{head}");
        let (status, head, _) = o.http("POST", "/api/run", &[("origin", "https://evil.example")], Some(json!({})));
        assert_eq!(status, 403, "{head}");
        assert!(!head.contains("access-control-allow-origin"), "{head}");
    })
    .await
    .unwrap();
}

#[test]
fn tailnet_requests_need_the_owner_except_to_join_with_an_invite() {
    let home = start("home", &[]);
    let login = ("tailscale-user-login", OWNER);
    let host = ("host", PUBLIC);
    // Through serve with the owner's login: fine. Without a login (a tagged
    // node, or Funnel): refused. Someone else: refused.
    assert_eq!(home.http("GET", "/api/hosts", &[host, login], None).0, 200);
    assert_eq!(home.http("GET", "/api/hosts", &[host], None).0, 403);
    assert_eq!(home.http("GET", "/api/hosts", &[host, ("tailscale-user-login", "friend@example.com")], None).0, 403);
    assert_eq!(home.http("POST", "/api/hosts/invite", &[host], None).0, 403, "only the owner mints invites");
    // A foreign Host is refused before anything else, join included.
    assert_eq!(home.http("POST", "/api/hosts/join", &[("host", "evil.example")], Some(json!({}))).0, 421);

    // A sandbox (no identity) joins with an invite, once.
    let (status, _, body) = home.http("POST", "/api/hosts/invite", &[host, login], None);
    assert_eq!(status, 200);
    let token = serde_json::from_str::<Value>(&body).unwrap()["token"].as_str().unwrap().to_owned();
    let join = |token: &str| {
        let body = json!({"token": token, "host": {"name": "sbx", "urls": ["https://sbx.example.ts.net"]}});
        home.http("POST", "/api/hosts/join", &[host], Some(body))
    };
    assert_eq!(join("ilj_not-a-token").0, 403);
    let (status, _, body) = join(&token);
    assert_eq!(status, 200, "{body}");
    // The sandbox learns whom to let in: the home daemon's owner.
    let joined: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(joined["owner"], OWNER);
    assert_eq!(joined["host"]["name"], "sbx");
    assert_eq!(join(&token).0, 403, "an invite is spent");
    let (_, _, list) = home.http("GET", "/api/hosts", &[host, login], None);
    let list: Value = serde_json::from_str(&list).unwrap();
    assert_eq!(list["hosts"][0]["name"], "sbx");
    // Joining doesn't open anything else up.
    assert_eq!(home.http("GET", "/api/hosts/join", &[host], None).0, 403);
    assert_eq!(home.http("POST", "/api/run", &[host], Some(json!({}))).0, 403);
}

/// #17: a pane that runs on another daemon, with its place in the home
/// daemon's layout. The home daemon keeps only `{host, pane}`; the pane is
/// the other daemon's own, in a session named after the home daemon.
#[test]
fn a_remote_pane_runs_there_and_has_its_place_here() {
    let home = start("home", &[]);
    let other = start("other", &[]);
    stdout(&cli(&home, &["hosts", "add", "other", &other.url()]));
    let first = serde_json::from_str::<Value>(&stdout(&cli(&home, &["--json", "ls"]))).unwrap()[0]["id"].clone();

    // A tab here, running there.
    let made: Value =
        serde_json::from_str(&stdout(&cli(&home, &["--host", "other", "--json", "run", "--home", "--cwd", "/tmp"])))
            .unwrap();
    let (block, pane) = (made["block"].as_u64().unwrap(), made["pane"].as_u64().unwrap());
    assert_eq!(made["host"], "other");
    let here: Value = serde_json::from_str(&stdout(&cli(&home, &["--json", "ls"]))).unwrap();
    let b = here.as_array().unwrap().iter().find(|p| p["id"] == block).expect("the block is here");
    assert_eq!(b["type"], "remote");
    let there: Value = serde_json::from_str(&stdout(&cli(&other, &["--json", "ls"]))).unwrap();
    let p = there.as_array().unwrap().iter().find(|p| p["id"] == pane).expect("the pane is there");
    assert_eq!(p["type"], "terminal");
    assert_eq!(p["session_name"], "home", "{p}");
    // A new session there holds just that pane, not a shell beside it.
    let in_session = there.as_array().unwrap().iter().filter(|p| p["session_name"] == "home").count();
    assert_eq!(in_session, 1, "{there}");
    let d: Value = serde_json::from_str(&stdout(&cli(&home, &["--json", "describe", &format!("%{block}")]))).unwrap();
    assert_eq!(d["state"], json!({"host": "other", "pane": pane}), "{d}");

    // Beside a pane here too; a second one joins the same session there.
    let made: Value = serde_json::from_str(&stdout(&cli(
        &home,
        &["--host", "other", "--json", "run", "--home", "--split", &format!("%{first}"), "--", "echo beside; exec cat"],
    )))
    .unwrap();
    let pane2 = made["pane"].as_u64().unwrap();
    let there: Value = serde_json::from_str(&stdout(&cli(&other, &["--json", "ls"]))).unwrap();
    assert_eq!(there.as_array().unwrap().iter().filter(|p| p["session_name"] == "home").count(), 2);
    let tail = || stdout(&cli(&other, &["tail", &format!("%{pane2}"), "--text"]));
    wait_for("its output there", || tail().contains("beside"));

    // Only hosts in the list, and never this daemon itself.
    for (host, why) in [("nope", "no host nope"), ("home", "is this daemon")] {
        let body = json!({"type": "remote", "config": {"host": host, "pane": 1}});
        let (status, _, body) = home.http("POST", "/api/blocks", &[], Some(body));
        assert_eq!(status, 400);
        assert!(body.contains(why), "{body}");
    }

    // The daemon closes only its place: the pane is still there.
    let (status, _, _) = home.http("POST", &format!("/api/panes/{block}/close"), &[], None);
    assert_eq!(status, 200);
    let ids = |d: &Daemon| -> Vec<Value> {
        let v: Value = serde_json::from_str(&stdout(&cli(d, &["--json", "ls"]))).unwrap();
        v.as_array().unwrap().iter().map(|p| p["id"].clone()).collect()
    };
    assert!(!ids(&home).contains(&json!(block)));
    assert!(ids(&other).contains(&json!(pane)));
    // The CLI closes both, as the web client does.
    let block2 = ids(&home).into_iter().find(|id| *id != first).unwrap();
    stdout(&cli(&home, &["close", &format!("%{block2}")]));
    assert!(!ids(&home).contains(&block2));
    wait_for("the pane there to close", || !ids(&other).contains(&json!(pane2)));
}
