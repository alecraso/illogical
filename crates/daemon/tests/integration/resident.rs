//! M4b against a real wispd: a shell on a sandbox with no daemon there,
//! then a daemon made resident in it and reached through the home daemon's
//! provider tunnel by the CLI (`--host`), which nothing else on the
//! sandbox's loopback can use without the home daemon's token. Skips
//! without a wisp token or the static build (`just static`). Going cold is
//! in `web/e2e/resident.spec.ts`.

// Over the daemon's Unix socket; Windows gets its named pipe in M56 (#219).
#![cfg(unix)]

use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
    time::Duration,
};

use arugula_testkit::arugulad;
use serde_json::{Value, json};

const WISP: &str = "http://127.0.0.1:7788";

fn token() -> Option<String> {
    let file = std::env::var_os("ARUGULA_WISP_TOKEN_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap()).join(".local/share/wisp/token"));
    std::fs::read_to_string(file).ok().map(|t| t.trim().to_owned()).filter(|t| !t.is_empty())
}

fn static_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/x86_64-unknown-linux-musl/release")
}

/// The Sprites API, with curl (the test's own client).
fn wisp(method: &str, path: &str, body: Option<Value>) -> Value {
    let auth = format!("Authorization: Bearer {}", token().unwrap());
    let mut c = Command::new("curl");
    c.args(["-s", "-X", method, "-H", &auth]);
    if let Some(b) = body {
        c.args(["-H", "Content-Type: application/json", "-d", &b.to_string()]);
    }
    let out = c.arg(format!("{WISP}/v1/sprites{path}")).output().unwrap();
    serde_json::from_slice(&out.stdout).unwrap_or_default()
}

/// The home daemon, which deletes the sandbox when it goes.
struct Home {
    d: arugula_testkit::Daemon,
    sprite: String,
}

impl Drop for Home {
    fn drop(&mut self) {
        self.d.halt();
        wisp("DELETE", &format!("/{}", self.sprite), None);
    }
}

fn cli_bin() -> PathBuf {
    let bin = Path::new(env!("CARGO_BIN_EXE_arugulad")).with_file_name("arugula");
    let status = Command::new(env!("CARGO")).args(["build", "-q", "-p", "arugula"]).status().unwrap();
    assert!(status.success(), "building the CLI");
    bin
}

fn cli(home: &Home, args: &[&str]) -> Output {
    Command::new(cli_bin()).arg("--socket").arg(home.d.sock()).args(args).env_remove("ARUGULA_PANE").output().unwrap()
}

fn stdout(o: &Output) -> String {
    assert!(o.status.success(), "{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn wait_for(what: &str, f: impl FnMut() -> bool) {
    arugula_testkit::wait_for(what, Duration::from_secs(30), f);
}

#[test]
fn a_shell_then_a_resident_daemon_through_the_tunnel() {
    if token().is_none() || !static_dir().join("arugulad").exists() {
        eprintln!("SKIP: needs wispd's token (ARUGULA_WISP_TOKEN_FILE or ~/.local/share/wisp/token) and `just static`");
        return;
    }
    let sprite = format!("arugula-m4b-test-{}", std::process::id());
    let d = arugulad!("resident")
        .args(["--name", "home", "--wisp-url", WISP])
        .no_tailscale()
        .arg("--static-dir")
        .arg(static_dir())
        .start();
    let home = Home { d, sprite: sprite.clone() };
    assert_eq!(wisp("POST", "", Some(json!({ "name": sprite })))["name"], sprite.as_str());

    // Listed, with what the provider can do.
    let list: Value = serde_json::from_str(&stdout(&cli(&home, &["--json", "sandboxes"]))).unwrap();
    assert_eq!(list["provider"]["name"], "wisp");
    assert_eq!(list["provider"]["resident"], true);
    assert!(list["sandboxes"].as_array().unwrap().iter().any(|s| s["name"] == sprite.as_str()), "{list}");

    // A shell there with no daemon: a pane here on a borrowed machine.
    let pane = stdout(&cli(&home, &["run", "--sandbox", &sprite, "--", "echo on-$(hostname)-$((6*7))"]));
    let pane = pane.trim();
    wait_for("its output", || stdout(&cli(&home, &["tail", pane, "--text"])).contains(&format!("on-{sprite}-42")));
    let machines: Value = serde_json::from_str(&stdout(&cli(&home, &["--json", "machines"]))).unwrap();
    assert_eq!(machines[0]["borrowed"], true, "{machines}");
    stdout(&cli(&home, &["close", pane]));
    wait_for("the machine let go", || stdout(&cli(&home, &["--json", "machines"])).trim() == "[]");
    assert_eq!(wisp("GET", &format!("/{sprite}"), None)["name"], sprite.as_str(), "the sandbox stays");

    // Resident: copied in, a service, and a host reached through the tunnel.
    stdout(&cli(&home, &["sandboxes", "promote", &sprite, "--as", "res"]));
    let hosts: Value = serde_json::from_str(&stdout(&cli(&home, &["--json", "hosts"]))).unwrap();
    let h = &hosts["hosts"][0];
    assert_eq!((h["name"].as_str(), h["transport"].as_str()), (Some("res"), Some("provider")), "{hosts}");
    assert_eq!(h["provider"]["sandbox"], sprite.as_str());
    assert!(!hosts.to_string().contains("ilp_"), "the tunnel token never leaves the home daemon");
    let p = stdout(&cli(&home, &["--host", "res", "run", "--", "echo resident-$(hostname)-$((7*6))"]));
    let p = p.trim();
    wait_for("output through the tunnel", || {
        stdout(&cli(&home, &["--host", "res", "tail", p, "--text"])).contains(&format!("resident-{sprite}-42"))
    });
    // The service is the provider's to keep running.
    let services = wisp("GET", &format!("/{sprite}/services"), None);
    assert!(services.to_string().contains("arugulad"), "{services}");

    // Anything else on the sandbox's loopback is refused without the token.
    let probe = "curl -s -o /dev/null -w code-%{http_code} http://127.0.0.1:7681/api/panes";
    let q = stdout(&cli(&home, &["run", "--sandbox", &sprite, "--", probe]));
    let q = q.trim();
    wait_for("the refusal", || stdout(&cli(&home, &["tail", q, "--text"])).contains("code-401"));

    // Stopped: the service is gone and so is the host.
    stdout(&cli(&home, &["sandboxes", "demote", &sprite]));
    let hosts: Value = serde_json::from_str(&stdout(&cli(&home, &["--json", "hosts"]))).unwrap();
    assert!(hosts["hosts"].as_array().unwrap().is_empty(), "{hosts}");
    assert!(!cli(&home, &["--host", "res", "ls"]).status.success());
}
