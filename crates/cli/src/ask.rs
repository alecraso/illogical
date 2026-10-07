//! `arugula ask`: Claude Code's `PreToolUse` hook on `AskUserQuestion`.
//!
//! It reads the hook's input on stdin, shows the questions as a card beside
//! the pane it runs in (on every client, with a push notification), waits
//! for the answer, and prints it for Claude Code: `allow`, with the answers
//! in the tool's input, so Claude Code never shows its own picker.
//!
//! Anything else ends it with no output, which leaves Claude Code to show
//! its picker as usual: "Answer in terminal" on the card, running outside an
//! Arugula pane, input that isn't AskUserQuestion's, or a daemon that's
//! gone for good. When Claude Code gives up on it (Esc or Ctrl-C in the TUI,
//! or the hook's timeout) it gets SIGTERM, and withdraws the card first.
//! A daemon restart while it waits only means asking the new one.

use std::{
    io::{Read, Write},
    time::{Duration, Instant},
};

#[cfg(unix)]
use nix::sys::signal::{SigSet, Signal};
use serde_json::{Value, json};

use crate::http::{Target, request};

/// How long to keep asking a daemon that doesn't answer before leaving the
/// question to the terminal.
const GIVE_UP: Duration = Duration::from_secs(60);

/// Runs the hook; always exits 0 (any other code would be Claude Code's
/// "error" or "block"). What it prints is the answer, or nothing.
pub fn run(sock: Target) -> i32 {
    // Read all of stdin whatever happens, so Claude Code never sees a broken
    // pipe.
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let Some(pane) = crate::util::env_pane() else { return 0 };
    let Ok(hook) = serde_json::from_str::<Value>(&input) else { return 0 };
    if hook["tool_name"].as_str().is_some_and(|t| t != "AskUserQuestion") {
        return 0;
    }
    let questions = hook["tool_input"]["questions"].clone();
    if !questions.as_array().is_some_and(|q| !q.is_empty()) {
        return 0;
    }
    let id = hook["tool_use_id"].as_str().map(str::to_owned);
    withdraw_on_signals(sock.clone(), pane, id.clone());
    if let Some(output) = wait_for_answer(&sock, pane, &questions, id) {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{output}");
        let _ = out.flush();
    }
    0
}

/// The hook's output, or `None` to leave the question to the terminal.
fn wait_for_answer(sock: &Target, pane: u32, questions: &Value, id: Option<String>) -> Option<Value> {
    let body = json!({ "questions": questions, "id": id });
    let mut failing_since: Option<Instant> = None;
    loop {
        let answer = request(sock, "POST", &format!("/api/panes/{pane}/ask"), Some(&body));
        match answer {
            Ok(res) if res.status == 503 => {}
            Ok(res) => {
                let v = res.json().ok()?;
                return match v["action"].as_str() {
                    Some("accept" | "decline") => Some(v["output"].clone()),
                    _ => None,
                };
            }
            Err(_) => {}
        }
        // The daemon is restarting (or gone): ask again shortly.
        let since = *failing_since.get_or_insert_with(Instant::now);
        if since.elapsed() > GIVE_UP {
            eprintln!("arugula ask: the daemon isn't answering; answer in the terminal");
            return None;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// On SIGTERM (Claude Code interrupting the hook), SIGINT or SIGHUP:
/// withdraw the card, then exit quietly.
#[cfg(unix)]
pub fn withdraw_on_signals(sock: Target, pane: u32, id: Option<String>) {
    let mut set = SigSet::empty();
    for s in [Signal::SIGTERM, Signal::SIGINT, Signal::SIGHUP] {
        set.add(s);
    }
    // Blocked here, and so in every thread started after: only the waiting
    // thread takes them.
    if set.thread_block().is_err() {
        return;
    }
    std::thread::spawn(move || {
        if set.wait().is_ok() {
            let _ = request(&sock, "POST", &format!("/api/panes/{pane}/ask/withdraw"), Some(&json!({ "id": id })));
            std::process::exit(0);
        }
    });
}

/// Windows: on Ctrl-C, Ctrl-Break or the console closing, withdraw the
/// card, then exit quietly. (A hook ended with TerminateProcess gets no say:
/// the card stays until the question times out.)
#[cfg(windows)]
pub fn withdraw_on_signals(sock: Target, pane: u32, id: Option<String>) {
    use std::sync::OnceLock;
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
    static CARD: OnceLock<(Target, u32, Option<String>)> = OnceLock::new();
    if CARD.set((sock, pane, id)).is_err() {
        return;
    }
    unsafe extern "system" fn on_ctrl(_event: u32) -> windows_sys::core::BOOL {
        if let Some((sock, pane, id)) = CARD.get() {
            let _ = request(sock, "POST", &format!("/api/panes/{pane}/ask/withdraw"), Some(&json!({ "id": id })));
        }
        std::process::exit(0);
    }
    // SAFETY: a handler that only makes a request and exits.
    unsafe { SetConsoleCtrlHandler(Some(on_ctrl), 1) };
}
