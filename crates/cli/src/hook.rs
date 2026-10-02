//! Claude Code's hooks for team answers (M29).
//!
//! `illogical hook` takes any hook event on stdin. A `PermissionRequest`
//! (a tool asking to run) becomes an approval card beside the pane, on
//! every client and as a push, and waits: the card's allow, allow always
//! (one of Claude Code's own suggestions) or deny is printed for Claude
//! Code. Claude Code shows its own dialog at the same time, and a "Yes"
//! there never reaches the hook (S18), so every other event (`PreToolUse`,
//! `PostToolUse`, `PostToolUseFailure`, `UserPromptSubmit`) is passed to
//! the daemon, which closes a card the terminal answered first; "No" or Esc
//! there stops the hook, which withdraws its card. AskUserQuestion is left
//! to `illogical ask` (M6c).
//!
//! `illogical inbox` is a background (`asyncRewake`) hook on `Stop` and
//! `SessionStart`: it waits for a follow-up someone sends the agent, and
//! exits 2 with it, which wakes Claude Code and runs it as the next
//! instruction. It never types into the prompt, so it never mixes with
//! what the driver has half typed.
//!
//! Outside an illogical pane both do nothing.

use std::{
    io::{Read, Write},
    time::{Duration, Instant},
};

use serde_json::Value;

use crate::http::{Target, request};

/// How long to keep asking a daemon that doesn't answer before leaving the
/// permission to the terminal.
const GIVE_UP: Duration = Duration::from_secs(60);

fn stdin_hook() -> Option<(u32, Value)> {
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let pane = crate::env_pane()?;
    Some((pane, serde_json::from_str(&input).ok()?))
}

/// `illogical hook`: always exits 0; prints a decision only for a
/// permission someone answered on a card.
pub fn run(sock: Target) -> i32 {
    let Some((pane, hook)) = stdin_hook() else { return 0 };
    if hook["hook_event_name"] != "PermissionRequest" {
        let _ = request(&sock, "POST", &format!("/api/panes/{pane}/hook"), Some(&hook));
        return 0;
    }
    if hook["tool_name"].as_str().is_none_or(|t| t == illogical_proto::ask::ASK_USER_QUESTION) {
        return 0;
    }
    crate::ask::withdraw_on_signals(sock.clone(), pane, None);
    let mut failing_since: Option<Instant> = None;
    loop {
        match request(&sock, "POST", &format!("/api/panes/{pane}/permit"), Some(&hook)) {
            Ok(res) if res.status == 503 => {}
            Ok(res) => {
                let Ok(v) = res.json() else { return 0 };
                if matches!(v["action"].as_str(), Some("allow" | "deny")) {
                    let mut out = std::io::stdout().lock();
                    let _ = writeln!(out, "{}", v["output"]);
                    let _ = out.flush();
                }
                return 0;
            }
            Err(_) => {}
        }
        let since = *failing_since.get_or_insert_with(Instant::now);
        if since.elapsed() > GIVE_UP {
            eprintln!("illogical hook: the daemon isn't answering; answer in the terminal");
            return 0;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// `illogical inbox`: exits 2 with a follow-up (Claude Code's
/// `asyncRewake` wakes the agent with it), or 0 when a newer waiter takes
/// over or there's nothing to wait for.
pub fn inbox(sock: Target) -> i32 {
    let Some((pane, hook)) = stdin_hook() else { return 0 };
    loop {
        match request(&sock, "POST", &format!("/api/panes/{pane}/inbox"), Some(&hook)) {
            Ok(res) if res.status == 503 => {}
            Ok(res) if res.status >= 400 => return 0,
            Ok(res) => {
                let Ok(v) = res.json() else { return 0 };
                if v["action"] != "follow_up" {
                    return 0;
                }
                let name = v["by"]["name"].as_str().unwrap_or("a teammate");
                let text = v["text"].as_str().unwrap_or_default();
                let mut err = std::io::stderr().lock();
                let _ = writeln!(err, "A follow-up from {name} (sent through illogical): {text}");
                let _ = err.flush();
                return 2;
            }
            // The daemon is restarting: wait for the next one.
            Err(_) => {}
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}
