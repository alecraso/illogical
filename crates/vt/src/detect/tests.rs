//! Rules over screens captured from Claude Code 2.1 and Codex 0.155
//! (`fixtures/screens/*.txt`: the title on the first line, then the screen;
//! paths and prompts replaced). Each is drawn in a real terminal first, so
//! the rules see what the daemon's engine would.

use std::time::{Duration, Instant};

use super::{AgentState, Debounce, agent};
use crate::{GhosttyEngine, VtEngine};

fn draw(name: &str) -> GhosttyEngine {
    let mut e = GhosttyEngine::new(100, 30);
    feed(&mut e, name);
    e
}

fn feed(e: &mut GhosttyEngine, name: &str) {
    let path = format!("{}/fixtures/screens/{name}.txt", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let (title, screen) = text.split_once('\n').unwrap();
    let title = title.strip_prefix("title: ").unwrap();
    e.feed(format!("\x1b]2;{title}\x07\x1b[2J\x1b[H").as_bytes());
    e.feed(screen.trim_end_matches('\n').replace('\n', "\r\n").as_bytes());
}

fn state(agent_id: &str, e: &GhosttyEngine) -> Option<(AgentState, &'static str, Option<String>)> {
    let d = agent(agent_id).unwrap().detect(&e.title(), &e.screen_lines())?;
    Some((d.state, d.rule, d.headline))
}

#[test]
fn claude_code_screens() {
    use AgentState::*;
    for (name, want) in [
        ("claude_idle", Idle),
        ("claude_working", Working),
        ("claude_working_title", Working),
        ("claude_done", Idle),
        ("claude_permission", Blocked),
        ("claude_trust", Blocked),
        ("claude_old_question_in_history", Idle),
    ] {
        let got = state("claude", &draw(name));
        assert_eq!(got.as_ref().map(|g| g.0), Some(want), "{name}: {got:?}");
    }
    let (_, rule, headline) = state("claude", &draw("claude_permission")).unwrap();
    assert_eq!(rule, "permission_prompt");
    assert_eq!(headline.as_deref(), Some("Claude Code asks to run `touch made-by-claude.txt`"));
    let (_, _, headline) = state("claude", &draw("claude_trust")).unwrap();
    assert_eq!(headline.as_deref(), Some("Claude Code asks whether to trust this folder"));
}

#[test]
fn codex_screens() {
    use AgentState::*;
    for (name, want) in [
        ("codex_idle", Idle),
        ("codex_working", Working),
        ("codex_working_status", Working),
        ("codex_done", Idle),
        ("codex_approval", Blocked),
        ("codex_trust", Blocked),
    ] {
        let got = state("codex", &draw(name));
        assert_eq!(got.as_ref().map(|g| g.0), Some(want), "{name}: {got:?}");
    }
    let (_, _, headline) = state("codex", &draw("codex_approval")).unwrap();
    assert_eq!(headline.as_deref(), Some("Codex asks to run `curl -sI https://example.com`"));
}

#[test]
fn a_blank_screen_says_nothing() {
    let e = GhosttyEngine::new(100, 30);
    assert_eq!(state("claude", &e), None);
    assert_eq!(state("codex", &e), None);
}

#[test]
fn only_the_live_bottom_counts() {
    // A permission prompt scrolled off into history, an idle prompt now.
    let mut e = draw("claude_permission");
    e.feed(&b"\r\n".repeat(40));
    feed(&mut e, "claude_idle");
    assert!(e.history_lines() > 0);
    assert_eq!(state("claude", &e).map(|s| s.0), Some(AgentState::Idle));
}

#[test]
fn agents_by_program() {
    assert_eq!(agent("claude").map(|a| a.id), Some("claude"));
    assert_eq!(agent("/home/me/.local/bin/codex").map(|a| a.id), Some("codex"));
    assert!(agent("aider").is_none());
}

#[test]
fn debounce_waits_out_startup_and_spinner_gaps() {
    use AgentState::*;
    let t0 = Instant::now();
    let at = |ms| t0 + Duration::from_millis(ms);
    let mut d = Debounce::new(t0);
    // Its banner, drawn in pieces: nothing yet.
    assert_eq!(d.see(at(100), Some(Idle)), None);
    assert!(d.pending(at(100)));
    // Working shows at once.
    assert_eq!(d.see(at(1100), Some(Working)), Some(Working));
    assert!(!d.pending(at(1100)));
    // A gap between spinner frames isn't idle...
    assert_eq!(d.see(at(1200), Some(Idle)), None);
    assert!(d.pending(at(1200)));
    assert_eq!(d.see(at(1250), Some(Working)), None);
    // ...idle held over three looks 100 ms apart is.
    assert_eq!(d.see(at(2000), Some(Idle)), None);
    assert_eq!(d.see(at(2050), Some(Idle)), None);
    assert_eq!(d.see(at(2100), Some(Idle)), None);
    assert_eq!(d.see(at(2200), Some(Idle)), Some(Idle));
    // Blocked shows at once; can't-tell changes nothing.
    assert_eq!(d.see(at(3000), Some(Blocked)), Some(Blocked));
    assert_eq!(d.see(at(3100), None), None);
    assert_eq!(d.shown(), Some(Blocked));
    // Looked at rarely, idle held past the cap settles it.
    assert_eq!(d.see(at(4000), Some(Idle)), None);
    assert_eq!(d.see(at(4800), Some(Idle)), Some(Idle));
}
