//! Standing permission rules (#166): an agent block's "Always" for a
//! directory or for every block, kept by the daemon rather than the block.
//!
//! They live in `rules.json` in the daemon's state directory: this machine's
//! only, not synced, and not Claude Code's settings. Every agent block on
//! this daemon checks them, as they are now, beside its own rules, so a rule
//! made in one block answers the next block's request and forgetting one
//! takes effect at once.
//!
//! A rule allows a tool (`Bash`, `Edit`...) outright, or only the requests
//! whose title (for `Bash`, the command) starts with a prefix, word for
//! word. A prefix never allows a title with a shell separator,
//! substitution or redirect in it, so `git status` doesn't allow
//! `git status; rm -rf ~`.

use std::{
    io,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};

use crate::store::{now_ms, write_atomic};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Standing {
    pub tool: String,
    /// Only titles starting with this (word for word); the whole tool
    /// without it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    /// Blocks working in this directory or under it; every block without
    /// it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// The VM (sprite) the directory is on, for a `cwd` rule made by an
    /// agent there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sprite: Option<String>,
    /// When it was made, and the request that made it.
    #[serde(default)]
    pub at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
}

impl Standing {
    fn same(&self, o: &Standing) -> bool {
        (&self.tool, &self.prefix, &self.cwd, &self.sprite) == (&o.tool, &o.prefix, &o.cwd, &o.sprite)
    }

    /// Whether it answers a request for `tool` titled `title`, from a block
    /// working in `cwd` (on `sprite`).
    pub fn allows(&self, tool: &str, title: &str, cwd: Option<&str>, sprite: Option<&str>) -> bool {
        if self.tool != tool {
            return false;
        }
        if let Some(dir) = &self.cwd {
            let Some(cwd) = cwd else { return false };
            if self.sprite.as_deref() != sprite || !under(cwd, dir) {
                return false;
            }
        }
        match &self.prefix {
            None => true,
            Some(p) => prefixed(title, p),
        }
    }

    /// How it reads in a list: `Bash git status… in ~/src/x`.
    pub fn describe(&self) -> String {
        let what = match &self.prefix {
            Some(p) => format!("{} {p}…", self.tool),
            None => format!("{} (any)", self.tool),
        };
        match (&self.cwd, &self.sprite) {
            (Some(d), Some(s)) => format!("{what} in {d} on {s}"),
            (Some(d), None) => format!("{what} in {d}"),
            _ => format!("{what} everywhere"),
        }
    }
}

/// `path` is `dir` or under it.
fn under(path: &str, dir: &str) -> bool {
    let dir = dir.trim_end_matches('/');
    let path = path.trim_end_matches('/');
    dir.is_empty() || path == dir || path.strip_prefix(dir).is_some_and(|rest| rest.starts_with('/'))
}

/// `title` starts with `prefix`, ending at a word, and runs nothing else.
fn prefixed(title: &str, prefix: &str) -> bool {
    let prefix = prefix.trim();
    if prefix.is_empty() || title.contains(['\n', ';', '&', '|', '`', '<', '>']) || title.contains("$(") {
        return false;
    }
    let title = title.trim();
    title == prefix || title.strip_prefix(prefix).is_some_and(|rest| rest.starts_with(char::is_whitespace))
}

/// The daemon's standing rules.
pub struct Rules {
    path: PathBuf,
    list: Mutex<Vec<Standing>>,
}

impl Rules {
    /// The rules in `path`; none if it isn't there (or can't be read).
    pub fn open(path: PathBuf) -> Arc<Self> {
        let list = std::fs::read(&path)
            .ok()
            .and_then(|b| match serde_json::from_slice(&b) {
                Ok(l) => Some(l),
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "standing rules unreadable; starting with none");
                    None
                }
            })
            .unwrap_or_default();
        Arc::new(Self { path, list: Mutex::new(list) })
    }

    pub fn list(&self) -> Vec<Standing> {
        self.list.lock().unwrap().clone()
    }

    fn save(&self, list: &[Standing]) -> io::Result<()> {
        write_atomic(&self.path, &serde_json::to_vec_pretty(list).unwrap_or_default())
    }

    /// Keep `rule` (once).
    pub fn add(&self, mut rule: Standing) -> io::Result<()> {
        let mut g = self.list.lock().unwrap();
        if g.iter().any(|r| r.same(&rule)) {
            return Ok(());
        }
        if rule.at_ms == 0 {
            rule.at_ms = now_ms();
        }
        g.push(rule);
        self.save(&g)
    }

    /// Forget the rule at `index`, or all of them.
    pub fn forget(&self, index: Option<usize>) -> Result<(), String> {
        let mut g = self.list.lock().unwrap();
        match index {
            Some(i) if i < g.len() => _ = g.remove(i),
            Some(_) => return Err("no such rule".into()),
            None => g.clear(),
        }
        self.save(&g).map_err(|e| e.to_string())
    }

    /// The first rule that answers this request.
    pub fn allowing(&self, tool: &str, title: &str, cwd: Option<&str>, sprite: Option<&str>) -> Option<Standing> {
        self.list.lock().unwrap().iter().find(|r| r.allows(tool, title, cwd, sprite)).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(tool: &str, prefix: Option<&str>, cwd: Option<&str>) -> Standing {
        Standing {
            tool: tool.into(),
            prefix: prefix.map(Into::into),
            cwd: cwd.map(Into::into),
            sprite: None,
            at_ms: 1,
            from: None,
        }
    }

    #[test]
    fn a_directory_rule_holds_under_it_only() {
        let r = rule("Bash", None, Some("/src/x"));
        assert!(r.allows("Bash", "make", Some("/src/x"), None));
        assert!(r.allows("Bash", "make", Some("/src/x/crates/a"), None));
        assert!(!r.allows("Bash", "make", Some("/src/xy"), None));
        assert!(!r.allows("Bash", "make", Some("/src"), None));
        assert!(!r.allows("Bash", "make", None, None));
        assert!(!r.allows("Edit", "make", Some("/src/x"), None));
        assert!(!r.allows("Bash", "make", Some("/src/x"), Some("vm-1")), "the same path on a VM is another place");
        assert!(rule("Bash", None, None).allows("Bash", "anything", None, Some("vm-1")));
    }

    #[test]
    fn a_prefix_matches_whole_words_and_one_command() {
        let r = rule("Bash", Some("git status"), None);
        assert!(r.allows("Bash", "git status", None, None));
        assert!(r.allows("Bash", "git status --short", None, None));
        assert!(!r.allows("Bash", "git statusx", None, None));
        assert!(!r.allows("Bash", "git stash", None, None));
        for sneaky in [
            "git status; rm -rf ~",
            "git status && curl x",
            "git status | sh",
            "git status > f",
            "git status $(id)",
            "git status `id`",
            "git status\nrm x",
        ] {
            assert!(!r.allows("Bash", sneaky, None, None), "{sneaky}");
        }
    }

    #[test]
    fn rules_are_kept_once_and_forgotten() {
        let dir = std::env::temp_dir().join(format!("illogical-rules-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rules.json");
        let _ = std::fs::remove_file(&path);
        let rules = Rules::open(path.clone());
        rules.add(rule("Bash", Some("make"), Some("/src/x"))).unwrap();
        rules.add(rule("Bash", Some("make"), Some("/src/x"))).unwrap();
        rules.add(rule("Edit", None, None)).unwrap();
        assert_eq!(Rules::open(path.clone()).list().len(), 2, "kept on disk, once");
        assert_eq!(rules.allowing("Edit", "a.rs", Some("/elsewhere"), None).map(|r| r.tool), Some("Edit".into()));
        rules.forget(Some(1)).unwrap();
        assert!(rules.allowing("Edit", "a.rs", None, None).is_none());
        assert!(rules.forget(Some(5)).is_err());
        rules.forget(None).unwrap();
        assert!(Rules::open(path).list().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
