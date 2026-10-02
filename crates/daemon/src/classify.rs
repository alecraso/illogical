//! What a pane is busy with, and which project it's in (M23): enough to
//! colour and group hundreds of panes without attaching to any.
//!
//! `kind` reads a command line: the foreground process's argv from the OS
//! first (it sees through aliases, so `c` reads as `claude`), else the text
//! the shell integration reported. S16 got 52 of 54 labelled commands right
//! this way. `project` is the nearest directory up from the working
//! directory with a `.git` in it, found without running git, and cached.

use std::{
    collections::HashMap,
    path::Path,
    sync::{LazyLock, Mutex},
};

use illogical_proto::{Project, WorkKind};
use regex::Regex;

const AGENTS: &[&str] = &["claude", "codex", "aider", "gemini", "opencode", "goose", "amp", "cursor-agent"];
const EDITORS: &[&str] = &["vim", "nvim", "vi", "hx", "helix", "emacs", "nano", "micro", "kak"];
/// Follow-mode log readers (with `-f`), or always.
const LOGS_FOLLOW: &[&str] = &["tail", "less"];
const LOGS: &[&str] = &["journalctl", "stern", "lnav"];
const LOGS_TWO: &[&str] = &["kubectl logs", "docker logs", "fly logs", "flyctl logs"];
/// Commands that run another: look past them (`uv run pytest`, `sudo vim`).
/// A shell with a script is the script (`bash ./bin/deploy test`, which is
/// what /proc shows for a script with a `#!/bin/bash` line).
const WRAPPERS: &[&str] = &[
    "sudo", "time", "nice", "env", "nohup", "uv", "npx", "pnpm", "npm", "yarn", "bunx", "exec", "node", "python",
    "python3", "bun", "deno", "bash", "sh", "zsh",
];

static SERVERS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\b(run dev|serve|server|uvicorn|gunicorn|rails s|runserver|vite$|vite --|vite dev|next dev|illogicald --foreground|caddy run|nginx|watch|cargo watch|air|npm start|pnpm dev|yarn dev|hugo server)\b",
    )
    .unwrap()
});
static TESTS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(test|tests|pytest|nextest|jest|vitest|playwright test|go test|rspec|bats|ctest)\b").unwrap()
});
static BUILDS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\b(build|make|ninja|cmake|clippy|check|tsc|webpack|esbuild|bazel|gradle|mvn|zig build|docker build|just build|cargo b)\b",
    )
    .unwrap()
});

fn base(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

/// What a command line is: an agent, an editor, logs, a server, tests, a
/// build, or anything else (`shell`, also for an empty line).
pub fn kind(command: &str) -> WorkKind {
    let words: Vec<&str> = command.split_whitespace().map(|w| w.trim_matches('\'')).collect();
    let Some(first) = words.first().map(|w| base(w)) else { return WorkKind::Shell };
    // Look through wrappers (`uv run`, `npx`, `sudo`).
    let mut i = 0;
    while i + 1 < words.len() && WRAPPERS.contains(&base(words[i])) {
        i += 1;
        if words[i] == "run" {
            i = (i + 1).min(words.len() - 1);
        }
    }
    let head = base(words[i]);
    let two = words[i..].iter().take(2).map(|w| base(w)).collect::<Vec<_>>().join(" ");
    let is_agent = |w: &str| AGENTS.iter().any(|a| w == *a || w.starts_with(&format!("{a}-")));
    if is_agent(head) || is_agent(first) {
        return WorkKind::Agent;
    }
    if EDITORS.contains(&head) {
        return WorkKind::Editor;
    }
    let follows = words.iter().any(|w| *w == "-f" || *w == "-F" || w.starts_with("-f") && w.len() <= 4);
    if LOGS_TWO.contains(&two.as_str()) || LOGS.contains(&head) || (LOGS_FOLLOW.contains(&head) && follows) {
        return WorkKind::Logs;
    }
    // The rest read the line without its paths' directories.
    let text = words.iter().map(|w| base(w)).collect::<Vec<_>>().join(" ");
    if SERVERS.is_match(&text) {
        WorkKind::Server
    } else if TESTS.is_match(&text) {
        WorkKind::Test
    } else if BUILDS.is_match(&text) {
        WorkKind::Build
    } else {
        WorkKind::Shell
    }
}

/// Directories already looked at, so a pane's project costs a lookup.
static PROJECTS: LazyLock<Mutex<HashMap<String, Option<Project>>>> = LazyLock::new(Default::default);

/// The git repository `cwd` is in: the nearest directory up with a `.git`
/// (a directory, or a worktree's file). Cached per directory.
pub fn project(cwd: &str) -> Option<Project> {
    if let Some(p) = PROJECTS.lock().unwrap().get(cwd) {
        return p.clone();
    }
    let found =
        Path::new(cwd).ancestors().take_while(|p| *p != Path::new("/")).find(|p| p.join(".git").exists()).map(|root| {
            Project {
                root: root.display().to_string(),
                name: root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            }
        });
    let mut cache = PROJECTS.lock().unwrap();
    // Bounded: a daemon that has seen this many directories starts over.
    if cache.len() > 4096 {
        cache.clear();
    }
    cache.insert(cwd.to_owned(), found.clone());
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use WorkKind::*;

    /// S16's labelled fixture, plus what panes run in this repo.
    #[test]
    fn kinds_of_real_commands() {
        let fixture = [
            ("cargo build --release", Build),
            ("cargo build -p illogical-control", Build),
            ("cargo clippy --all-targets -- -D warnings", Build),
            ("just build", Build),
            ("make -j32", Build),
            ("npm run build", Build),
            ("pnpm run build", Build),
            ("vite build", Build),
            ("tsc --noEmit", Build),
            ("docker build -t api .", Build),
            ("zig build", Build),
            ("cargo test", Test),
            ("cargo test -p illogicald --test questions", Test),
            ("cargo nextest run", Test),
            ("npx playwright test passkey", Test),
            ("pytest -x", Test),
            ("uv run pytest -k slots", Test),
            ("go test ./...", Test),
            ("pnpm vitest", Test),
            ("just test", Test),
            ("bats tests/", Test),
            ("claude", Agent),
            ("claude --continue", Agent),
            ("claude --dangerously-skip-permissions", Agent),
            ("codex", Agent),
            ("aider --model sonnet", Agent),
            ("/home/user/.local/bin/claude", Agent),
            // What /proc shows for `c`, an alias for it.
            ("claude --dangerously-skip-permissions", Agent),
            ("node /home/user/.local/share/agents/node_modules/.bin/claude-agent-acp", Agent),
            ("npm run dev", Server),
            ("pnpm dev", Server),
            ("npm start", Server),
            ("uvicorn app:main --reload", Server),
            ("python manage.py runserver", Server),
            ("illogicald --foreground", Server),
            ("hugo server", Server),
            ("cargo watch -x check", Server),
            ("caddy run", Server),
            ("node /work/app/node_modules/.bin/vite", Server),
            ("tail -f /var/log/caddy.log", Logs),
            ("tail -F app.log", Logs),
            ("journalctl -fu illogicald", Logs),
            ("journalctl -f", Logs),
            ("kubectl logs -f deploy/api", Logs),
            ("fly logs -a illogical-control", Logs),
            ("docker logs -f web", Logs),
            ("nvim src/main.rs", Editor),
            ("vim /tmp/x.txt", Editor),
            ("sudo vim /etc/hosts", Editor),
            ("hx .", Editor),
            ("emacs -nw", Editor),
            ("", Shell),
            ("git status", Shell),
            ("ls -la", Shell),
            ("ssh box", Shell),
            ("htop", Shell),
            ("git push origin main", Shell),
            ("sudo apt upgrade", Shell),
            ("python3", Shell),
            ("tail -n 20 app.log", Shell),
            // Scripts, as /proc shows them: their interpreter, then them.
            ("/bin/bash /home/user/bin/cargo test", Test),
            ("bash /opt/tools/journalctl -f", Logs),
            ("/bin/bash /home/user/.local/bin/claude perm", Agent),
            ("bash", Shell),
            ("bash -l", Shell),
            ("bash --norc --noprofile", Shell),
        ];
        let wrong: Vec<_> =
            fixture.iter().filter(|(c, want)| kind(c) != *want).map(|(c, want)| (c, want, kind(c))).collect();
        assert!(wrong.is_empty(), "misclassified: {wrong:?}");
    }

    /// The typed text hides an alias; the process's argv doesn't.
    #[test]
    fn argv_before_typed_text() {
        assert_eq!(kind("c"), Shell);
        assert_eq!(kind("claude"), Agent);
    }

    #[test]
    fn projects_are_git_roots() {
        let dir = std::env::temp_dir().join(format!("illogical-classify-{}", std::process::id()));
        let repo = dir.join("myrepo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("crates/core/src")).unwrap();
        std::fs::create_dir_all(dir.join("loose")).unwrap();
        let inner = repo.join("crates/core/src").display().to_string();
        let p = project(&inner).unwrap();
        assert_eq!((p.name.as_str(), p.root), ("myrepo", repo.display().to_string()));
        assert_eq!(project(&repo.display().to_string()).unwrap().name, "myrepo");
        assert_eq!(project(&dir.join("loose").display().to_string()), None);
        // A worktree's .git is a file.
        let wt = dir.join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), "gitdir: elsewhere").unwrap();
        assert_eq!(project(&wt.display().to_string()).unwrap().name, "wt");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
