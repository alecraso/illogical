//! Rules over screens captured from Claude Code 2.1 and Codex 0.155
//! (`fixtures/screens/*.txt`: the title on the first line, then the screen;
//! paths and prompts replaced), and over whole sessions recorded with their
//! timing for the replay agent (`fixtures/agents/*.cast`). Each is drawn in
//! a real terminal first, so the rules see what the daemon's engine would.

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
        // The transcript view (Ctrl-O) hides the prompt box: only the
        // title says.
        ("claude_transcript", Idle),
        ("claude_transcript_working", Working),
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
    assert!(d.pending());
    // Working shows at once.
    assert_eq!(d.see(at(1100), Some(Working)), Some(Working));
    assert!(!d.pending());
    // A gap between spinner frames isn't idle...
    assert_eq!(d.see(at(1200), Some(Idle)), None);
    assert!(d.pending());
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

#[test]
fn what_it_drew_during_the_grace_period_is_still_read() {
    // A dialog drawn at once, then nothing more printed: it wants a look
    // after the grace period, whenever that comes.
    let t0 = Instant::now();
    let mut d = Debounce::new(t0);
    assert_eq!(d.see(t0 + Duration::from_millis(300), Some(AgentState::Blocked)), None);
    assert!(d.pending());
    assert_eq!(d.see(t0 + Duration::from_secs(5), Some(AgentState::Blocked)), Some(AgentState::Blocked));
    assert!(!d.pending());
}

/// A recording for the replay agent (`fixtures/agents/*.cast`): its size,
/// and its events (seconds, kind, text).
fn cast(name: &str) -> (u16, u16, Vec<(f64, String, String)>) {
    let path = format!("{}/fixtures/agents/{name}.cast", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let mut lines = text.lines();
    let header: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    let events = lines.filter(|l| !l.trim().is_empty()).map(|l| serde_json::from_str(l).unwrap()).collect();
    (header["width"].as_u64().unwrap() as u16, header["height"].as_u64().unwrap() as u16, events)
}

/// Plays a recording through a terminal and reads the screen at each of
/// its markers, which say what the screen showed when it was recorded.
fn markers(agent_id: &str, name: &str) -> Vec<(f64, AgentState, Option<AgentState>)> {
    let (cols, rows, events) = cast(name);
    let mut e = GhosttyEngine::new(cols, rows);
    let mut out = vec![];
    for (t, kind, text) in events {
        match kind.as_str() {
            "o" => e.feed(text.as_bytes()),
            "m" => {
                let want = match text.as_str() {
                    "working" => AgentState::Working,
                    "blocked" => AgentState::Blocked,
                    "idle" => AgentState::Idle,
                    other => panic!("{name}: marker {other}"),
                };
                let got = agent(agent_id).unwrap().detect(&e.title(), &e.screen_lines()).map(|d| d.state);
                out.push((t, want, got));
            }
            _ => {}
        }
    }
    out
}

#[test]
fn a_recorded_claude_code_session() {
    // Real Claude Code 2.1.289 (`record.py claude_turn`): the trust
    // dialog, a prompt, an approval, the turn, the transcript view while
    // idle and while working.
    let seen = markers("claude", "claude_turn");
    assert!(seen.len() >= 12, "{seen:?}");
    for (t, want, got) in &seen {
        assert_eq!(Some(*want), *got, "at {t}s: {seen:?}");
    }
}

#[test]
fn a_recorded_codex_session() {
    let seen = markers("codex", "codex_turn");
    assert!(seen.len() >= 6, "{seen:?}");
    for (t, want, got) in &seen {
        assert_eq!(Some(*want), *got, "at {t}s: {seen:?}");
    }
}

#[test]
fn explain_says_which_rule_saw_what() {
    let e = draw("claude_permission");
    let looks = agent("claude").unwrap().explain(&e.title(), &e.screen_lines());
    // Highest priority first; the first that matched is the one detect
    // answers with.
    assert!(looks.windows(2).all(|w| w[0].priority >= w[1].priority));
    let fired = looks.iter().find(|l| l.matched).unwrap();
    assert_eq!((fired.rule, fired.state), ("permission_prompt", AgentState::Blocked));
    assert_eq!(fired.region, "after the last rule");
    assert!(fired.text.iter().any(|l| l.contains("Do you want to proceed?")), "{fired:?}");
    let title = looks.iter().find(|l| l.rule == "title_idle").unwrap();
    assert_eq!(title.text, vec!["✳ Create a file".to_owned()]);
}
