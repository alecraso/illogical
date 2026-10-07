//! M27: editor blocks on this host, against a stand-in code-server
//! (`fake_code_server.py`, which takes the real one's flags and serves HTTP
//! on its socket). One server for every block, on a 0600 socket with no
//! auth of its own, reached only through each block's site; the block names
//! its workspace, opens its file as the page loads, reports the file and
//! the lines around the cursor in summaries and captures, comes back after
//! a daemon restart with the file it had, and its server comes back when it
//! has stopped. Guests can't open one. Playwright runs the real code-server
//! (`web/e2e/editors.spec.ts`).

// Over the daemon's Unix socket; Windows gets its named pipe in M56 (#219).
#![cfg(unix)]

use std::{
    io::{BufRead, BufReader, Write},
    net::SocketAddr,
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

use arugula_testkit::{Scratch, arugulad};
use serde_json::{Value, json};

const OWNER: &str = "me@example.com";
const FRIEND: &str = "friend@example.com";

/// A testkit daemon with the stand-in code-server, in a dir of its own
/// (its state, the projects, the servers' log).
struct Daemon {
    d: arugula_testkit::Daemon,
    dir: Scratch,
}

impl std::ops::Deref for Daemon {
    type Target = arugula_testkit::Daemon;
    fn deref(&self) -> &Self::Target {
        &self.d
    }
}

impl std::ops::DerefMut for Daemon {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.d
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.d.halt();
        for pid in self.servers().iter().map(|s| s["pid"].as_i64().unwrap()) {
            let _ = Command::new("kill").arg(pid.to_string()).stderr(Stdio::null()).status();
        }
    }
}

impl Daemon {
    fn new(name: &str, idle: Option<&'static str>) -> Self {
        let dir = Scratch::new(&format!("editors-{name}"));
        let fake = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fake_code_server.py");
        let mut b = arugulad!("editors")
            .state_dir(dir.join("state"))
            .block_listen()
            .no_wisp()
            .args(["--owner", OWNER, "--tailscale-socket", "/nonexistent/sock"])
            .arg("--code-server")
            .arg(fake)
            .env("FAKE_CS_LOG", dir.join("servers.log"))
            // A daemon started in a VS Code terminal doesn't hand that on.
            .env("VSCODE_IPC_HOOK_CLI", "/tmp/not-for-code-server.sock");
        if let Some(i) = idle {
            b = b.env("FAKE_CS_IDLE", i);
        }
        Self { d: b.start(), dir }
    }

    fn block(&self, id: u64) -> Value {
        self.get(&format!("/api/blocks/{id}"))
    }

    fn running(&self, id: u64) {
        wait_for("its server", || self.block(id)["state"]["server"]["is"] == "running");
    }

    /// What the stand-in code-server noted: each start, and each request.
    fn notes(&self) -> Vec<Value> {
        std::fs::read_to_string(self.dir.join("servers.log"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    fn servers(&self) -> Vec<Value> {
        self.notes().into_iter().filter(|n| n["start"] == true).collect()
    }
}

fn wait_for(what: &str, f: impl FnMut() -> bool) {
    arugula_testkit::wait_for(what, Duration::from_secs(15), f);
}

/// A client that resolves the block's name to the block listener, as a
/// browser does for `*.localhost`.
fn client(host: &str, listener: u16) -> reqwest::Client {
    let addr: SocketAddr = format!("127.0.0.1:{listener}").parse().unwrap();
    reqwest::Client::builder().resolve(host, addr).redirect(reqwest::redirect::Policy::none()).build().unwrap()
}

fn project(dir: &Path) -> PathBuf {
    let p = dir.join("proj");
    std::fs::create_dir_all(p.join(".git")).unwrap();
    std::fs::create_dir_all(p.join("src")).unwrap();
    std::fs::create_dir_all(dir.join("other")).unwrap();
    std::fs::write(p.join("src/main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(p.join("src/lib.rs"), "pub fn x() {}\n").unwrap();
    p.canonicalize().unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_editor_on_this_host() {
    let mut d = Daemon::new("here", None);
    let proj = project(&d.dir);
    let main = proj.join("src/main.rs").display().to_string();

    // A file opens in its project, at its line.
    let id = d.post("/api/blocks", json!({"type": "editor", "config": {"path": main, "line": 3}}))["block"]
        .as_u64()
        .unwrap();
    d.running(id);
    let b = d.block(id);
    assert_eq!(b["info"]["type"], "editor");
    assert_eq!(b["info"]["kind"], "editor");
    assert_eq!(b["info"]["project"]["name"], "proj");
    assert_eq!(b["info"]["cwd"], proj.display().to_string());
    assert_eq!(b["info"]["file"], "src/main.rs");
    assert_eq!(b["state"]["line"], 3);
    assert_eq!(b["info"]["attention"], "idle");

    // Its server: code-server's flags, its own folder, a 0600 socket, and a
    // pane's environment without the pane or VS Code's own.
    let started = d.servers();
    assert_eq!(started.len(), 1, "{started:?}");
    let argv: Vec<String> = serde_json::from_value(started[0]["argv"].clone()).unwrap();
    let after =
        |f: &str| argv[argv.iter().position(|a| a == f).unwrap_or_else(|| panic!("no {f}: {argv:?}")) + 1].clone();
    let editor = d.state.join("editor");
    assert_eq!(after("--auth"), "none");
    assert_eq!(after("--user-data-dir"), editor.join("user").display().to_string());
    assert_eq!(after("--extensions-dir"), editor.join("extensions").display().to_string());
    assert_eq!(after("--config"), editor.join("config.yaml").display().to_string());
    assert_eq!(after("--socket-mode"), "600");
    assert!(argv.contains(&"--disable-workspace-trust".to_owned()));
    assert!(!argv.iter().any(|a| a.starts_with("--bind-addr")), "no TCP port: {argv:?}");
    let socket = PathBuf::from(after("--socket"));
    assert_eq!(socket.metadata().unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(started[0]["env"]["ARUGULA_SOCK"], d.sock().display().to_string());
    assert_eq!(started[0]["env"]["ARUGULA_PANE"], Value::Null);
    assert_eq!(started[0]["env"]["VSCODE_IPC_HOOK_CLI"], Value::Null);
    // Its settings start with the theme; the extension is installed.
    let settings = std::fs::read_to_string(editor.join("user/User/settings.json")).unwrap();
    assert!(settings.contains(r#""workbench.colorTheme": "arugula""#), "{settings}");
    let exts = std::fs::read_to_string(editor.join("extensions/extensions.json")).unwrap();
    assert!(exts.contains("arugula.arugula"), "{exts}");

    // Its page: the block's own origin, its workspace (which names the
    // block), and the file opened as it loads.
    let src = b["state"]["src"].as_str().unwrap().to_owned();
    let url = reqwest::Url::parse(&src).unwrap();
    let host = url.host_str().unwrap().to_owned();
    assert!(host.starts_with(&format!("b-{id}-")) && host.ends_with(".localhost"), "{host}");
    assert_eq!(url.port(), Some(d.block_port));
    let q: std::collections::HashMap<String, String> = url.query_pairs().into_owned().collect();
    let ws: Value = serde_json::from_str(&std::fs::read_to_string(&q["workspace"]).unwrap()).unwrap();
    assert_eq!(ws["folders"][0]["path"], proj.display().to_string());
    assert_eq!(ws["settings"]["arugula.block"], id);
    let payload: Value = serde_json::from_str(&q["payload"]).unwrap();
    assert_eq!(payload[0][1], format!("vscode-remote://{host}:{}{main}:3", d.block_port));

    // Through its site only: the server sees a local request.
    let http = client(&host, d.block_port);
    let page = http.get(&src).send().await.unwrap();
    assert_eq!(page.status(), 200);
    assert!(page.text().await.unwrap().contains("fake code-server"));
    // #69: the page, as a frame loads it, has Arugula's storage script
    // first, served from the block's own origin.
    let page = http
        .get(&src)
        .header("sec-fetch-site", "cross-site")
        .header("sec-fetch-mode", "navigate")
        .header("sec-fetch-dest", "iframe")
        .header("accept-encoding", "gzip, br")
        .send()
        .await
        .unwrap();
    assert!(page.headers().get("content-encoding").is_none());
    let text = page.text().await.unwrap();
    assert!(
        text.starts_with(r#"<html><head><script src="/.arugula/head.js"></script><title>fake code-server"#),
        "{text}"
    );
    let js = http.get(format!("http://{host}:{}/.arugula/head.js", d.block_port)).send().await.unwrap();
    assert_eq!(js.headers()["content-type"], "text/javascript; charset=utf-8");
    assert!(js.text().await.unwrap().contains("localStorage"));
    // Another site still can't fetch it.
    let r = http
        .get(format!("http://{host}:{}/.arugula/head.js", d.block_port))
        .header("sec-fetch-site", "cross-site")
        .header("sec-fetch-mode", "no-cors")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    let echo: Value =
        http.get(format!("http://{host}:{}/echo", d.block_port)).send().await.unwrap().json().await.unwrap();
    assert_eq!(echo["headers"]["host"], "localhost");
    let wrong = format!("b-{id}-wrongkeywrongkeywrong.localhost");
    let r = client(&wrong, d.block_port).get(format!("http://{wrong}:{}/", d.block_port)).send().await.unwrap();
    assert_eq!(r.status(), 404, "a wrong key");

    // The extension's report: the file and the lines around the cursor,
    // in its summary and its capture (the swarm's preview).
    let lib = proj.join("src/lib.rs").display().to_string();
    let lines = json!(["// 8", "// 9", "pub fn x() {}", "// 11"]);
    d.post(
        &format!("/api/blocks/{id}/call/report"),
        json!({"file": lib, "line": 10, "col": 5, "top": 8, "lines": lines, "dirty": 1}),
    );
    let b = d.block(id);
    assert_eq!(b["info"]["file"], "src/lib.rs");
    assert_eq!(b["info"]["title"], "lib.rs — proj");
    assert_eq!(b["state"]["top"], 8);
    let (_, text) = d.raw("GET", &format!("/api/panes/{id}/capture"), None);
    assert_eq!(text, "src/lib.rs:10\n// 8\n// 9\npub fn x() {}\n// 11\n");
    let (_, ls) = d.raw("GET", "/api/panes", None);
    assert!(ls.contains("\"file\":\"src/lib.rs\""), "{ls}");

    // M28: the window's extension joins the swarm as this block (its
    // workspace names it), not as an editor of its own: the block reports
    // through it, and it can be followed.
    let mut link = UnixStream::connect(d.sock()).unwrap();
    write!(
        link,
        "GET /api/editors/connect HTTP/1.1\r\nHost: x\r\nConnection: Upgrade\r\nUpgrade: arugula-editor\r\n\r\n"
    )
    .unwrap();
    let mut r = BufReader::new(link.try_clone().unwrap());
    let mut line = String::new();
    r.read_line(&mut line).unwrap();
    assert!(line.starts_with("HTTP/1.1 101"), "{line}");
    while {
        line.clear();
        r.read_line(&mut line).unwrap();
        !line.trim().is_empty()
    } {}
    writeln!(link, "{}", json!({"t": "hello", "editor": "code-server", "workspace": proj, "block": id})).unwrap();
    line.clear();
    r.read_line(&mut line).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&line).unwrap(), json!({"t": "welcome", "id": id}));
    writeln!(link, "{}", json!({"t": "peek", "file": lib, "line": 10, "col": 9, "top": 8, "lines": lines, "dirty": 0}))
        .unwrap();
    writeln!(link, "{}", json!({"t": "summary", "diag": {"e": 1, "w": 0, "i": 0}})).unwrap();
    wait_for("the peek", || d.block(id)["state"]["col"] == 9 && d.block(id)["state"]["dirty"] == 0);
    let b = d.block(id);
    assert_eq!(b["info"]["file"], "src/lib.rs");
    assert_eq!(b["info"]["editor"]["app"], "code-server");
    assert_eq!(b["info"]["editor"]["diag"]["e"], 1);
    assert_eq!(d.get("/api/editors").as_array().unwrap().len(), 1, "the block, once");
    assert_eq!(d.get("/api/editors")[0]["block"], true);
    drop(r);
    drop(link);
    wait_for("its link to go", || d.block(id)["info"]["editor"].is_null());

    // A second block shares the server.
    let other = d.dir.join("other").canonicalize().unwrap();
    let id2 =
        d.post("/api/blocks", json!({"type": "editor", "config": {"path": other.display().to_string()}}))["block"]
            .as_u64()
            .unwrap();
    d.running(id2);
    assert_eq!(d.block(id2)["info"]["project"], Value::Null);
    assert_eq!(d.block(id2)["state"]["folder"], other.display().to_string());
    assert_eq!(d.servers().len(), 1);

    // A restart: the block comes back with the file it had, on the server
    // that kept running.
    let pid = started[0]["pid"].as_i64().unwrap();
    d.stop();
    d.start();
    d.running(id);
    let b = d.block(id);
    assert_eq!(b["info"]["file"], "src/lib.rs");
    let src2 = b["state"]["src"].as_str().unwrap().to_owned();
    assert!(src2.starts_with(&format!("http://{host}:{}/", d.block_port)), "same origin: {src2}");
    let q: std::collections::HashMap<String, String> =
        reqwest::Url::parse(&src2).unwrap().query_pairs().into_owned().collect();
    let payload: Value = serde_json::from_str(&q["payload"]).unwrap();
    assert_eq!(payload[0][1], format!("vscode-remote://{host}:{}{lib}:10", d.block_port));
    assert_eq!(d.servers().len(), 1, "the same server");
    assert_eq!(http.get(&src2).send().await.unwrap().status(), 200);

    // Its server gone (a reboot, or stopped when idle): the next request
    // starts another.
    let _ = Command::new("kill").arg(pid.to_string()).status();
    wait_for("it to go", || UnixStream::connect(&socket).is_err());
    assert_eq!(http.get(&src2).send().await.unwrap().status(), 200);
    assert_eq!(d.servers().len(), 2);

    // Closed: its name stops working and its workspace goes.
    assert_eq!(d.raw("POST", &format!("/api/panes/{id}/close"), Some(json!({}))).0, 200);
    let r = client(&host, d.block_port).get(&src2).send().await.unwrap();
    assert_eq!(r.status(), 404);
    assert!(!Path::new(&q["workspace"]).exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_servers_stop_and_come_back() {
    let d = Daemon::new("idle", Some("0.5"));
    let proj = project(&d.dir);
    let id = d.post("/api/blocks", json!({"type": "editor", "config": {"path": proj.display().to_string()}}))["block"]
        .as_u64()
        .unwrap();
    d.running(id);
    let src = d.block(id)["state"]["src"].as_str().unwrap().to_owned();
    let host = reqwest::Url::parse(&src).unwrap().host_str().unwrap().to_owned();
    // Nobody uses it: it stops, and the block says so.
    wait_for("stopped", || d.block(id)["state"]["server"]["is"] == "stopped");
    assert_eq!(d.block(id)["info"]["attention"], "idle");
    // Someone looks: it starts again.
    let r = client(&host, d.block_port).get(&src).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(d.servers().len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opening_one_needs_the_owner() {
    let d = Daemon::new("guests", None);
    let proj = project(&d.dir).display().to_string();
    // A path that isn't there, and one that isn't whole, are refused.
    let (status, body) =
        d.raw("POST", "/api/blocks", Some(json!({"type": "editor", "config": {"path": "/nowhere/x"}})));
    assert_eq!(status, 400, "{body}");
    let (status, _) = d.raw("POST", "/api/blocks", Some(json!({"type": "editor", "config": {"path": "rel/x"}})));
    assert_eq!(status, 400);

    // Share the session: as a viewer, then as an editor.
    let panes = d.get("/api/panes");
    let pane = panes[0]["id"].as_u64().unwrap();
    let session = panes[0]["session"].as_u64().unwrap();
    let http = reqwest::Client::new();
    let open = || {
        http.post(format!("http://127.0.0.1:{}/api/blocks", d.port))
            .header("tailscale-user-login", FRIEND)
            .json(&json!({"type": "editor", "config": {"path": proj}, "from_pane": pane, "split": pane}))
            .send()
    };
    for role in ["viewer", "editor"] {
        d.post("/api/acl", json!({"session": session, "principal": format!("tailnet:{FRIEND}"), "role": role}));
        let r = open().await.unwrap();
        let status = r.status().as_u16();
        let text = r.text().await.unwrap();
        assert!(status == 400 || status == 403, "{role}: {status} {text}");
    }
    assert_eq!(d.get("/api/panes").as_array().unwrap().len(), 1, "no block was made");
    assert!(d.servers().is_empty(), "no server was started");

    // The owner opens one beside the pane, in its directory.
    let b = d.post("/api/blocks", json!({"type": "editor", "config": {}, "split": pane, "from_pane": pane}));
    let id = b["block"].as_u64().unwrap();
    let cwd = d.get("/api/panes").as_array().unwrap().iter().find(|p| p["id"] == pane).unwrap()["cwd"].clone();
    assert_eq!(d.block(id)["state"]["folder"], cwd);
}
