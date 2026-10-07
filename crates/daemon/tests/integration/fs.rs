//! M7: the `fs` methods on this host and, through the provider, on a
//! sandbox with no daemon of ours in it (a real wispd; skipped without its
//! token), `cd` only into an idle shell, panes started in a directory, and
//! generated names. Sprites this makes are named `arugula-m7-…` and
//! deleted afterwards, whatever happens.

// Over the daemon's Unix socket; Windows gets its named pipe in M56 (#219).
#![cfg(unix)]

use std::{path::PathBuf, process::Command, time::Duration};

use arugula_testkit::arugulad;
use serde_json::{Value, json};

const WISP: &str = "http://127.0.0.1:7788";

fn token() -> Option<String> {
    let file = std::env::var_os("ARUGULA_WISP_TOKEN_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap()).join(".local/share/wisp/token"));
    std::fs::read_to_string(file).ok().map(|t| t.trim().to_owned()).filter(|t| !t.is_empty())
}

fn curl(token: &str, method: &str, url: &str, body: Option<&str>) -> String {
    let mut c = Command::new("curl");
    c.args(["-s", "-X", method, "-H", &format!("Authorization: Bearer {token}")]);
    if let Some(b) = body {
        c.args(["-H", "Content-Type: application/json", "-d", b]);
    }
    String::from_utf8_lossy(&c.arg(url).output().unwrap().stdout).into_owned()
}

/// A testkit daemon that deletes the machines it left behind.
struct Daemon(arugula_testkit::Daemon);

impl std::ops::Deref for Daemon {
    type Target = arugula_testkit::Daemon;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.0.halt();
        // Machines a failed test left behind: this daemon's only.
        if let (Some(token), Ok(id)) = (token(), std::fs::read_to_string(self.state.join("daemon-id"))) {
            let list = curl(&token, "GET", &format!("{WISP}/v1/sprites?prefix=illogical-eph-{}-", id.trim()), None);
            let names: Value = serde_json::from_str(&list).unwrap_or_default();
            for n in names["sprites"].as_array().into_iter().flatten().filter_map(|s| s["name"].as_str()) {
                curl(&token, "DELETE", &format!("{WISP}/v1/sprites/{n}"), None);
            }
        }
    }
}

impl Daemon {
    fn new(tag: &str, wisp: bool) -> Self {
        let b = arugulad!(&format!("m7-{tag}"));
        Self(if wisp { b.args(["--wisp-url", WISP]) } else { b.no_wisp() }.start())
    }

    fn pane(&self, id: u64) -> Value {
        self.get("/api/panes").as_array().unwrap().iter().find(|p| p["id"] == id).cloned().unwrap_or(Value::Null)
    }

    fn wait_for(&self, what: &str, secs: u64, f: impl FnMut() -> bool) {
        arugula_testkit::wait_for(what, Duration::from_secs(secs), f);
    }
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~/".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// A GET's response head over the socket, lowercase.
fn head_of(d: &Daemon, path: &str) -> String {
    use std::io::{BufRead, BufReader, Write};
    let mut s = std::os::unix::net::UnixStream::connect(d.sock()).unwrap();
    write!(s, "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").unwrap();
    let mut r = BufReader::new(s);
    let mut head = String::new();
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).unwrap() == 0 || line.trim().is_empty() {
            break;
        }
        head.push_str(&line);
    }
    head.to_ascii_lowercase()
}

fn names(list: &Value) -> Vec<String> {
    list["entries"].as_array().unwrap().iter().map(|e| e["name"].as_str().unwrap().to_owned()).collect()
}

#[test]
fn this_hosts_files_cd_and_names() {
    let d = Daemon::new("local", false);
    // Physical path: on macOS the temp dir is under /var, a link to /private/var.
    let root = std::env::temp_dir().canonicalize().unwrap().join(format!("ilg-m7-files-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src/deep")).unwrap();
    std::fs::write(root.join("notes.txt"), "0123456789").unwrap();
    std::os::unix::fs::symlink("/proc/self/environ", root.join("env")).unwrap();
    let r = root.display().to_string();

    // The first session's name is generated.
    let first = d.get("/api/panes")[0].clone();
    let name = first["session_name"].as_str().unwrap();
    assert!(name.split(' ').count() == 2 && name.chars().all(|c| c.is_ascii_lowercase() || c == ' '), "{name}");

    let list = d.get(&format!("/api/fs/list?path={}", enc(&r)));
    assert_eq!(names(&list), ["env", "notes.txt", "src"]);
    assert_eq!(names(&d.get(&format!("/api/fs/list?path={}&dirs=1", enc(&r)))), ["src"]);
    let (status, body) = d.raw("GET", &format!("/api/fs/read?path={}/notes.txt&offset=3&len=4", enc(&r)), None);
    assert_eq!((status, body.as_str()), (200, "3456"));
    // #505: its size and offset under both names, for an older CLI.
    let head = head_of(&d, &format!("/api/fs/read?path={}/notes.txt&offset=3&len=4", enc(&r)));
    for h in
        ["x-arugula-size: 10\r\n", "x-illogical-size: 10\r\n", "x-arugula-offset: 3\r\n", "x-illogical-offset: 3\r\n"]
    {
        assert!(head.contains(h), "{head}");
    }
    assert_eq!(d.get(&format!("/api/fs/stat?path={}/notes.txt", enc(&r)))["size"], 10);
    // Refused: kernel files, a link into them, the daemon's own state.
    // (macOS has no /proc: there the link leads nowhere, a 404.)
    let mut refused = vec!["/proc/self/environ".to_owned(), d.state.join("daemon-id").display().to_string()];
    if cfg!(any(target_os = "linux", target_os = "android")) {
        refused.push(format!("{r}/env"));
    }
    for p in refused {
        let (status, body) = d.raw("GET", &format!("/api/fs/read?path={}", enc(&p)), None);
        assert_eq!(status, 403, "{p}: {body}");
    }
    assert_eq!(d.raw("GET", "/api/fs/list?path=/nonexistent-m7", None).0, 404);

    // A shell started in a directory, then cd'd elsewhere at its prompt.
    let pane = d.post("/api/run", json!({ "cwd": format!("{r}/src") }))["pane"].as_u64().unwrap();
    d.wait_for("the shell in src", 10, || d.pane(pane)["cwd"] == format!("{r}/src"));
    d.wait_for("its prompt", 10, || {
        d.raw("POST", &format!("/api/panes/{pane}/cd"), Some(json!({"path": format!("{r}/src/deep")}))).0 == 200
    });
    d.wait_for("the cd", 10, || d.pane(pane)["cwd"] == format!("{r}/src/deep"));
    // Busy: refused, and nothing typed.
    d.post(&format!("/api/panes/{pane}/send"), json!({"text": "sleep 20", "enter": true}));
    d.wait_for("the command to start", 10, || !d.pane(pane)["current"].is_null());
    let (status, body) = d.raw("POST", &format!("/api/panes/{pane}/cd"), Some(json!({ "path": r })));
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("running"), "{body}");

    // Recent: where panes are and were, newest first.
    let recent = d.get("/api/fs/recent");
    assert!(recent.as_array().unwrap().iter().any(|x| *x == format!("{r}/src/deep")), "{recent}");

    std::fs::remove_dir_all(&root).unwrap();
}

/// A sandbox we create for the test, deleted when dropped.
struct Sprite {
    token: String,
    name: String,
}

impl Drop for Sprite {
    fn drop(&mut self) {
        curl(&self.token, "DELETE", &format!("{WISP}/v1/sprites/{}", self.name), None);
    }
}

#[test]
fn a_sandbox_through_the_provider() {
    let Some(token) = token() else {
        eprintln!("SKIP: no wisp token on this host (ARUGULA_WISP_TOKEN_FILE or ~/.local/share/wisp/token)");
        return;
    };
    let sprite = Sprite { token: token.clone(), name: format!("arugula-m7-{}", std::process::id()) };
    curl(&token, "POST", &format!("{WISP}/v1/sprites"), Some(&json!({ "name": sprite.name }).to_string()));
    let d = Daemon::new("vm", true);
    // A shell on it, nothing installed there.
    let pane = d.post("/api/run", json!({ "sandbox": sprite.name }))["pane"].as_u64().unwrap();
    d.wait_for("the guest prompt", 60, || d.pane(pane)["cwd"] == "/home/sprite");
    d.post(
        &format!("/api/panes/{pane}/send"),
        json!({"text": "mkdir -p ~/m7/inner && printf abcdef > ~/m7/f.txt && ln -sfn /proc ~/m7/p && echo MADE-$((6*7))", "enter": true}),
    );
    d.wait_for("the files", 20, || d.raw("GET", &format!("/api/fs/stat?pane={pane}&path=~/m7/f.txt"), None).0 == 200);

    let list = d.get(&format!("/api/fs/list?pane={pane}&path=~/m7"));
    assert_eq!(list["path"], "/home/sprite/m7");
    assert_eq!(names(&list), ["f.txt", "inner", "p"]);
    assert_eq!(names(&d.get(&format!("/api/fs/list?pane={pane}&path=/home/sprite/m7&dirs=1"))), ["inner"]);
    let (status, body) = d.raw("GET", &format!("/api/fs/read?pane={pane}&path=~/m7/f.txt&offset=2&len=3"), None);
    assert_eq!((status, body.as_str()), (200, "cde"));
    // The same, by machine.
    let m = d.pane(pane)["host"].as_u64().unwrap();
    assert_eq!(d.get(&format!("/api/fs/stat?machine={m}&path=/home/sprite/m7/f.txt"))["size"], 6);
    // No /proc, by path or through a link the sandbox made.
    assert_eq!(d.raw("GET", &format!("/api/fs/read?pane={pane}&path=/proc/1/environ"), None).0, 403);
    assert_eq!(d.raw("GET", &format!("/api/fs/read?pane={pane}&path=~/m7/p/1/environ"), None).0, 403);
    assert_eq!(d.raw("GET", &format!("/api/fs/list?pane={pane}&path=~/nope"), None).0, 404);

    // A pane joining it, in a directory there.
    let joined = d.post("/api/run", json!({"split": pane, "join": true, "cwd": "/home/sprite/m7/inner"}))["pane"]
        .as_u64()
        .unwrap();
    d.wait_for("the joined shell", 60, || d.pane(joined)["cwd"] == "/home/sprite/m7/inner");
    let machines = d.get("/api/machines");
    let sprite_of = |p: u64| {
        let h = d.pane(p)["host"].clone();
        machines.as_array().unwrap().iter().find(|m| m["id"] == h).map(|m| m["sprite"].clone())
    };
    assert_eq!(sprite_of(joined), Some(json!(sprite.name)));
    // cd, at its prompt.
    d.wait_for("cd", 20, || {
        d.raw("POST", &format!("/api/panes/{pane}/cd"), Some(json!({"path": "/home/sprite/m7"}))).0 == 200
    });
    d.wait_for("the cd", 20, || d.pane(pane)["cwd"] == "/home/sprite/m7");

    // A VM tab's machine gets a generated name; its sprite keeps its id.
    d.post("/api/run", json!({ "vm_tab": true }));
    let machines = d.get("/api/machines");
    let ours = machines.as_array().unwrap().iter().find(|m| m["borrowed"] != true).unwrap().clone();
    assert!(ours["name"].as_str().is_some_and(|n| n.split(' ').count() == 2), "{ours}");
    assert!(ours["sprite"].as_str().unwrap().starts_with("illogical-eph-"));
    // Borrowed ones go by their own name.
    assert!(machines.as_array().unwrap().iter().filter(|m| m["borrowed"] == true).all(|m| m["name"].is_null()));
}
