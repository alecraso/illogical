//! M33: Claude Code conversations as agent blocks, against a seeded
//! `$CLAUDE_CONFIG_DIR` and the fake ACP agent standing in for
//! `claude-agent-acp` (through `$ILLOGICAL_AGENTS_DIR`). Listing, opening
//! (no process), following the transcript, continuing (`session/resume`
//! with the imported session's settings and model), a restart, and a
//! session held by another process: continuing refused, forking allowed.

mod agentd;

use std::{path::PathBuf, time::Duration};

use agentd::*;
use serde_json::{Value, json};

struct Claude {
    root: PathBuf,
    /// `$CLAUDE_CONFIG_DIR`.
    dir: PathBuf,
    /// The conversations' folder.
    work: PathBuf,
    agents: PathBuf,
}

impl Drop for Claude {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Claude {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("ilg-conv-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (dir, work, agents) = (root.join("claude"), root.join("work"), root.join("agents"));
        std::fs::create_dir_all(dir.join("sessions")).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        // The fake agent as Claude Code's adapter.
        let bin = agents.join("claude/node_modules/.bin");
        std::fs::create_dir_all(&bin).unwrap();
        let shim = bin.join("claude-agent-acp");
        std::fs::write(&shim, format!("#!/bin/sh\nexec python3 {} \"$@\"\n", fake())).unwrap();
        std::fs::set_permissions(&shim, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        Self { root, dir, work, agents }
    }

    fn env(&self) -> Vec<(&'static str, String)> {
        vec![
            ("CLAUDE_CONFIG_DIR", self.dir.display().to_string()),
            ("ILLOGICAL_AGENTS_DIR", self.agents.display().to_string()),
        ]
    }

    fn transcript(&self, id: &str) -> PathBuf {
        let slug = self.work.display().to_string().replace(['/', '.'], "-");
        let d = self.dir.join("projects").join(slug);
        std::fs::create_dir_all(&d).unwrap();
        d.join(format!("{id}.jsonl"))
    }

    /// A terminal session: a prompt, a reply, a command and its output.
    fn seed(&self, id: &str, word: &str, entrypoint: &str) -> PathBuf {
        let cwd = self.work.display().to_string();
        let line = |v: Value| format!("{v}\n");
        let base = |t: &str, uuid: &str, parent: Option<&str>| {
            json!({ "type": t, "uuid": uuid, "parentUuid": parent, "sessionId": id, "cwd": cwd,
                    "gitBranch": "main", "entrypoint": entrypoint, "version": "2.1.288",
                    "timestamp": "2026-10-02T20:00:00.000Z", "isSidechain": false })
        };
        let mut text = String::new();
        let mut v = base("user", "u1", None);
        v["message"] = json!({ "role": "user", "content": format!("remember {word}") });
        text += &line(v);
        let mut v = base("assistant", "a1", Some("u1"));
        v["message"] = json!({ "id": "msg1", "role": "assistant", "model": "claude-haiku-4-5-20251001",
                               "content": [{ "type": "text", "text": "Noted." }] });
        text += &line(v);
        let mut v = base("assistant", "a2", Some("a1"));
        v["message"] = json!({ "id": "msg1", "role": "assistant", "model": "claude-haiku-4-5-20251001",
                               "content": [{ "type": "tool_use", "id": "toolu_1", "name": "Bash",
                                             "input": { "command": "echo hi", "description": "Say hi" } }] });
        text += &line(v);
        let mut v = base("user", "u2", Some("a2"));
        v["message"] = json!({ "role": "user", "content": [{ "type": "tool_result", "tool_use_id": "toolu_1", "content": "hi" }] });
        text += &line(v);
        text += &line(json!({ "type": "ai-title", "aiTitle": format!("Remembering {word}"), "sessionId": id }));
        let p = self.transcript(id);
        std::fs::write(&p, text).unwrap();
        p
    }

    /// This test process holds `id`, as a running Claude Code would.
    fn hold(&self, id: &str) {
        let me = std::process::id();
        // Without /proc (macOS) only the pid is checked.
        let start = std::fs::read_to_string(format!("/proc/{me}/stat"))
            .map_or("0".to_owned(), |s| s.rsplit_once(')').unwrap().1.split_whitespace().nth(19).unwrap().to_owned());
        std::fs::write(
            self.dir.join(format!("sessions/{me}.json")),
            json!({ "pid": me, "sessionId": id, "procStart": start, "kind": "interactive",
                    "entrypoint": "cli", "status": "idle", "cwd": self.work })
            .to_string(),
        )
        .unwrap();
    }
}

fn daemon(c: &Claude) -> Daemon {
    let env = c.env();
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    Daemon::child_env(&[], &env)
}

fn texts(s: &Value) -> Vec<String> {
    entries(s).iter().filter_map(|e| e["text"].as_str().map(str::to_owned)).collect()
}

fn last_reply(s: &Value) -> String {
    entries(s).iter().rev().find(|e| e["type"] == "agent").and_then(|e| e["text"].as_str()).unwrap_or("").to_owned()
}

fn conversations(d: &Daemon, q: &str) -> Vec<Value> {
    d.get(&format!("/api/conversations?{q}"))["conversations"].as_array().cloned().unwrap_or_default()
}

const ID: &str = "11111111-2222-4333-8444-555555555555";
const HELD: &str = "99999999-2222-4333-8444-555555555555";
const OTHER: &str = "77777777-2222-4333-8444-555555555555";

#[test]
fn a_conversation_opens_stopped_follows_and_continues() {
    let c = Claude::new("open");
    let path = c.seed(ID, "kestrel", "cli");
    c.seed(OTHER, "sparrow", "sdk-ts");
    let mut d = daemon(&c);

    // Listed: a terminal one, with its title and folder. An SDK run only
    // with `all`.
    let list = conversations(&d, "");
    assert_eq!(list.len(), 1, "{list:#?}");
    let one = &list[0];
    assert_eq!((one["id"].as_str(), one["source"].as_str()), (Some(ID), Some("terminal")));
    assert_eq!(one["title"], "Remembering kestrel");
    assert_eq!(one["first_prompt"], "remember kestrel");
    assert_eq!(one["cwd"], c.work.display().to_string());
    assert_eq!(one["block"], Value::Null);
    assert_eq!(conversations(&d, "all=1").len(), 2);
    assert_eq!(conversations(&d, "q=nothing-like-it").len(), 0);

    // Opened by a prefix: a stopped block with its transcript; nothing runs.
    let v = d.post("/api/conversations/open", json!({ "id": "11111111" }));
    let id = v["block"].as_u64().unwrap();
    assert_eq!(v["opened"], true);
    let s = d.state(id);
    assert_eq!(s["status"], "stopped", "{s}");
    assert_eq!(s["pid"], Value::Null);
    assert_eq!(s["import"]["continued"], false);
    assert_eq!(s["import"]["source"], "terminal");
    assert_eq!(s["title"], "Remembering kestrel");
    assert!(texts(&s).contains(&"remember kestrel".to_owned()), "{s}");
    let tool = last_tool(&s);
    assert_eq!(
        (tool["title"].as_str(), tool["status"].as_str(), tool["output"].as_str()),
        (Some("echo hi"), Some("completed"), Some("hi"))
    );
    // Opening it again is the same block, and the list says so.
    let again = d.post("/api/conversations/open", json!({ "id": ID }));
    assert_eq!((again["block"].as_u64(), again["opened"].as_bool()), (Some(id), Some(false)));
    assert_eq!(conversations(&d, "")[0]["block"], id);

    // It follows the transcript as it grows.
    let mut more = std::fs::read_to_string(&path).unwrap();
    more += &json!({ "type": "user", "uuid": "u3", "parentUuid": "u2", "sessionId": ID, "cwd": c.work,
                     "entrypoint": "cli", "message": { "role": "user", "content": "a later prompt" } })
    .to_string();
    more.push('\n');
    std::fs::write(&path, more).unwrap();
    d.wait_for("the new prompt", || texts(&d.state(id)).contains(&"a later prompt".to_owned()));

    // Continuing: the session resumes with its context, your settings and
    // CLAUDE.md, no hooks, and the model it had.
    d.call(id, "send", json!({ "text": "recall" }));
    assert_eq!(d.wait(id, "idle"), "done");
    let s = d.state(id);
    assert_eq!(last_reply(&s), "You said kestrel.", "{s}");
    assert_eq!(s["import"]["continued"], true);
    assert_eq!(s["session_id"], ID);
    assert!(texts(&s).contains(&"Continued in illogical".to_owned()));
    assert!(d.state.join(format!("blocks/{id}/imported.json")).is_file());
    d.call(id, "send", json!({ "text": "meta" }));
    d.wait(id, "idle");
    let meta: Value = serde_json::from_str(last_reply(&d.state(id)).trim_start_matches("META ")).unwrap();
    assert_eq!(
        meta,
        json!({ "claudeCode": { "options": { "settingSources": ["user", "project", "local"], "settings": { "disableAllHooks": true } } } })
    );
    d.call(id, "send", json!({ "text": "model" }));
    d.wait(id, "idle");
    assert_eq!(last_reply(&d.state(id)), "Model: haiku");

    // A restart: what it had before it was continued, and since.
    d.stop();
    d.start();
    let s = d.state(id);
    let t = texts(&s);
    assert!(t.contains(&"remember kestrel".to_owned()) && t.contains(&"You said kestrel.".to_owned()), "{s}");
    assert_eq!(s["import"]["continued"], true);
    d.call(id, "send", json!({ "text": "recall" }));
    assert_eq!(d.wait(id, "idle"), "done");
    assert_eq!(last_reply(&d.state(id)), "You said kestrel.");
}

#[test]
fn a_held_conversation_is_forked_not_shared() {
    let c = Claude::new("held");
    let path = c.seed(HELD, "plover", "cli");
    c.hold(HELD);
    let d = daemon(&c);
    let list = conversations(&d, "live=1");
    assert_eq!(list.len(), 1, "{list:#?}");
    assert_eq!(list[0]["live"]["pid"], std::process::id());
    // "open in a terminal (pid N)", or the pane this test runs in.
    let place = list[0]["live"]["place"].as_str().unwrap();
    assert!(place.contains("terminal") || place.contains("pane %"), "{}", list[0]);

    // Continuing is refused while it's held, with where.
    let v = d.post("/api/conversations/open", json!({ "id": HELD, "then": "continue" }));
    let id = v["block"].as_u64().unwrap();
    assert!(v["error"].as_str().unwrap().contains("fork it instead"), "{v}");
    let s = d.state(id);
    assert_eq!(s["status"], "stopped");
    assert_eq!(s["import"]["held"]["pid"], std::process::id());
    let (status, body) = d.raw("POST", &format!("/api/blocks/{id}/call/send"), Some(json!({ "text": "recall" })));
    assert_eq!(status, 400, "{body}");

    // Forking: a new session with its history; the original untouched.
    let before = std::fs::read(&path).unwrap();
    d.call(id, "fork", json!({}));
    d.wait_for("the fork", || d.state(id)["session_id"].as_str().is_some_and(|s| s.starts_with("fork-")));
    d.wait_for("ready", || d.state(id)["status"] == "ready");
    d.call(id, "send", json!({ "text": "recall" }));
    assert_eq!(d.wait(id, "idle"), "done");
    let s = d.state(id);
    assert_eq!(last_reply(&s), "You said plover.", "{s}");
    assert!(texts(&s).iter().any(|t| t.starts_with("Forked into a new session")), "{s}");
    assert_eq!(s["import"]["held"], Value::Null, "the fork is the block's own");
    assert_eq!(std::fs::read(&path).unwrap(), before);

    // Once the holder goes, the list says so.
    std::fs::remove_file(c.dir.join(format!("sessions/{}.json", std::process::id()))).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    assert!(conversations(&d, "live=1").is_empty());
}
