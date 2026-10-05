//! Resuming a pane's agent conversation after a reboot (#146).
//!
//! A pane running Claude Code knows which conversation it holds, from its
//! hooks (`session_id` and `transcript_path` in every hook's input) or,
//! without them, from Claude Code's session files under its pid. When the
//! daemon starts again, a pane whose policy is [`Policy::Resume`] (the
//! default for a pane running Claude Code) and that was running its agent
//! runs `claude --resume <id>`, with the id as its own argument: it never
//! reaches a shell as text. If the transcript or the directory is gone, the
//! pane is a shell that says why.

use std::path::Path;

use illogical_proto::Policy;

use crate::store::{AgentSession, PaneMeta};

/// A session id an agent would give: a UUID, or near it. Anything else is
/// refused rather than passed on.
pub fn valid_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && !id.starts_with('-')
        && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// What people call the agent.
pub fn name(agent: &str) -> &str {
    match agent {
        "claude" => "Claude Code",
        "codex" => "Codex",
        other => other,
    }
}

/// The command that resumes `s`, as argv.
fn argv(s: &AgentSession) -> Option<Vec<String>> {
    let (program, flag) = match s.agent.as_str() {
        "claude" => ("claude", "--resume"),
        "codex" => ("codex", "resume"),
        _ => return None,
    };
    Some(vec![program.into(), flag.into(), s.id.clone()])
}

/// Whether a restart of this pane resumes its conversation: its policy says
/// so (or is the default), and the agent was running in it.
pub fn applies(meta: &PaneMeta) -> Option<&AgentSession> {
    let s = meta.session.as_ref()?;
    let wants = match meta.policy {
        Policy::Resume => true,
        Policy::Shell => !meta.policy_set,
        _ => false,
    };
    (wants && s.running && meta.host.is_none()).then_some(s)
}

/// Where a restart resumes it: where the agent ran, else the pane's
/// directory.
pub fn dir(meta: &PaneMeta) -> Option<&str> {
    meta.session.as_ref().and_then(|s| s.cwd.as_deref()).or(meta.cwd.as_deref())
}

/// What a restart of this pane runs: `Ok(argv)` to resume its
/// conversation, `Err(why)` for a shell that says why it couldn't, `None`
/// when it doesn't resume one. `transcript` finds a Claude Code
/// conversation's transcript by id when its hooks didn't say where.
pub fn plan(meta: &PaneMeta, transcript: impl Fn(&str) -> Option<String>) -> Option<Result<Vec<String>, String>> {
    let cwd = dir(meta);
    let s = applies(meta)?;
    let what = format!("{} conversation {}", name(&s.agent), s.id);
    if !valid_id(&s.id) {
        return Some(Err(format!("not resuming {}: its session id isn't one", name(&s.agent))));
    }
    if let Some(cwd) = cwd
        && !Path::new(cwd).is_dir()
    {
        return Some(Err(format!("can't resume {what}: {cwd} is gone")));
    }
    if s.agent == "claude" {
        let path = s.transcript.clone().filter(|t| Path::new(t).is_file()).or_else(|| transcript(&s.id));
        if !path.is_some_and(|p| Path::new(&p).is_file()) {
            return Some(Err(format!("can't resume {what}: its transcript is gone")));
        }
    }
    argv(s).map(Ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(policy: Policy, set: bool, command: &str, id: &str, transcript: Option<&str>) -> PaneMeta {
        PaneMeta {
            policy,
            policy_set: set,
            cwd: Some("/".into()),
            session: Some(AgentSession {
                agent: "claude".into(),
                id: id.into(),
                transcript: transcript.map(str::to_owned),
                cwd: None,
                running: crate::classify::agent(command) == Some("claude"),
            }),
            ..Default::default()
        }
    }

    #[test]
    fn ids() {
        assert!(valid_id("0f3c2a9e-1b7d-4c8e-9f00-123456789abc"));
        for bad in ["", "-rf", "a b", "x;touch pwned", "$(id)", "`id`", "a'b", "a\nb", &"x".repeat(129)] {
            assert!(!valid_id(bad), "{bad:?}");
        }
    }

    #[test]
    fn when_it_resumes() {
        let t = std::env::temp_dir().join(format!("resume-{}.jsonl", std::process::id()));
        std::fs::write(&t, "{}").unwrap();
        let t = t.to_str().unwrap();
        let id = "0f3c2a9e-1b7d-4c8e-9f00-123456789abc";
        let none = |_: &str| None;
        let want = Some(Ok(vec!["claude".to_owned(), "--resume".into(), id.into()]));
        // The default, and asked for.
        assert_eq!(plan(&meta(Policy::Shell, false, "claude", id, Some(t)), none), want);
        assert_eq!(plan(&meta(Policy::Resume, true, "/usr/bin/claude --model haiku", id, Some(t)), none), want);
        // Someone picked a shell; the agent wasn't running; another policy.
        assert_eq!(plan(&meta(Policy::Shell, true, "claude", id, Some(t)), none), None);
        assert_eq!(plan(&meta(Policy::Resume, true, "vim", id, Some(t)), none), None);
        assert_eq!(plan(&meta(Policy::Rerun { confirm: false }, true, "claude", id, Some(t)), none), None);
        // Found through the index when its hooks didn't say.
        let found = |_: &str| Some(t.to_owned());
        assert_eq!(plan(&meta(Policy::Resume, true, "claude", id, None), found), want);
        // Gone: a shell that says so.
        let gone = plan(&meta(Policy::Resume, true, "claude", id, Some("/nonexistent.jsonl")), none);
        assert_eq!(gone, Some(Err(format!("can't resume Claude Code conversation {id}: its transcript is gone"))));
        let mut m = meta(Policy::Resume, true, "claude", id, Some(t));
        m.cwd = Some("/nonexistent-dir".into());
        assert!(plan(&m, none).unwrap().unwrap_err().contains("/nonexistent-dir is gone"));
        // An id that isn't one (a layout.json edited by hand) never runs.
        let bad = plan(&meta(Policy::Resume, true, "claude", "x'; touch pwned; '", Some(t)), none);
        assert_eq!(bad, Some(Err("not resuming Claude Code: its session id isn't one".into())));
        std::fs::remove_file(t).unwrap();
    }
}
