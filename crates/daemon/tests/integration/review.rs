//! M11: diff and file blocks, on this host (`web/e2e/changes.spec.ts` has
//! them on a VM, from a phone).
//!
//! A diff block lists a repository's working tree against HEAD (staged,
//! unstaged, untracked, binary, renamed, too big), one revision against the
//! working tree, and a range; opens a file's hunks; `capture --text` is the
//! unified diff, and `illogical diff` prints the list. A file block shows a
//! file at a line, follows it through edits keeping its line, but only
//! while a client draws it, as does the diff. A shared session's viewer
//! sees both in their state and can't change them. And a failed command
//! can be run again from its reason (Rerun).

// Over the daemon's Unix socket; Windows gets its named pipe in M56 (#219).
#![cfg(unix)]

use crate::agentd;

use std::{
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use agentd::*;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::{connect_async, tungstenite::Message};

const OWNER: &str = "me@example.com";
const FRIEND: &str = "friend@example.com";

fn cli_bin() -> PathBuf {
    let bin = Path::new(env!("CARGO_BIN_EXE_illogicald")).with_file_name("illogical");
    let status = Command::new(env!("CARGO")).args(["build", "-q", "-p", "illogical"]).status().unwrap();
    assert!(status.success(), "building the CLI");
    bin
}

fn cli(d: &Daemon, args: &[&str]) -> String {
    let out = Command::new(cli_bin()).arg("--socket").arg(d.sock()).args(args).output().unwrap();
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.email=t@example.com", "-c", "user.name=t", "-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .unwrap();
    assert!(ok.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&ok.stderr));
}

/// A repository with two commits, then changes of every kind.
fn repo(d: &Daemon) -> PathBuf {
    let r = d.sessions.join("repo");
    std::fs::create_dir_all(r.join("src")).unwrap();
    git(&r, &["init", "-q", "-b", "main"]);
    std::fs::write(r.join("src/a.rs"), "fn a() {}\nfn b() {}\nfn c() {}\n").unwrap();
    std::fs::write(r.join("old.txt"), (1..=40).map(|i| format!("line {i}\n")).collect::<String>()).unwrap();
    std::fs::write(r.join("gone.txt"), "bye\n").unwrap();
    std::fs::write(r.join("big.txt"), "x\n").unwrap();
    git(&r, &["add", "."]);
    git(&r, &["commit", "-qm", "one"]);
    std::fs::write(r.join("second.txt"), "2\n").unwrap();
    git(&r, &["add", "."]);
    git(&r, &["commit", "-qm", "two"]);
    // Unstaged, staged, deleted, renamed, untracked, binary, too big.
    std::fs::write(r.join("src/a.rs"), "fn a() {}\nfn B() {}\nfn c() {}\nfn d() {}\n").unwrap();
    std::fs::write(r.join("staged.txt"), "new\n").unwrap();
    git(&r, &["add", "staged.txt"]);
    git(&r, &["rm", "-q", "gone.txt"]);
    git(&r, &["mv", "old.txt", "moved.txt"]);
    std::fs::write(r.join("notes.md"), "# notes\nhello\n").unwrap();
    std::fs::write(r.join("blob.bin"), [0u8, 1, 2, 3, 0, 255]).unwrap();
    std::fs::write(r.join("big.txt"), "y\n".repeat(200_000)).unwrap();
    r
}

/// A block's state once it has read what it shows.
fn loaded(d: &Daemon, id: u64) -> Value {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let s = d.state(id);
        if s["loading"] != true {
            return s;
        }
        assert!(Instant::now() < deadline, "%{id} never loaded: {s}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn files(s: &Value) -> Vec<(String, String, u64, u64)> {
    s["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            let n = |k: &str| f[k].as_u64().unwrap_or(0);
            (f["path"].as_str().unwrap().to_owned(), f["status"].as_str().unwrap().to_owned(), n("add"), n("del"))
        })
        .collect()
}

fn file<'a>(s: &'a Value, path: &str) -> &'a Value {
    s["files"].as_array().unwrap().iter().find(|f| f["path"] == path).unwrap_or_else(|| panic!("no {path} in {s}"))
}

#[test]
fn a_repository_s_changes_every_way() {
    let d = Daemon::child();
    let r = repo(&d);
    let id = d.open_with(json!({ "type": "diff", "config": { "repo": r.join("src") } }));
    let s = loaded(&d, id);
    assert_eq!(s["error"], Value::Null, "{s}");
    assert_eq!(s["repo"], r.canonicalize().unwrap().display().to_string());
    assert_eq!(s["against"], "the working tree against HEAD");
    let mut got = files(&s);
    got.sort();
    let big = got.iter().find(|f| f.0 == "big.txt").unwrap().clone();
    assert_eq!(big.1, "modified");
    assert!(file(&s, "big.txt")["big"] == true, "a 400 KB diff is too big to show: {s}");
    got.retain(|f| f.0 != "big.txt");
    let want = |p: &str, st: &str, a, dl| (p.to_owned(), st.to_owned(), a, dl);
    assert_eq!(
        got,
        [
            want("blob.bin", "untracked", 0, 0),
            want("gone.txt", "deleted", 0, 1),
            want("moved.txt", "renamed", 0, 0),
            want("notes.md", "untracked", 2, 0),
            want("src/a.rs", "modified", 2, 1),
            want("staged.txt", "added", 1, 0),
        ]
    );
    assert_eq!(file(&s, "blob.bin")["binary"], true);
    assert_eq!(file(&s, "moved.txt")["old"], "old.txt");
    // Nothing is open yet: no hunks in the state.
    assert!(file(&s, "src/a.rs").get("hunks").is_none());

    // Open one: its hunks, both sides numbered.
    assert_eq!(d.call(id, "file", json!({ "path": "src/a.rs" }))["open"], true);
    let s = d.state(id);
    let h = &file(&s, "src/a.rs")["hunks"][0];
    assert_eq!(h["at"], "@@ -1,3 +1,4 @@");
    assert_eq!(h["lines"][1], json!(["-", 2, 2, "fn b() {}"]));
    assert_eq!(h["lines"][2], json!(["+", 3, 2, "fn B() {}"]));
    assert_eq!(h["lines"][4], json!(["+", 4, 4, "fn d() {}"]));
    let (status, body) = d.raw("POST", &format!("/api/blocks/{id}/call/file"), Some(json!({ "path": "nope" })));
    assert_eq!(status, 400, "{body}");

    // capture --text is the unified diff; describe has the state.
    let text = cli(&d, &["capture", &format!("%{id}")]);
    assert!(text.contains("diff --git a/src/a.rs b/src/a.rs") && text.contains("+fn B() {}"), "{text}");
    assert!(text.contains("+hello") && text.contains("rename from old.txt"), "{text}");
    assert!(text.contains("too big to show here"), "{text}");
    let desc: Value = serde_json::from_str(&cli(&d, &["--json", "describe", &format!("%{id}")])).unwrap();
    assert_eq!(desc["info"]["type"], "diff");
    assert_eq!(desc["info"]["title"], "Changes in repo");

    // One revision: against the working tree; a range; a bad one.
    let one = d.open_with(json!({ "type": "diff", "config": { "repo": r, "rev_a": "HEAD~1" } }));
    let s = loaded(&d, one);
    assert!(files(&s).iter().any(|f| f.0 == "second.txt" && f.1 == "added"), "{s}");
    assert!(files(&s).iter().any(|f| f.0 == "notes.md"), "untracked too: {s}");
    let range = d.open_with(json!({ "type": "diff", "config": { "repo": r, "rev_a": "HEAD~1", "rev_b": "HEAD" } }));
    let s = loaded(&d, range);
    assert_eq!(files(&s), [("second.txt".to_owned(), "added".to_owned(), 1, 0)]);
    assert_eq!(s["against"], "HEAD~1..HEAD");
    let bad = d.open_with(json!({ "type": "diff", "config": { "repo": r, "rev_a": "nope" } }));
    assert_eq!(loaded(&d, bad)["error"], "no such revision: nope");
    let (status, _) = d.raw(
        "POST",
        "/api/blocks",
        Some(json!({ "type": "diff", "config": { "repo": r, "rev_a": "--output=/tmp/x" } })),
    );
    assert_eq!(status, 400, "an option isn't a revision");
    let not = d.open_with(json!({ "type": "diff", "config": { "repo": "/" } }));
    assert!(loaded(&d, not)["error"].as_str().unwrap().contains("not a git repository"));

    // `illogical diff` from a pane: that pane's repository, then the list.
    let pane = d.post("/api/run", json!({ "cwd": r.join("src") }))["pane"].as_u64().unwrap();
    d.wait_for("the pane's directory", || {
        d.get("/api/panes").as_array().unwrap().iter().any(|p| p["id"] == pane && p["cwd"].is_string())
    });
    let out = cli(&d, &["diff", &format!("%{pane}"), "HEAD~1", "HEAD"]);
    let mut lines = out.lines();
    assert!(lines.next().unwrap().starts_with('%'));
    assert_eq!(lines.next().unwrap().split_whitespace().collect::<Vec<_>>(), ["added", "second.txt", "+1", "-0"]);
    assert_eq!(lines.next().unwrap(), "1 file changed, +1 -0 (HEAD~1..HEAD)");

    // `illogical view`: a path here, or from a pane's directory.
    let block = |out: String| out.trim().trim_start_matches('%').parse::<u64>().unwrap();
    let here = block(cli(&d, &["view", &format!("{}:2", r.join("notes.md").display())]));
    assert_eq!(cli(&d, &["capture", &format!("%{here}")]), "# notes\nhello\n");
    assert_eq!(loaded(&d, here)["line"], 2);
    let there = block(cli(&d, &["view", &format!("%{pane}:a.rs:3")]));
    let s = loaded(&d, there);
    assert_eq!(
        (s["real"].as_str(), s["line"].as_u64()),
        (Some(r.canonicalize().unwrap().join("src/a.rs").to_str().unwrap()), Some(3))
    );
}

/// A client that draws the tab `tab`, until dropped.
async fn draw(d: &Daemon, tab: u64) -> tokio::task::JoinHandle<()> {
    let (mut ws, _) = connect_async(d.ws("/ws")).await.unwrap();
    let view = json!({ "type": "view", "tab": tab, "cols": 80, "rows": 24, "zoom": null, "claim": false });
    ws.send(Message::Text(view.to_string().into())).await.unwrap();
    tokio::spawn(async move { while ws.next().await.is_some() {} })
}

fn tab_of(d: &Daemon, id: u64) -> u64 {
    d.get("/api/panes").as_array().unwrap().iter().find(|p| p["id"] == id).unwrap()["tab"].as_u64().unwrap()
}

fn wait_state(d: &Daemon, id: u64, what: &str, f: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let s = d.state(id);
        if f(&s) {
            return s;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}: {s}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn live_only_while_drawn() {
    let d = Daemon::child();
    let r = repo(&d);
    let f = r.join("src/a.rs");
    let id = d.open_with(json!({ "type": "file", "config": { "path": f, "line": 3 } }));
    let s = loaded(&d, id);
    assert_eq!(
        (s["text"].as_str(), s["line"].as_u64()),
        (Some("fn a() {}\nfn B() {}\nfn c() {}\nfn d() {}\n"), Some(3))
    );
    assert_eq!(s["watching"], false);
    let text = cli(&d, &["capture", &format!("%{id}")]);
    assert_eq!(text, "fn a() {}\nfn B() {}\nfn c() {}\nfn d() {}\n");
    // The diff, split beside it: on the same host, the file's repository.
    let diff = d.open_with(json!({ "type": "diff", "config": {}, "from_pane": id, "split": id }));
    assert_eq!(loaded(&d, diff)["repo"], r.canonicalize().unwrap().display().to_string());

    // Nobody draws them: edits aren't seen.
    std::fs::write(&f, "// top\nfn a() {}\nfn B() {}\nfn c() {}\nfn d() {}\n").unwrap();
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let s = d.state(id);
    assert_eq!((s["rev"].as_u64(), s["watching"].as_bool()), (Some(1), Some(false)), "{s}");
    assert_eq!(file(&d.state(diff), "src/a.rs")["add"], 2);

    // A client draws their tab: both watch, catch up, and follow edits; the
    // marked line stays on its text.
    let client = draw(&d, tab_of(&d, id)).await;
    let s = wait_state(&d, id, "the edit", |s| s["watching"] == true && s["rev"] == 2);
    assert_eq!(s["line"], 4, "{s}");
    wait_state(&d, diff, "the diff's catching up", |s| s["watching"] == true && file(s, "src/a.rs")["add"] == 3);
    std::fs::write(&f, "// top\n// more\nfn a() {}\nfn B() {}\nfn c() {}\nfn d() {}\n").unwrap();
    let s = wait_state(&d, id, "the second edit", |s| s["rev"] == 3);
    assert_eq!(s["line"], 5);
    assert_eq!(s["jump"], 1, "following an edit doesn't scroll anyone");
    wait_state(&d, diff, "the diff's following", |s| file(s, "src/a.rs")["add"] == 4);
    d.call(id, "goto", json!({ "line": 1 }));
    let s = d.state(id);
    assert_eq!((s["line"].as_u64(), s["jump"].as_u64()), (Some(1), Some(2)));

    // It goes: they stop.
    client.abort();
    let _ = client.await;
    wait_state(&d, id, "not watching", |s| s["watching"] == false);
    wait_state(&d, diff, "not watching", |s| s["watching"] == false);
    std::fs::write(&f, "changed while nobody looked\n").unwrap();
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(d.state(id)["rev"], 3);

    // A summaries-only client (the swarm) doesn't count.
    let (mut ws, _) = connect_async(d.ws("/ws")).await.unwrap();
    let tab = tab_of(&d, id);
    for m in [
        json!({ "type": "subscribe", "summary": true }),
        json!({ "type": "view", "tab": tab, "cols": 80, "rows": 24, "zoom": null, "claim": false }),
    ] {
        ws.send(Message::Text(m.to_string().into())).await.unwrap();
    }
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(d.state(id)["watching"], false);
    drop(ws);

    // The owner points it at another file; a path that isn't served is
    // refused.
    let other = r.join("notes.md");
    let v = d.call(id, "open", json!({ "path": other, "line": 2 }));
    assert_eq!(v["line"], 2);
    assert_eq!(d.state(id)["text"], "# notes\nhello\n");
    let (status, body) =
        d.raw("POST", &format!("/api/blocks/{id}/call/open"), Some(json!({ "path": "/proc/self/environ" })));
    assert_eq!(status, 400, "{body}");
    let (status, _) = d.raw(
        "POST",
        "/api/blocks",
        Some(json!({ "type": "file", "config": { "path": d.state.join("layout.json") } })),
    );
    assert_eq!(status, 400, "the daemon's own state isn't served");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_viewer_sees_them_and_cant_change_them() {
    let d = Daemon::child_with(&[
        "--wisp-token-file",
        "/nonexistent",
        "--owner",
        OWNER,
        "--tailscale-socket",
        "/nonexistent/sock",
    ]);
    let r = repo(&d);
    let panes = d.get("/api/panes");
    let (pane, session) = (panes[0]["id"].as_u64().unwrap(), panes[0]["session"].as_u64().unwrap());
    let file =
        d.open_with(json!({ "type": "file", "config": { "path": r.join("src/a.rs"), "line": 2 }, "split": pane }));
    let diff = d.open_with(json!({ "type": "diff", "config": { "repo": r }, "split": pane }));
    loaded(&d, file);
    loaded(&d, diff);
    d.call(diff, "file", json!({ "path": "src/a.rs" }));
    d.post("/api/acl", json!({ "session": session, "principal": format!("tailnet:{FRIEND}"), "role": "viewer" }));

    // What a viewer's client is sent: both states, the hunks and the text.
    let mut req = tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(format!(
        "ws://127.0.0.1:{}/ws",
        d.port
    ))
    .unwrap();
    req.headers_mut().insert("tailscale-user-login", FRIEND.parse().unwrap());
    let (mut ws, _) = connect_async(req).await.unwrap();
    let (mut saw_file, mut saw_diff) = (None, None);
    let until = Instant::now() + Duration::from_secs(10);
    while (saw_file.is_none() || saw_diff.is_none()) && Instant::now() < until {
        let Ok(Some(Ok(Message::Text(t)))) = tokio::time::timeout(Duration::from_secs(5), ws.next()).await else {
            break;
        };
        let v: Value = serde_json::from_str(&t).unwrap();
        if v["type"] == "block" && v["block"] == file {
            saw_file = Some(v["state"].clone());
        }
        if v["type"] == "block" && v["block"] == diff {
            saw_diff = Some(v["state"].clone());
        }
    }
    let (f, s) = (saw_file.expect("the file's state"), saw_diff.expect("the diff's state"));
    assert_eq!((f["text"].as_str().map(|t| t.lines().count()), f["line"].as_u64()), (Some(4), Some(2)));
    assert_eq!(a_rs(&s)["hunks"][0]["at"], "@@ -1,3 +1,4 @@");
    drop(ws);

    // ...and can't change what they show, or open more.
    let http = reqwest::Client::new();
    let post = |path: String, body: Value| {
        http.post(format!("http://127.0.0.1:{}{path}", d.port))
            .header("tailscale-user-login", FRIEND)
            .json(&body)
            .send()
    };
    for (path, body) in [
        (format!("/api/blocks/{diff}/call/file"), json!({ "path": "src/a.rs" })),
        (format!("/api/blocks/{diff}/call/refresh"), json!({})),
        (format!("/api/blocks/{file}/call/goto"), json!({ "line": 1 })),
        (format!("/api/blocks/{file}/call/open"), json!({ "path": "/etc/hostname" })),
        ("/api/blocks".into(), json!({ "type": "file", "config": { "path": "/etc/hostname" }, "split": pane })),
    ] {
        let st = post(path.clone(), body).await.unwrap().status().as_u16();
        assert!(st == 403 || st == 400, "{path}: {st}");
    }
    // A viewer may describe them (their state) but not read files.
    let get = |path: String| {
        http.get(format!("http://127.0.0.1:{}{path}", d.port)).header("tailscale-user-login", FRIEND).send()
    };
    assert_eq!(get(format!("/api/blocks/{file}")).await.unwrap().status().as_u16(), 200);
    assert_eq!(get("/api/fs/read?path=/etc/hostname".into()).await.unwrap().status().as_u16(), 403);
    assert!(d.state(diff)["files"].as_array().unwrap().iter().any(|f| f["open"] == true), "still open");

    // An editor may change what's drawn, but not point the file block at
    // another file of the owner's.
    d.post("/api/acl", json!({ "session": session, "principal": format!("tailnet:{FRIEND}"), "role": "editor" }));
    let st = post(format!("/api/blocks/{file}/call/goto"), json!({ "line": 3 })).await.unwrap().status().as_u16();
    assert_eq!(st, 200);
    let res = post(format!("/api/blocks/{file}/call/open"), json!({ "path": "/etc/hostname" })).await.unwrap();
    assert_eq!(res.status().as_u16(), 400);
    assert!(res.text().await.unwrap().contains("only the owner"));
}

fn a_rs(s: &Value) -> &Value {
    file(s, "src/a.rs")
}

/// `illogical attention --json`, this pane's reason.
fn reason_of(d: &Daemon, pane: u64) -> Option<Value> {
    let out = cli(d, &["attention", "--json"]);
    let v: Value = serde_json::from_str(&out).unwrap();
    v.as_array().unwrap().iter().find(|i| i["pane"] == pane).map(|i| i["reason"].clone())
}

#[test]
fn a_failed_build_runs_again() {
    let d = Daemon::child();
    let pane = d.post("/api/run", json!({}))["pane"].as_u64().unwrap();
    d.wait_for("a prompt", || {
        d.get("/api/panes").as_array().unwrap().iter().any(|p| p["id"] == pane && p["running"] == true)
    });
    std::thread::sleep(Duration::from_millis(500));
    let count = d.sessions.join("runs");
    let def = format!("build() {{ echo x >> {}; sleep 3.2; return 2; }}", count.display());
    d.post(&format!("/api/panes/{pane}/send"), json!({ "text": def, "enter": true }));
    // Not idle yet: refused, and it says why.
    d.post(&format!("/api/panes/{pane}/send"), json!({ "text": "build --all", "enter": true }));
    d.wait_for("it started", || count.exists());
    let (status, body) = d.raw("POST", "/api/attention/act", Some(json!({ "action": "rerun", "pane": pane })));
    assert_eq!(status, 409, "{body}");
    d.wait_for("the failure", || reason_of(&d, pane).is_some());
    let r = reason_of(&d, pane).unwrap();
    assert_eq!((r["kind"].as_str(), r["command"].as_str()), (Some("failed"), Some("build --all")), "{r}");
    assert_eq!(r["actions"], json!(["rerun", "dismiss"]));

    // Rerun: typed again, and it runs (and fails) again.
    cli(&d, &["rerun", &format!("%{pane}")]);
    d.wait_for("the second run", || std::fs::read_to_string(&count).unwrap_or_default().lines().count() == 2);
    d.wait_for("its failure again", || reason_of(&d, pane).is_some_and(|r| r["kind"] == "failed"));
    let history = d.get(&format!("/api/history?pane={pane}&limit=10"));
    let runs = history.as_array().unwrap().iter().filter(|c| c["text"] == "build --all").count();
    assert_eq!(runs, 2, "{history}");
}
