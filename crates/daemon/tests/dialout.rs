//! M4c end to end: a sandbox that can only dial out. It reaches the home
//! daemon over one WebSocket with a per-host token, shows up in the home
//! daemon's host list, and is used through it (the CLI's `--host`, the
//! client WebSocket and API at `/h/NAME/...`), never the other way round.
//! It pushes its history, which the home daemon keeps encrypted and still
//! answers from after the sandbox is gone. Both daemons are real binaries on
//! loopback; nothing ever connects to the sandbox's own port.

mod listen;
mod strays;

use std::{
    io::{Read, Write},
    net::TcpStream,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::atomic::{AtomicU32, Ordering},
    time::{Duration, Instant},
};

use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};

const PUBLIC: &str = "home.example.ts.net";
const OWNER: &str = "me@example.com";

struct Daemon {
    child: Child,
    port: u16,
    state: PathBuf,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        strays::remove(&self.state);
    }
}

fn temp(what: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let d = std::env::temp_dir().join(format!(
        "ilg-dial-{what}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn start(name: &str, extra: &[&str]) -> Daemon {
    let state = temp(name);
    let child = Command::new(env!("CARGO_BIN_EXE_illogicald"))
        .args(["--listen", listen::ANY, "--shell", "bash --norc --noprofile", "--no-manager-env"])
        .args(["--name", name, "--tailscale-socket", "/nonexistent/tailscaled.sock"])
        .args(["--public-host", PUBLIC, "--owner", OWNER])
        .args(extra)
        .arg("--state-dir")
        .arg(&state)
        .env("PS1", "$ ")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut d = Daemon { child, port: 0, state };
    d.port = listen::wait_port(&d.state);
    let deadline = Instant::now() + Duration::from_secs(10);
    while std::os::unix::net::UnixStream::connect(d.sock()).is_err()
        || TcpStream::connect(("127.0.0.1", d.port)).is_err()
    {
        assert!(Instant::now() < deadline, "daemon did not start");
        std::thread::sleep(Duration::from_millis(50));
    }
    d
}

impl Daemon {
    fn sock(&self) -> PathBuf {
        match std::fs::read_to_string(self.state.join("sock.path")) {
            Ok(p) => PathBuf::from(p.trim()),
            Err(_) => self.state.join("sock"),
        }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// The local token loopback callers show.
    fn token(&self) -> String {
        std::fs::read_to_string(self.state.join("local-token")).unwrap_or_default().trim().to_owned()
    }

    /// One HTTP request over TCP with these headers; status, headers, body.
    fn http(&self, method: &str, path: &str, headers: &[(&str, &str)], body: Option<Value>) -> (u16, String, String) {
        let mut s = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        let body = body.map(|b| b.to_string()).unwrap_or_default();
        let mut req = format!("{method} {path} HTTP/1.1\r\nConnection: close\r\nContent-Length: {}\r\n", body.len());
        if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("host")) {
            req.push_str(&format!("Host: 127.0.0.1:{}\r\n", self.port));
        }
        for (k, v) in headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        // A program on this machine shows the local token (serve's
        // requests carry an identity instead, and hosts' paths a token of
        // their own).
        let own_credential = ["/api/sync/", "/api/hosts/join", "/api/dial"].iter().any(|p| path.starts_with(p));
        if !own_credential
            && !self.token().is_empty()
            && !headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("authorization") || k.eq_ignore_ascii_case("tailscale-user-login"))
        {
            req.push_str(&format!("Authorization: Bearer {}\r\n", self.token()));
        }
        if !body.is_empty() {
            req.push_str("Content-Type: application/json\r\n");
        }
        req.push_str("\r\n");
        req.push_str(&body);
        s.write_all(req.as_bytes()).unwrap();
        let mut res = Vec::new();
        s.read_to_end(&mut res).unwrap();
        let res = String::from_utf8_lossy(&res).into_owned();
        let (head, body) = res.split_once("\r\n\r\n").unwrap_or((&res, ""));
        let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
        (status, head.to_ascii_lowercase(), body.to_owned())
    }

    fn get(&self, path: &str) -> Value {
        let (status, _, body) = self.http("GET", path, &[], None);
        assert_eq!(status, 200, "GET {path}: {body}");
        // Chunked bodies: the JSON is the line that parses.
        serde_json::from_str(&body)
            .or_else(|_| body.lines().find_map(|l| serde_json::from_str(l).ok()).ok_or(()))
            .unwrap_or_else(|_| panic!("GET {path}: not JSON: {body}"))
    }
}

fn cli_bin() -> PathBuf {
    let bin = Path::new(env!("CARGO_BIN_EXE_illogicald")).with_file_name("illogical");
    let status = Command::new(env!("CARGO")).args(["build", "-q", "-p", "illogical"]).status().unwrap();
    assert!(status.success(), "building the CLI");
    bin
}

fn cli(home: &Daemon, args: &[&str]) -> Output {
    Command::new(cli_bin()).arg("--socket").arg(home.sock()).args(args).env_remove("ILLOGICAL_PANE").output().unwrap()
}

fn stdout(o: &Output) -> String {
    assert!(o.status.success(), "{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn wait_for(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A token minted on `home` for `name`, in a private file (in home's state
/// dir, so it goes with it).
fn token_file(home: &Daemon, name: &str) -> (PathBuf, String) {
    let token = stdout(&cli(home, &["hosts", "token", name])).trim().to_owned();
    assert!(token.starts_with("ilh_"), "{token}");
    let f = home.state.join(format!("test-token-{name}"));
    std::fs::write(&f, &token).unwrap();
    (f, token)
}

fn sandbox(home: &Daemon, name: &str, token: &Path, extra: &[&str]) -> Daemon {
    let peer = format!("ws://127.0.0.1:{}", home.port);
    let mut args = vec!["--peer", &peer, "--token", token.to_str().unwrap()];
    args.extend_from_slice(extra);
    start(name, &args)
}

fn host_entry(home: &Daemon, name: &str) -> Option<Value> {
    let list: Value = serde_json::from_str(&stdout(&cli(home, &["--json", "hosts"]))).unwrap();
    list["hosts"].as_array()?.iter().find(|h| h["name"] == name).cloned()
}

/// Every file under `dir`.
fn files(dir: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(files(&p));
        } else {
            out.push(p);
        }
    }
    out
}

#[test]
fn a_sandbox_that_only_dials_out_is_used_through_home_and_its_history_outlives_it() {
    let home = start("home", &[]);
    let (token, _) = token_file(&home, "sbx");
    // Listed before it ever connects: a dial-out host has no URL.
    let entry = host_entry(&home, "sbx").unwrap();
    assert_eq!((entry["transport"].as_str(), entry["urls"].clone()), (Some("dial_out"), json!([])));

    let sbx = sandbox(&home, "sbx", &token, &["--sync", "--sync-live", "--sync-every", "1"]);
    wait_for("the sandbox to dial in", 15, || {
        host_entry(&home, "sbx").is_some_and(|h| h["last_seen_ms"].is_u64())
            && home.http("GET", "/h/sbx/api/host", &[], None).0 == 200
    });
    let me = home.get("/h/sbx/api/host");
    assert_eq!(me["name"], "sbx", "through home, it's the sandbox answering");

    // The CLI's --host goes through the home daemon.
    let run = cli(&home, &["--host", "sbx", "run", "--wait", "--", "echo via-tunnel-$((6*7)); exit 3"]);
    assert_eq!(run.status.code(), Some(3), "{}", String::from_utf8_lossy(&run.stderr));
    let pane = String::from_utf8_lossy(&run.stdout).trim().trim_start_matches('%').to_owned();
    let tail = stdout(&cli(&home, &["--host", "sbx", "tail", &format!("%{pane}"), "--text"]));
    assert!(tail.contains("via-tunnel-42"), "{tail}");
    let there: Value = serde_json::from_str(&stdout(&cli(&home, &["--host", "sbx", "--json", "ls"]))).unwrap();
    assert_eq!(there.as_array().unwrap().len(), 2);
    let here: Value = serde_json::from_str(&stdout(&cli(&home, &["--json", "ls"]))).unwrap();
    assert_eq!(here.as_array().unwrap().len(), 1, "nothing new on the home daemon");
    // Streaming responses work through the tunnel too (follow, then stop).
    let mut follow = Command::new(cli_bin())
        .arg("--socket")
        .arg(home.sock())
        .args(["--host", "sbx", "tail", &format!("%{pane}"), "-f", "--text"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut first = [0u8; 64];
    let n = follow.stdout.as_mut().unwrap().read(&mut first).unwrap();
    assert!(n > 0);
    follow.kill().unwrap();
    let _ = follow.wait();

    // The client protocol through home, for the home page's origin only.
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let ws = |origin: &str| {
            let mut req = format!("ws://127.0.0.1:{}/h/sbx/ws", home.port).into_client_request().unwrap();
            req.headers_mut().insert("origin", origin.parse().unwrap());
            req.headers_mut().insert("authorization", format!("Bearer {}", home.token()).parse().unwrap());
            connect_async(req)
        };
        let (mut sock, _) = ws(&home.url()).await.expect("the home page reaches the sandbox through home");
        let hello = loop {
            if let Some(Ok(Message::Text(t))) = sock.next().await {
                break serde_json::from_str::<Value>(&t).unwrap();
            }
        };
        assert_eq!(hello["type"], "hello");
        let panes: Vec<u64> =
            hello["state"]["panes"].as_array().unwrap().iter().filter_map(|p| p["id"].as_u64()).collect();
        assert!(panes.contains(&pane.parse().unwrap()), "{panes:?}");
        assert!(ws("https://evil.example").await.is_err());
        assert!(ws(&sbx.url()).await.is_err(), "the sandbox's own page is nothing to home");
    });

    // Home's access rules hold for the way through: not the owner, no way.
    let (status, _, _) = home.http("GET", "/h/sbx/api/panes", &[("host", PUBLIC)], None);
    assert_eq!(status, 403, "a tagged node or Funnel (serve, no user)");
    let friend = [("host", PUBLIC), ("tailscale-user-login", "friend@example.com")];
    assert_eq!(home.http("GET", "/h/sbx/api/panes", &friend, None).0, 403);
    // What comes back is defanged: it's served on home's origin.
    let (status, head, _) = home.http("GET", "/h/sbx/api/panes", &[], None);
    assert_eq!(status, 200);
    assert!(head.contains("x-content-type-options: nosniff"), "{head}");
    assert!(head.contains("content-security-policy: sandbox"), "{head}");
    // Only its WebSocket and API, and nothing that leads anywhere else.
    assert_eq!(home.http("GET", "/h/sbx/", &[], None).0, 404);
    assert_eq!(home.http("GET", "/h/sbx/index.html", &[], None).0, 404);
    assert_eq!(home.http("GET", "/h/sbx/api/sync/state", &[], None).0, 404, "no home routes over a tunnel");
    assert_eq!(home.http("GET", "/h/sbx/h/other/api/host", &[], None).0, 404);
    assert_eq!(home.http("GET", "/h/nope/api/host", &[], None).0, 502);

    // History: pushed as it grows, kept encrypted.
    stdout(&cli(&home, &["--host", "sbx", "close", &format!("%{pane}")]));
    wait_for("the closed pane to sync", 20, || {
        let s = home.get("/api/synced");
        s.as_array().unwrap().iter().any(|h| h["name"] == "sbx" && h["panes"][&pane]["closed_ms"].is_u64())
    });
    let key = home.state.join("synced/key");
    let mode = std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&key).unwrap().permissions());
    assert_eq!(mode & 0o777, 0o600);
    let synced = files(&home.state.join("synced/sbx"));
    assert!(synced.iter().any(|f| f.to_string_lossy().ends_with(".enc")), "{synced:?}");
    for f in &synced {
        let raw = std::fs::read(f).unwrap();
        assert!(!raw.windows(13).any(|w| w == b"via-tunnel-42"), "plaintext in {}", f.display());
        assert!(!raw.windows(9).any(|w| w == b"exit 3; e" || w == b"via-tunne"), "plaintext in {}", f.display());
    }

    // Revoking the token cuts it off for good: redials are refused.
    stdout(&cli(&home, &["hosts", "revoke", "sbx"]));
    wait_for("the tunnel to drop", 10, || home.http("GET", "/h/sbx/api/host", &[], None).0 == 502);
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(home.http("GET", "/h/sbx/api/host", &[], None).0, 502, "a revoked token doesn't get back in");

    // The sandbox is deleted, and taken off the list.
    drop(sbx);
    stdout(&cli(&home, &["hosts", "rm", "sbx"]));
    // Its history is still searchable here: asked for directly...
    let hits = stdout(&cli(&home, &["search", "via-tunnel-[0-9]+", "--synced", "sbx"]));
    assert!(hits.contains(&format!("sbx:%{pane}@")) && hits.contains("via-tunnel-42"), "{hits}");
    let all: Value =
        serde_json::from_str(&stdout(&cli(&home, &["--json", "search", "via-tunnel-42", "--synced", "all"]))).unwrap();
    assert_eq!(all[0]["host"], "sbx");
    // ...or with --host, which falls back to it when the host is gone.
    let o = cli(&home, &["--host", "sbx", "history"]);
    let out = stdout(&o);
    assert!(out.contains("echo via-tunnel") || out.contains(&format!("sbx:%{pane}")), "{out}");
    assert!(String::from_utf8_lossy(&o.stderr).contains("synced here"));
    let tail = stdout(&cli(&home, &["--host", "sbx", "tail", &format!("%{pane}"), "--text"]));
    assert!(tail.contains("via-tunnel-42"), "{tail}");
    let tail = stdout(&cli(&home, &["tail", &format!("%{pane}"), "--synced", "sbx", "--text"]));
    assert!(tail.contains("via-tunnel-42"), "{tail}");
    // Local history is untouched by it.
    let local = stdout(&cli(&home, &["search", "via-tunnel-42"]));
    assert!(!local.contains("via-tunnel-42"), "{local}");

    // Rotating the key keeps it readable and leaves no old key behind.
    let old_key = std::fs::read_to_string(&key).unwrap();
    stdout(&cli(&home, &["synced", "rotate-key"]));
    assert_ne!(std::fs::read_to_string(&key).unwrap(), old_key);
    let hits = stdout(&cli(&home, &["search", "via-tunnel-42", "--synced", "sbx"]));
    assert!(hits.contains("via-tunnel-42"), "{hits}");
    stdout(&cli(&home, &["synced", "rm", "sbx"]));
    assert!(!stdout(&cli(&home, &["search", "via-tunnel-42", "--synced", "sbx"])).contains("via-tunnel"));
}

#[test]
fn the_sandbox_works_alone_and_reconnects_when_home_comes_back() {
    // The home daemon isn't up yet: the sandbox runs regardless.
    let mut home = start("home", &[]);
    let home_port = home.port;
    let (token, _) = token_file(&home, "lone");
    let home_state = home.state.clone();
    let _ = home.child.kill();
    let _ = home.child.wait();
    let peer = format!("ws://127.0.0.1:{home_port}");
    let sbx = start("lone", &["--peer", &peer, "--token", token.to_str().unwrap()]);
    let alone = cli(&sbx, &["run", "--wait", "--", "echo alone"]);
    assert_eq!(alone.status.code(), Some(0));

    // Home comes up (same state, so the token is known) on the port it dials.
    home.state = temp("unused");
    let child = Command::new(env!("CARGO_BIN_EXE_illogicald"))
        .args(["--listen", &format!("127.0.0.1:{home_port}"), "--shell", "bash --norc --noprofile", "--no-manager-env"])
        .args(["--name", "home", "--tailscale-socket", "/nonexistent/tailscaled.sock", "--owner", OWNER])
        .arg("--state-dir")
        .arg(&home_state)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let home = Daemon { child, port: home_port, state: home_state };
    wait_for("the sandbox to redial", 40, || {
        TcpStream::connect(("127.0.0.1", home_port)).is_ok() && home.http("GET", "/h/lone/api/host", &[], None).0 == 200
    });
    let out = stdout(&cli(&home, &["--host", "lone", "run", "--wait", "--", "echo back"]));
    assert!(out.starts_with('%'));
}

#[tokio::test]
async fn host_tokens_open_nothing_but_their_own_host() {
    let home = std::sync::Arc::new(start("home", &[]));
    let h = home.clone();
    let (a, b) = tokio::task::spawn_blocking(move || (token_file(&h, "a").1, token_file(&h, "b").1)).await.unwrap();
    let dial = |token: Option<&str>| {
        let mut req = format!("ws://127.0.0.1:{}/api/dial", home.port).into_client_request().unwrap();
        if let Some(t) = token {
            req.headers_mut().insert("authorization", format!("Bearer {t}").parse().unwrap());
        }
        connect_async(req)
    };
    let refused = |r: Result<_, tokio_tungstenite::tungstenite::Error>| match r {
        Err(tokio_tungstenite::tungstenite::Error::Http(res)) => res.status().as_u16(),
        Err(e) => panic!("{e}"),
        Ok(_) => 101,
    };
    assert_eq!(refused(dial(None).await), 401);
    assert_eq!(refused(dial(Some("ilh_forged")).await), 403);
    assert_eq!(refused(dial(Some(&a)).await), 101, "a's own token dials in");

    let (h, a2, b2) = (home.clone(), a.clone(), b.clone());
    tokio::task::spawn_blocking(move || {
        let bearer = |t: &str| format!("Bearer {t}");
        // Pushes land under the token's host, whatever the caller says.
        let (s, _, _) = h.http("POST", "/api/sync/1/log?from=0", &[("authorization", &bearer(&a2))], None);
        assert_eq!(s, 200);
        let (s, _, body) = h.http("GET", "/api/sync/state", &[("authorization", &bearer(&b2))], None);
        assert_eq!((s, body.as_str()), (200, r#"{"panes":{}}"#), "b sees nothing of a's");
        assert_eq!(h.http("GET", "/api/sync/state", &[], None).0, 401);
        assert_eq!(h.http("GET", "/api/sync/state", &[("authorization", "Bearer ilh_x")], None).0, 403);
        // A host token is no owner credential, from the tailnet or anywhere.
        for host in [PUBLIC, "127.0.0.1:1"] {
            let hdr = [("host", host), ("authorization", &bearer(&a2))];
            assert_ne!(h.http("GET", "/api/panes", &hdr, None).0, 200);
            assert_ne!(h.http("GET", "/api/synced", &hdr, None).0, 200);
            assert_ne!(h.http("POST", "/api/hosts/a/token", &hdr, None).0, 200);
            assert_ne!(h.http("GET", "/h/a/api/panes", &hdr, None).0, 200);
        }
        // A foreign Host is refused before any token is looked at.
        let hdr = [("host", "evil.example"), ("authorization", &bearer(&a2))];
        assert_eq!(h.http("GET", "/api/sync/state", &hdr, None).0, 421);
        // Revoked: nothing.
        stdout(&cli(&h, &["hosts", "revoke", "a"]));
        assert_eq!(h.http("GET", "/api/sync/state", &[("authorization", &bearer(&a2))], None).0, 403);
        assert!(!cli(&h, &["hosts", "revoke", "a"]).status.success(), "nothing left to revoke");
    })
    .await
    .unwrap();
    assert_eq!(refused(dial(Some(&a)).await), 403);
    assert_eq!(refused(dial(Some(&b)).await), 101);
}
