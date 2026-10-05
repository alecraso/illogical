//! `illogical hooks`: put Claude Code's hooks for illogical into its
//! settings.json (`install`), or say which are there (`status`). The merge
//! only ever adds: every other key and every other hook stays as it is, and
//! a hook already there (same event, same matcher, same command) isn't
//! added twice, so a second `install` changes nothing.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};
use clap::Subcommand;
use serde_json::{Map, Value, json};

/// The hooks illogical wants in Claude Code's settings.json. This is the
/// JSON in docs/cli.md § "Claude Code in a pane"; a test fails if the two
/// differ, so change them together.
pub const HOOKS_SNIPPET: &str = r#"{
  "hooks": {
    "Notification": [{ "hooks": [{ "type": "command", "command": "illogical attention needs-input" }] }],
    "Stop": [
      { "hooks": [{ "type": "command", "command": "illogical attention done" }] },
      { "hooks": [{ "type": "command", "command": "illogical inbox", "asyncRewake": true, "timeout": 86400 }] }
    ],
    "SessionStart": [{ "hooks": [{ "type": "command", "command": "illogical inbox", "asyncRewake": true, "timeout": 86400 }] }],
    "PreToolUse": [
      { "matcher": "AskUserQuestion", "hooks": [{ "type": "command", "command": "illogical ask", "timeout": 604800 }] },
      { "hooks": [{ "type": "command", "command": "illogical hook" }] }
    ],
    "PermissionRequest": [{ "hooks": [{ "type": "command", "command": "illogical hook", "timeout": 604800 }] }],
    "PostToolUse": [{ "hooks": [{ "type": "command", "command": "illogical hook" }] }],
    "PostToolUseFailure": [{ "hooks": [{ "type": "command", "command": "illogical hook" }] }],
    "UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": "illogical hook" }] }]
  }
}"#;

#[derive(Subcommand)]
pub enum HooksCmd {
    /// Merge the hooks into Claude Code's settings.json, keeping everything
    /// already there. Running it again changes nothing.
    Install {
        /// Write DIR/.claude/settings.json instead of ~/.claude/settings.json.
        #[arg(long, value_name = "DIR")]
        project: Option<PathBuf>,
        /// Print the resulting JSON; write nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Which events have the illogical hook (exits 0 even if some don't).
    Status {
        /// Check DIR/.claude/settings.json instead of ~/.claude/settings.json.
        #[arg(long, value_name = "DIR")]
        project: Option<PathBuf>,
    },
}

pub fn run(cmd: HooksCmd, json_out: bool) -> anyhow::Result<i32> {
    match cmd {
        HooksCmd::Install { project, dry_run } => install(&settings_path(project)?, dry_run),
        HooksCmd::Status { project } => status(&settings_path(project)?, json_out),
    }
    .map(|()| 0)
}

fn settings_path(project: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    let base = match project {
        Some(dir) => dir,
        None => PathBuf::from(std::env::var_os("HOME").context("no HOME")?),
    };
    Ok(base.join(".claude/settings.json"))
}

fn snippet() -> Map<String, Value> {
    let v: Value = serde_json::from_str(HOOKS_SNIPPET).expect("HOOKS_SNIPPET is JSON");
    match v["hooks"].clone() {
        Value::Object(m) => m,
        _ => unreachable!("HOOKS_SNIPPET has a hooks object"),
    }
}

/// Whether `groups` (an event's array) has a group with this matcher and
/// one of this group's commands. A group with another matcher is another
/// entry: `AskUserQuestion`'s `illogical ask` is not the bare `illogical hook`.
fn has_group(groups: &[Value], want: &Value) -> bool {
    let commands = |g: &Value| -> Vec<String> {
        g["hooks"].as_array().into_iter().flatten().filter_map(|h| h["command"].as_str().map(str::to_owned)).collect()
    };
    let wanted = commands(want);
    groups.iter().any(|g| g.get("matcher") == want.get("matcher") && commands(g).iter().any(|c| wanted.contains(c)))
}

/// `settings` with the snippet's hooks added where missing; whether it changed.
fn merge(settings: &mut Value) -> anyhow::Result<bool> {
    let Some(root) = settings.as_object_mut() else {
        bail!("settings.json isn't a JSON object; leaving it alone");
    };
    let hooks = root.entry("hooks").or_insert_with(|| json!({}));
    let Some(hooks) = hooks.as_object_mut() else {
        bail!("`hooks` in settings.json isn't an object; leaving it alone");
    };
    let mut changed = false;
    for (event, wanted) in snippet() {
        let groups = hooks.entry(event.clone()).or_insert_with(|| json!([]));
        let Some(groups) = groups.as_array_mut() else {
            bail!("`hooks.{event}` in settings.json isn't an array; leaving it alone");
        };
        for want in wanted.as_array().into_iter().flatten() {
            if !has_group(groups, want) {
                groups.push(want.clone());
                changed = true;
            }
        }
    }
    Ok(changed)
}

fn read_settings(path: &Path) -> anyhow::Result<Value> {
    match fs::read_to_string(path) {
        Ok(s) if s.trim().is_empty() => Ok(json!({})),
        Ok(s) => {
            serde_json::from_str(&s).with_context(|| format!("{} isn't valid JSON; leaving it alone", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// A temp file beside `path`, then a rename, so Claude Code never reads half
/// a file. A symlinked settings.json (dotfiles) is written through, and the
/// mode of an existing file is kept.
fn write_atomic(path: &Path, text: &str) -> anyhow::Result<()> {
    let target = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let dir = target.parent().context("settings.json has no directory")?;
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let tmp = dir.join(format!(".settings.json.illogical-{}", std::process::id()));
    let result = (|| {
        fs::write(&tmp, text)?;
        if let Ok(meta) = fs::metadata(&target) {
            fs::set_permissions(&tmp, meta.permissions())?;
        }
        fs::rename(&tmp, &target)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.with_context(|| format!("writing {}", target.display()))
}

fn pretty(v: &Value) -> String {
    format!("{}\n", serde_json::to_string_pretty(v).unwrap_or_default())
}

fn install(path: &Path, dry_run: bool) -> anyhow::Result<()> {
    let mut settings = read_settings(path)?;
    let changed = merge(&mut settings)?;
    let text = pretty(&settings);
    if dry_run {
        print!("{text}");
        return Ok(());
    }
    if !changed && path.exists() {
        println!("{}: the hooks are already there", path.display());
        return Ok(());
    }
    write_atomic(path, &text)?;
    println!("{}: hooks added", path.display());
    Ok(())
}

/// Per event: whether every illogical entry for it is in `settings`.
fn present(settings: &Value) -> Vec<(String, bool)> {
    snippet()
        .into_iter()
        .map(|(event, wanted)| {
            let groups = settings["hooks"][&event].as_array().map(Vec::as_slice).unwrap_or_default();
            let all = wanted.as_array().into_iter().flatten().all(|w| has_group(groups, w));
            (event, all)
        })
        .collect()
}

fn status(path: &Path, json_out: bool) -> anyhow::Result<()> {
    let events = present(&read_settings(path)?);
    let installed = events.iter().all(|(_, p)| *p);
    if json_out {
        let events: Map<String, Value> = events.into_iter().map(|(e, p)| (e, Value::Bool(p))).collect();
        let v = json!({ "settings": path, "installed": installed, "events": events });
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
        return Ok(());
    }
    println!("{}", path.display());
    for (event, p) in &events {
        println!("  {event:<20} {}", if *p { "present" } else { "missing" });
    }
    if !installed {
        println!("`illogical hooks install` adds the missing ones.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh directory under the system temp dir.
    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("illogical-hooks-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// A settings file with a user's own hooks on PreToolUse and Stop, and
    /// keys that aren't hooks.
    const MINE: &str = r#"{
  "model": "opus",
  "permissions": { "allow": ["Bash(ls:*)"] },
  "hooks": {
    "PreToolUse": [{ "matcher": "Bash", "hooks": [{ "type": "command", "command": "my-lint" }] }],
    "Stop": [{ "hooks": [{ "type": "command", "command": "say done" }] }]
  }
}"#;

    fn merged(text: &str) -> (Value, bool) {
        let mut v: Value = serde_json::from_str(text).unwrap();
        let changed = merge(&mut v).unwrap();
        (v, changed)
    }

    #[test]
    fn merge_keeps_what_is_there() {
        let (v, changed) = merged(MINE);
        assert!(changed);
        assert_eq!(v["model"], "opus");
        assert_eq!(v["permissions"]["allow"][0], "Bash(ls:*)");
        let pre = v["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre[0]["hooks"][0]["command"], "my-lint");
        assert_eq!(pre.len(), 3);
        let stop = v["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop[0]["hooks"][0]["command"], "say done");
        assert_eq!(stop.len(), 3);
        assert!(present(&v).iter().all(|(_, p)| *p));
    }

    #[test]
    fn merge_twice_changes_nothing() {
        let (once, _) = merged(MINE);
        let (twice, changed) = merged(&serde_json::to_string(&once).unwrap());
        assert!(!changed);
        assert_eq!(once, twice);
        let (from_empty, _) = merged("{}");
        assert_eq!(from_empty["hooks"], Value::Object(snippet()));
    }

    #[test]
    fn ask_matcher_is_its_own_entry() {
        // A bare `illogical ask` (no matcher) isn't the AskUserQuestion one,
        // and the bare `illogical hook` isn't it either.
        let bare = r#"{"hooks":{"PreToolUse":[
            {"hooks":[{"type":"command","command":"illogical ask"}]},
            {"matcher":"Bash","hooks":[{"type":"command","command":"illogical hook"}]}]}}"#;
        let (v, changed) = merged(bare);
        assert!(changed);
        let pre = v["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 4);
        assert!(pre.iter().any(|g| g["matcher"] == "AskUserQuestion"));
        assert!(pre.iter().any(|g| g.get("matcher").is_none() && g["hooks"][0]["command"] == "illogical hook"));
        // Only the AskUserQuestion one present: the bare hook is still missing.
        let only_ask = r#"{"hooks":{"PreToolUse":[{"matcher":"AskUserQuestion","hooks":[{"type":"command","command":"illogical ask"}]}]}}"#;
        let (v, _) = merged(only_ask);
        assert_eq!(v["hooks"]["PreToolUse"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn odd_settings_are_refused_not_replaced() {
        for bad in [r#"[]"#, r#"{"hooks": []}"#, r#"{"hooks": {"Stop": {}}}"#] {
            let mut v: Value = serde_json::from_str(bad).unwrap();
            assert!(merge(&mut v).is_err(), "{bad}");
        }
    }

    #[test]
    fn install_writes_once_and_is_byte_identical_after() {
        let dir = temp("install");
        let path = dir.join(".claude/settings.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, MINE).unwrap();
        install(&path, false).unwrap();
        let first = fs::read(&path).unwrap();
        install(&path, false).unwrap();
        assert_eq!(first, fs::read(&path).unwrap());
        let v: Value = serde_json::from_slice(&first).unwrap();
        assert_eq!(v["hooks"]["Stop"][0]["hooks"][0]["command"], "say done");
        assert_eq!(v["hooks"]["PreToolUse"][0]["hooks"][0]["command"], "my-lint");
        // No temp file left beside it.
        let names: Vec<_> = fs::read_dir(path.parent().unwrap()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, ["settings.json"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_creates_the_file_and_dry_run_writes_nothing() {
        let dir = temp("create");
        let path = dir.join(".claude/settings.json");
        install(&path, true).unwrap();
        assert!(!path.exists());
        install(&path, false).unwrap();
        assert!(present(&read_settings(&path).unwrap()).iter().all(|(_, p)| *p));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_json_is_left_alone() {
        let dir = temp("invalid");
        let path = dir.join("settings.json");
        fs::write(&path, "{ not json").unwrap();
        assert!(install(&path, false).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "{ not json");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn status_per_event() {
        let (v, _) = merged(MINE);
        assert!(present(&v).iter().all(|(_, p)| *p));
        let none: Value = serde_json::from_str(MINE).unwrap();
        assert!(present(&none).iter().all(|(_, p)| !*p));
        // Half of Stop's entries is not enough for Stop.
        let half = r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"illogical attention done"}]}]}}"#;
        let p = present(&serde_json::from_str(half).unwrap());
        assert!(p.iter().any(|(e, ok)| e == "Stop" && !ok));
    }

    #[test]
    fn docs_show_the_snippet() {
        let docs = include_str!("../../../docs/cli.md");
        let at = docs.find("## Claude Code in a pane").expect("the section");
        let block = docs[at..].split("```json\n").nth(1).and_then(|s| s.split("\n```").next()).expect("a json block");
        assert_eq!(block, HOOKS_SNIPPET, "docs/cli.md and HOOKS_SNIPPET differ");
    }
}
