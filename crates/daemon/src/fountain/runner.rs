//! M45b: this host as the account's Fountain runner, as a Fountain block's
//! `view: runner` sees it, and the machine's line in `GET /api/host`.
//!
//! **This host's runner** is the one named by the `fountain-runner` systemd
//! unit's `--name` (M45a's `scripts/fountain-runner-setup.sh` writes it);
//! its sandboxes live under the unit's `--root`. A host without the unit
//! has no runner of its own: its view lists the account's runners and
//! raises nothing.
//!
//! **Attention** (`failed`, bundle `failed:fountain-runner`), judged by
//! [`judge`]:
//! - the unit is active (`systemctl is-active`) but Fountain has said this
//!   runner is offline (or doesn't list it) for [`grace`] (5 minutes);
//! - another runner on the account is online: Fountain puts a runner
//!   conversation on the most recently connected one, so it would win
//!   placement.
//!
//! **Sandboxes** are read as the `fountain` user, never through group
//! permissions (arugulad may not have the group, and the runner's own
//! `chmod`s may defeat it): every read goes through
//! `sudo -n -u fountain /bin/bash -c SCRIPT _ ARGS…`, the one command
//! M45a's sudoers rule allows (as `fountain`, only bash: `env …` or `-i`
//! are refused). `ARUGULA_FOUNTAIN_SUDO` names a stand-in `sudo` for tests,
//! `ARUGULA_FOUNTAIN_UNIT_FILE` another unit file, and
//! `ARUGULA_FOUNTAIN_SYSTEMCTL` another `systemctl`; no test runs sudo.

use std::{
    sync::{LazyLock, Mutex},
    time::Duration,
};

use arugula_proto::hosts::FountainRunnerInfo;
use serde::Serialize;

use super::api::{Runner, Sandbox, SandboxConversation};
use crate::store::now_ms;

/// The unit M45a's setup writes.
pub const UNIT: &str = "fountain-runner";
/// The runner's own user: the only one a diff block may run git as.
pub const USER: &str = "fountain";
const UNIT_FILE: &str = "/etc/systemd/system/fountain-runner.service";
/// How long Fountain must have said "offline" before it's news.
const GRACE: Duration = Duration::from_secs(300);
/// How often a drawn runner view reads `/api/runners`.
const POLL: Duration = Duration::from_secs(60);
/// The bundle every runner reason goes in (one card on the rail).
pub const BUNDLE: &str = "failed:fountain-runner";
/// What the view says about a shell it opens.
pub const PARK_NOTE: &str = "A shell opened here runs as fountain in the sandbox's directory, outside the runner's sandboxing: the fountain-runner unit's protections (hidden processes, no access to your home) don't apply to it. It isn't Fountain's, so parking the sandbox doesn't stop it.";
/// The sandbox states that may have a directory on the runner.
pub const LIVE_STATES: &str = "pending,starting,ready,suspended";

fn env_ms(k: &str) -> Option<Duration> {
    std::env::var(k).ok().and_then(|v| v.trim().parse().ok()).map(Duration::from_millis)
}

/// How long "offline" is tolerated (`ARUGULA_FOUNTAIN_RUNNER_GRACE_MS`
/// for tests).
pub fn grace() -> Duration {
    env_ms("ARUGULA_FOUNTAIN_RUNNER_GRACE_MS").unwrap_or(GRACE)
}

/// How often a drawn view reads (`ARUGULA_FOUNTAIN_POLL_MS`, as the
/// catalog, for tests).
pub fn interval() -> Duration {
    env_ms("ARUGULA_FOUNTAIN_POLL_MS").unwrap_or(POLL)
}

/// How often one nobody draws reads, so its attention still fires.
pub fn idle_interval() -> Duration {
    interval() * 5
}

// ---------------------------------------------------------------- the unit

/// What the unit file says.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Unit {
    /// `--name`: the runner's name on Fountain.
    pub name: String,
    /// `--root`: where its sandboxes are.
    pub root: Option<String>,
    /// The `fountain` it runs.
    pub bin: Option<String>,
}

/// The `ExecStart=` of a `fountain runner` unit.
pub fn parse_unit(text: &str) -> Option<Unit> {
    let exec = text.lines().map(str::trim).find_map(|l| l.strip_prefix("ExecStart="))?;
    let words: Vec<&str> = exec.trim_start_matches(['-', '@', '+', '!', ':']).split_whitespace().collect();
    let at = words.iter().position(|w| *w == "runner")?;
    let flag = |name: &str| -> Option<String> {
        let mut it = words[at + 1..].iter();
        while let Some(w) = it.next() {
            if *w == name {
                return it.next().map(|v| v.trim_matches('"').to_owned());
            }
            if let Some(v) = w.strip_prefix(name).and_then(|v| v.strip_prefix('=')) {
                return Some(v.trim_matches('"').to_owned());
            }
        }
        None
    };
    Some(Unit {
        name: flag("--name").filter(|n| !n.is_empty())?,
        root: flag("--root").filter(|r| r.starts_with('/')),
        bin: words.first().filter(|b| b.starts_with('/')).map(|b| (*b).to_owned()),
    })
}

/// This host's unit, if it has one.
pub fn unit() -> Option<Unit> {
    let path = std::env::var("ARUGULA_FOUNTAIN_UNIT_FILE").ok().filter(|p| !p.is_empty());
    parse_unit(&std::fs::read_to_string(path.as_deref().unwrap_or(UNIT_FILE)).ok()?)
}

/// `systemctl is-active fountain-runner` (no sudo needed): `None` where
/// there's no systemctl.
pub async fn unit_active() -> Option<bool> {
    let bin = std::env::var("ARUGULA_FOUNTAIN_SYSTEMCTL").ok().filter(|b| !b.is_empty());
    let out = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new(bin.as_deref().unwrap_or("systemctl"))
            .args(["is-active", UNIT])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim() == "active")
}

/// The installed `fountain`'s version (`fountain version v0.21.0` → `v0.21.0`).
pub fn parse_version(out: &str) -> Option<String> {
    let line = out.lines().map(str::trim).find(|l| !l.is_empty())?;
    let v = line.rsplit(' ').next()?.trim();
    (!v.is_empty()).then(|| v.to_owned())
}

// ---------------------------------------------------------------- sudo

fn sudo_bin() -> String {
    std::env::var("ARUGULA_FOUNTAIN_SUDO").ok().filter(|b| !b.is_empty()).unwrap_or_else(|| "sudo".into())
}

/// `sudo -n -u fountain /bin/bash -c SCRIPT _ ARGS…`: the only form the
/// sudoers rule allows. `-n`: never a password prompt.
pub fn sudo_argv(script: &str, args: &[String]) -> Vec<String> {
    let mut v: Vec<String> =
        [sudo_bin().as_str(), "-n", "-u", USER, "/bin/bash", "-c", script, "_"].map(str::to_owned).to_vec();
    v.extend(args.iter().cloned());
    v
}

/// Run `script` as `fountain` with `args` as `$1…`: its stdout and exit
/// code. sudo's own refusal (no rule, a password wanted) is the error.
pub async fn sudo_sh(script: &str, args: &[String]) -> Result<(Vec<u8>, Option<i32>), String> {
    let argv = sudo_argv(script, args);
    let run = tokio::process::Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(Duration::from_secs(30), run)
        .await
        .map_err(|_| "sudo as fountain took too long".to_owned())?
        .map_err(|e| format!("can't run {}: {e}", argv[0]))?;
    let err = String::from_utf8_lossy(&out.stderr);
    if out.stdout.is_empty() && err.contains("sudo") && !out.status.success() {
        let line = err.lines().find(|l| !l.trim().is_empty()).unwrap_or_default().trim();
        return Err(format!(
            "can't run bash as {USER} ({line}): is M45a's sudoers rule in place? (scripts/fountain-runner-setup.sh)"
        ));
    }
    Ok((out.stdout, out.status.code()))
}

/// `s` as one shell word: as it is when it's plain, else single-quoted.
pub fn quote(s: &str) -> String {
    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"_./-:=@%+,".contains(&b)) {
        return s.to_owned();
    }
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// The runner user's home: a shell's `HOME` (never the sandbox's, whose
/// dotfiles its agent writes).
pub const FOUNTAIN_HOME: &str = "/home/fountain";

/// *Shell*: bash as `fountain` in a sandbox, `HOME` the runner user's own,
/// reading no profile, rc, inputrc or history file (the sandbox's agent
/// could have written them). `$1` is
/// the unit's root and `$2` the directory, never part of the script; its
/// real path must be inside the root. It runs outside the runner's unit, so
/// none of the unit's protections apply to it.
pub fn shell_command(root: &str, dir: &str) -> String {
    format!(
        "exec {} -n -u {USER} /bin/bash -c {} _ {} {}",
        quote(&sudo_bin()),
        quote(&format!(
            "{INSIDE}\ncd -- \"$real\" && HOME={FOUNTAIN_HOME} INPUTRC=/dev/null HISTFILE=/dev/null exec bash --noprofile --norc"
        )),
        quote(root),
        quote(dir)
    )
}

/// `$1` the root, `$2` a directory: `$real` is the directory's real path,
/// or it says why not and exits (it must be inside the root, not the root).
/// Both sides are canonical (`cd -P`, `pwd -P`: macOS's `realpath` has no
/// `-e`, and its `/var` is `/private/var`).
const INSIDE: &str = r#"real=$(cd -P -- "$2" 2>/dev/null && pwd -P) && top=$(cd -P -- "$1" 2>/dev/null && pwd -P) && case $real/ in "$top"/?*/) true ;; *) false ;; esac || { printf 'err not inside the runner'\''s sandboxes (%s): %s\n' "$1" "$2"; exit 1; }"#;

/// Every git a script run as `fountain` uses reads only: no global or
/// system config, no hooks, fsmonitor, pager, external diff or askpass,
/// no remote ever contacted (no lazy fetch of a partial clone's missing
/// objects, every protocol refused), and every filter driver the
/// repository's own config names made inert
/// (its clean, smudge and process emptied), so a repository can't make a
/// read run its commands. Needs bash.
pub const GIT_SAFE: &str = r#"export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 GIT_ATTR_NOSYSTEM=1 GIT_NO_LAZY_FETCH=1 GIT_OPTIONAL_LOCKS=0 GIT_TERMINAL_PROMPT=0 GIT_PAGER=cat PAGER=cat
unset GIT_EXTERNAL_DIFF GIT_DIR GIT_WORK_TREE GIT_CONFIG GIT_CONFIG_PARAMETERS GIT_CONFIG_COUNT GIT_ASKPASS SSH_ASKPASS GIT_SSH GIT_SSH_COMMAND
git() {
  local o=(-c core.fsmonitor=false -c core.hooksPath=/dev/null -c core.pager=cat -c diff.external= -c core.untrackedCache=false -c core.sshCommand= -c core.askPass= -c protocol.allow=never)
  local k n
  while IFS= read -r k; do
    [ -n "$k" ] || continue
    n=${k#filter.}; n=${n%.*}
    case $n in *=*) return 1 ;; esac
    o+=(-c "filter.$n.clean=" -c "filter.$n.smudge=" -c "filter.$n.process=" -c "filter.$n.required=false")
  done < <(command git config --name-only --get-regexp '^filter\.' 2>/dev/null)
  command git "${o[@]}" "$@"
}
"#;

/// *Changes*: `$1` the unit's root, `$2` the sandbox. `ok`, then a line per
/// git checkout up to two levels down: `repo BASE<TAB>DIR`, where BASE is
/// what its edits are counted from (the upstream's merge base, else
/// `origin/HEAD`'s, else git's empty tree: everything in it is the
/// agent's). `err WHY` if it can't. Run after [`GIT_SAFE`].
pub const CHECKOUTS: &str = r#"real=$(cd -P -- "$2" 2>/dev/null && pwd -P) && top=$(cd -P -- "$1" 2>/dev/null && pwd -P) && case $real/ in "$top"/?*/) true ;; *) false ;; esac || { printf 'err %s: no such directory inside the runner'\''s sandboxes (%s)\n' "$2" "$1"; exit 0; }
cd -- "$real" || exit 0
echo ok
find . -maxdepth 3 -name .git -prune -print 2>/dev/null | LC_ALL=C sort | head -n 20 | while IFS= read -r g; do
  d=${g%/.git}; d=${d#.}; top=$real$d
  base=
  if u=$(cd -- "$top" && git rev-parse -q --verify '@{upstream}' 2>/dev/null); then base=$(cd -- "$top" && git merge-base HEAD "$u" 2>/dev/null); fi
  if [ -z "$base" ] && o=$(cd -- "$top" && git rev-parse -q --verify refs/remotes/origin/HEAD 2>/dev/null); then base=$(cd -- "$top" && git merge-base HEAD "$o" 2>/dev/null); fi
  printf 'repo %s\t%s\n' "${base:-EMPTY}" "$top"
done"#;

/// The diff block's prelude for `run_as`: `$1` (the repository) must be
/// inside `root`, really, and becomes that real path.
pub fn inside_prelude(root: &str) -> String {
    format!(
        "real=$(cd -P -- \"$1\" 2>/dev/null && pwd -P) && top=$(cd -P -- {} 2>/dev/null && pwd -P) && case $real/ in \"$top\"/?*/) true ;; *) false ;; esac || {{ printf 'err not inside the runner'\\''s sandboxes: %s\\n' \"$1\"; exit 0; }}\nset -- \"$real\" \"${{@:2}}\"\n",
        quote(root)
    )
}

/// git's empty tree, which a checkout with nothing upstream is diffed from.
pub const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// The checkouts [`CHECKOUTS`] found, as `(dir, base)`.
pub fn parse_checkouts(out: &str) -> Result<Vec<(String, String)>, String> {
    let mut lines = out.lines();
    match lines.next().map(str::trim) {
        Some("ok") => {}
        Some(l) if l.starts_with("err ") => return Err(l[4..].to_owned()),
        _ => return Err("couldn't look in the sandbox".into()),
    }
    Ok(lines
        .filter_map(|l| l.strip_prefix("repo "))
        .filter_map(|l| l.split_once('\t'))
        .map(|(base, dir)| {
            let base = if base == "EMPTY" { EMPTY_TREE } else { base };
            (dir.to_owned(), base.to_owned())
        })
        .filter(|(_, b)| b.len() >= 7 && b.bytes().all(|c| c.is_ascii_hexdigit()))
        .collect())
}

// ---------------------------------------------------------------- the view

/// One runner, as the view shows it.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct RunnerRow {
    pub id: String,
    pub name: String,
    pub online: bool,
    pub version: Option<String>,
    pub os: Option<String>,
    pub arch: Option<String>,
    pub root: Option<String>,
    pub last_seen_at: Option<String>,
    pub last_seen_ms: Option<u64>,
}

impl From<&Runner> for RunnerRow {
    fn from(r: &Runner) -> Self {
        Self {
            id: r.id.clone(),
            name: r.name.clone(),
            online: r.online,
            version: r.version.clone(),
            os: r.os.clone(),
            arch: r.arch.clone(),
            root: r.root.clone(),
            last_seen_at: r.last_seen_at.clone(),
            last_seen_ms: r.last_seen_at.as_deref().and_then(crate::gate::rfc3339_ms),
        }
    }
}

/// This host's runner among the account's: the one with the unit's name
/// (online first, then the latest seen, if Fountain has several).
pub fn this_runner<'a>(runners: &'a [Runner], unit: &Unit) -> Option<&'a Runner> {
    let seen = |r: &Runner| r.last_seen_at.as_deref().and_then(crate::gate::rfc3339_ms).unwrap_or(0);
    runners.iter().filter(|r| r.name == unit.name).max_by_key(|r| (r.online, seen(r)))
}

/// What wants the owner, if anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Want {
    /// The unit runs, but Fountain has said "offline" since `since_ms`
    /// (`None`: it doesn't list this runner at all).
    Offline { name: String, since_ms: u64, listed: bool },
    /// Other runners are online: one would win placement.
    Others { names: Vec<String>, this: String },
}

impl Want {
    /// One line, the same while the reason is (so it's raised once).
    pub fn headline(&self) -> String {
        match self {
            Want::Offline { name, listed: true, .. } => {
                format!("Fountain runner offline: {name} (its {UNIT} unit is active)")
            }
            Want::Offline { name, listed: false, .. } => {
                format!("Fountain runner offline: Fountain doesn't list {name} (its {UNIT} unit is active)")
            }
            Want::Others { names, this } => {
                format!("Another Fountain runner is online: {} (it would win placement over {this})", names.join(", "))
            }
        }
    }
}

/// The rules: offline for `grace` while the unit is active; another runner
/// online. `offline_seen_ms`: when this view first saw it offline (the
/// clock when Fountain has no last-seen time). Only a host with the unit
/// judges: elsewhere every runner is someone else's.
pub fn judge(
    runners: &[Runner],
    unit: Option<&Unit>,
    active: Option<bool>,
    offline_seen_ms: Option<u64>,
    now: u64,
    grace: Duration,
) -> Option<Want> {
    let unit = unit?;
    let this = this_runner(runners, unit);
    if active == Some(true) && this.is_none_or(|r| !r.online) {
        let seen = this.and_then(|r| r.last_seen_at.as_deref()).and_then(crate::gate::rfc3339_ms);
        let since = match (seen, offline_seen_ms) {
            (Some(s), _) => s,
            (None, Some(o)) => o,
            (None, None) => now,
        };
        if now.saturating_sub(since) >= grace.as_millis() as u64 {
            return Some(Want::Offline { name: unit.name.clone(), since_ms: since, listed: this.is_some() });
        }
    }
    let mut others: Vec<String> =
        runners.iter().filter(|r| r.online && r.name != unit.name).map(|r| r.name.clone()).collect();
    others.sort();
    others.dedup();
    (!others.is_empty()).then(|| Want::Others { names: others, this: unit.name.clone() })
}

/// When an offline runner becomes news, so the view reads again then.
pub fn offline_due(
    runners: &[Runner],
    unit: Option<&Unit>,
    active: Option<bool>,
    offline_seen_ms: Option<u64>,
    now: u64,
    grace: Duration,
) -> Option<u64> {
    let unit = unit?;
    let this = this_runner(runners, unit);
    if active != Some(true) || this.is_some_and(|r| r.online) {
        return None;
    }
    let seen = this.and_then(|r| r.last_seen_at.as_deref()).and_then(crate::gate::rfc3339_ms);
    let since = seen.or(offline_seen_ms).unwrap_or(now);
    let due = since + grace.as_millis() as u64;
    (due > now).then_some(due)
}

/// A runner id as a sandbox's name has it (no dashes).
pub fn compact(id: &str) -> String {
    id.replace('-', "")
}

/// Whether a sandbox is on `runner`: its runner, or (a parked one may have
/// lost it) a name of `runner-<id>-`.
pub fn ours(s: &Sandbox, runner: &str) -> bool {
    if s.runner.as_ref().and_then(|r| r.id.as_deref()) == Some(runner) {
        return true;
    }
    s.provider.as_deref().is_none_or(|p| p == "runner")
        && s.sprite_name.strip_prefix(&format!("runner-{}-", compact(runner))).is_some_and(|rest| !rest.is_empty())
}

/// Its directory on this host: the unit's `--root` and its name, only.
/// Fountain's own root is ignored, and a path Fountain gives must be exactly
/// that. `None` for anything else (no root, a name with a slash or `..`, a
/// path elsewhere). The scripts that use it check its real path too.
pub fn sandbox_dir(s: &Sandbox, root: Option<&str>) -> Option<String> {
    let clean = |p: &str| {
        p.starts_with('/') && !p.split('/').any(|c| c == ".." || c == ".") && !p.chars().any(char::is_control)
    };
    let name = &s.sprite_name;
    if name.is_empty() || name.contains('/') || name == ".." || name == "." || name.chars().any(char::is_control) {
        return None;
    }
    let root = root.filter(|r| clean(r))?.trim_end_matches('/');
    let dir = format!("{root}/{name}");
    match s.runner.as_ref().and_then(|r| r.path.as_deref()) {
        Some(p) if p.trim_end_matches('/') != dir => None,
        _ => Some(dir),
    }
}

/// A sandbox on this runner, as the view lists it.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct SandboxRow {
    pub id: String,
    pub name: String,
    pub status: String,
    /// Suspended: its processes stopped; it still opens.
    pub parked: bool,
    pub path: Option<String>,
    pub agent_id: Option<String>,
    /// The agent's name, once read.
    pub agent: Option<String>,
    pub mode: Option<String>,
    pub inserted_at: Option<String>,
    pub conversations: Vec<SandboxConversation>,
}

/// The sandboxes on `runner`, newest first.
pub fn rows(sandboxes: &[Sandbox], runner: &str, root: Option<&str>) -> Vec<SandboxRow> {
    let mut out: Vec<SandboxRow> = sandboxes
        .iter()
        .filter(|s| ours(s, runner))
        .map(|s| SandboxRow {
            id: s.id.clone(),
            name: s.sprite_name.clone(),
            status: s.status.clone(),
            parked: s.status == "suspended",
            path: sandbox_dir(s, root),
            agent_id: s.agent_id.clone(),
            agent: None,
            mode: s.mode.clone(),
            inserted_at: s.inserted_at.clone(),
            conversations: s.conversations.clone(),
        })
        .collect();
    out.sort_by(|a, b| b.inserted_at.cmp(&a.inserted_at));
    out
}

/// What `view: runner` shows (the block's state, under `runner`).
#[derive(Debug, Clone, Default, Serialize)]
pub struct ViewState {
    /// This host's `fountain-runner` unit, if it has one.
    pub unit: Option<Unit>,
    /// `systemctl is-active` (none without systemd or the unit).
    pub unit_active: Option<bool>,
    /// The installed `fountain --version`, to compare with the runner's.
    pub local_version: Option<String>,
    /// This host's runner, as Fountain lists it.
    pub this: Option<RunnerRow>,
    /// Every other runner on the account.
    pub others: Vec<RunnerRow>,
    /// This runner's sandboxes, newest first.
    pub sandboxes: Vec<SandboxRow>,
    pub sandboxes_error: Option<String>,
    /// What's raised now (the rail's line), if anything.
    pub attention: Option<String>,
    /// Since when this view has seen it offline while the unit runs.
    pub offline_since_ms: Option<u64>,
    pub note: &'static str,
}

impl ViewState {
    /// `capture --text`.
    pub fn text(&self, base: Option<&str>, now: u64) -> String {
        let mut out = String::from("Fountain runner");
        if let Some(b) = base {
            out.push_str(&format!(" on {b}"));
        }
        out.push('\n');
        if let Some(a) = &self.attention {
            out.push_str(&format!("! {a}\n"));
        }
        match (&self.unit, &self.this) {
            (Some(u), this) => {
                let unit = match self.unit_active {
                    Some(true) => "active",
                    Some(false) => "not active",
                    None => "unknown",
                };
                out.push_str(&format!("this host: {} (unit {UNIT} {unit})", u.name));
                match this {
                    Some(r) => {
                        out.push_str(&format!(", {}", if r.online { "online" } else { "offline" }));
                        out.push_str(&format!(
                            ", {}",
                            version_line(r.version.as_deref(), self.local_version.as_deref())
                        ));
                        if let Some(s) = r.last_seen_ms {
                            out.push_str(&format!(", last seen {}", ago(now, s)));
                        }
                        out.push_str(&format!(
                            ", {} sandbox{}",
                            self.sandboxes.len(),
                            if self.sandboxes.len() == 1 { "" } else { "es" }
                        ));
                    }
                    None => out.push_str(", not on Fountain's list"),
                }
                out.push('\n');
            }
            (None, _) => out.push_str(&format!("this host runs no {UNIT} unit\n")),
        }
        if self.others.is_empty() {
            out.push_str("other runners: none\n");
        } else {
            out.push_str("other runners:\n");
            for r in &self.others {
                let seen = r.last_seen_ms.map(|s| format!(", last seen {}", ago(now, s))).unwrap_or_default();
                out.push_str(&format!(
                    "  {} {}{}{}\n",
                    r.name,
                    if r.online { "online" } else { "offline" },
                    r.version.as_deref().map(|v| format!(", {v}")).unwrap_or_default(),
                    seen
                ));
            }
        }
        if let Some(e) = &self.sandboxes_error {
            out.push_str(&format!("sandboxes: {e}\n"));
        }
        if let (Some(u), true) = (&self.unit, self.this.is_some()) {
            out.push_str(&format!("sandboxes on {}:\n", u.name));
            if self.sandboxes.is_empty() {
                out.push_str("  none\n");
            }
            for s in &self.sandboxes {
                out.push_str(&format!(
                    "  {} {}{} {}\n",
                    s.name,
                    if s.parked { "parked" } else { s.status.as_str() },
                    s.agent.as_deref().map(|a| format!(" {a}")).unwrap_or_default(),
                    s.path.as_deref().unwrap_or("(no directory)")
                ));
                for c in &s.conversations {
                    out.push_str(&format!(
                        "    {} {}{}\n",
                        c.id,
                        c.status,
                        c.title.as_deref().map(|t| format!(" \"{t}\"")).unwrap_or_default()
                    ));
                }
            }
            out.push_str(&format!("{}\n", self.note));
        }
        out
    }
}

/// The runner's version against the installed CLI's.
pub fn version_line(runner: Option<&str>, local: Option<&str>) -> String {
    match (runner, local) {
        (Some(r), Some(l)) if r == l => format!("{r} (as installed)"),
        (Some(r), Some(l)) => format!("{r} (installed fountain is {l})"),
        (Some(r), None) => r.to_owned(),
        (None, Some(l)) => format!("version unknown (installed fountain is {l})"),
        (None, None) => "version unknown".into(),
    }
}

fn ago(now: u64, at: u64) -> String {
    let s = now.saturating_sub(at) / 1000;
    match s {
        0..60 => format!("{s}s ago"),
        60..3600 => format!("{}m ago", s / 60),
        3600..86400 => format!("{}h ago", s / 3600),
        _ => format!("{}d ago", s / 86400),
    }
}

// ---------------------------------------------------------------- the host's line

/// The last runner summary for `GET /api/host`: when it was read, and what.
static HOST: LazyLock<Mutex<(u64, Option<FountainRunnerInfo>, bool)>> = LazyLock::new(Mutex::default);

/// What a runner view (or the host's own refresh) last found.
pub fn remember(info: FountainRunnerInfo) {
    let mut h = HOST.lock().unwrap();
    (h.0, h.1) = (now_ms(), Some(info));
}

/// This host's runner line for `GET /api/host`: only where the unit is,
/// from what was last read (never Fountain on the request). Older than an
/// idle poll, it's read again in the background.
pub fn host_info(shell_env: &std::sync::Arc<crate::shellenv::ShellEnv>) -> Option<FountainRunnerInfo> {
    let unit = unit()?;
    let (at, info, busy) = HOST.lock().unwrap().clone();
    let stale = now_ms().saturating_sub(at) > idle_interval().as_millis() as u64;
    if stale && !busy && tokio::runtime::Handle::try_current().is_ok() {
        HOST.lock().unwrap().2 = true;
        let shell_env = shell_env.clone();
        tokio::spawn(async move {
            let info = read_host(&shell_env).await;
            let mut h = HOST.lock().unwrap();
            h.2 = false;
            if let Some(info) = info {
                (h.0, h.1) = (now_ms(), Some(info));
            } else {
                h.0 = now_ms();
            }
        });
    }
    Some(info.filter(|i| i.name == unit.name).unwrap_or(FountainRunnerInfo { name: unit.name, ..Default::default() }))
}

/// Read it for the host's line: the unit, `systemctl` and `/api/runners`.
async fn read_host(shell_env: &crate::shellenv::ShellEnv) -> Option<FountainRunnerInfo> {
    let unit = unit()?;
    let runner = super::local_runner(shell_env).await;
    let found = super::login::read(&runner).await.ok()?;
    let login = super::login::resolve(&found, None).ok()?;
    let client = super::api::Client::new(&login.base_url, &login.key);
    let (runners, active) = tokio::join!(client.runners(), unit_active());
    let runners = runners.ok()?.items;
    Some(summary(&runners, &unit, active, None, None))
}

/// The host's line from what was read.
pub fn summary(
    runners: &[Runner],
    unit: &Unit,
    active: Option<bool>,
    sandboxes: Option<u32>,
    problem: Option<String>,
) -> FountainRunnerInfo {
    let this = this_runner(runners, unit);
    FountainRunnerInfo {
        name: unit.name.clone(),
        online: Some(this.is_some_and(|r| r.online)),
        version: this.and_then(|r| r.version.clone()),
        unit_active: active,
        sandboxes,
        checked_ms: Some(now_ms()),
        problem,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fountain::api;

    fn fixture(f: &str) -> Vec<serde_json::Value> {
        let p = format!("{}/tests/fixtures/fountain/{f}", env!("CARGO_MANIFEST_DIR"));
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
        v["data"].as_array().unwrap().clone()
    }

    fn runners(rows: Vec<serde_json::Value>) -> Vec<Runner> {
        api::rows(rows).items
    }

    const UNIT_FILE_TEXT: &str = "[Unit]\nDescription=Fountain runner\n\n[Service]\nUser=fountain\nExecStart=/usr/local/bin/fountain runner --name runner-1 --root /srv/fountain/sandboxes\nRestart=always\n";

    fn unit1() -> Unit {
        parse_unit(UNIT_FILE_TEXT).unwrap()
    }

    #[test]
    fn the_unit_names_the_runner() {
        assert_eq!(
            unit1(),
            Unit {
                name: "runner-1".into(),
                root: Some("/srv/fountain/sandboxes".into()),
                bin: Some("/usr/local/bin/fountain".into())
            }
        );
        let u = parse_unit("ExecStart=fountain runner --name=box --root=/r\n").unwrap();
        assert_eq!((u.name.as_str(), u.root.as_deref(), u.bin), ("box", Some("/r"), None));
        assert_eq!(parse_unit("ExecStart=/usr/bin/fountain acp\n"), None);
        assert_eq!(parse_unit("[Service]\n"), None);
        assert_eq!(parse_version("fountain version v0.21.0\n").as_deref(), Some("v0.21.0"));
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn status_from_recorded_runners() {
        let rs = runners(fixture("runner-view-runners.json"));
        let now = rs[0].last_seen_at.as_deref().and_then(crate::gate::rfc3339_ms).unwrap() + 30_000;
        let u = unit1();
        let this = this_runner(&rs, &u).unwrap();
        assert!(this.online);
        assert_eq!(this.version.as_deref(), Some("v0.21.0"));
        let row = RunnerRow::from(this);
        assert_eq!(row.root.as_deref(), Some("/srv/fountain/sandboxes"));
        assert!(row.last_seen_ms.is_some());
        // Online, alone: nothing wants anyone.
        assert_eq!(judge(&rs, Some(&u), Some(true), None, now, GRACE), None);
        let s = summary(&rs, &u, Some(true), Some(2), None);
        assert_eq!((s.name.as_str(), s.online, s.version.as_deref()), ("runner-1", Some(true), Some("v0.21.0")));
        // Another host (no unit) judges nothing.
        assert_eq!(judge(&rs, None, Some(true), None, now, GRACE), None);
    }

    #[test]
    fn offline_while_the_unit_runs() {
        let mut rows = fixture("runner-view-runners.json");
        rows[0]["online"] = false.into();
        let rs = runners(rows);
        let seen = rs[0].last_seen_at.as_deref().and_then(crate::gate::rfc3339_ms).unwrap();
        let u = unit1();
        // Within the grace: nothing yet, but it's due then.
        assert_eq!(judge(&rs, Some(&u), Some(true), None, seen + 60_000, GRACE), None);
        assert_eq!(offline_due(&rs, Some(&u), Some(true), None, seen + 60_000, GRACE), Some(seen + 300_000));
        let w = judge(&rs, Some(&u), Some(true), None, seen + 300_000, GRACE).unwrap();
        assert_eq!(w, Want::Offline { name: "runner-1".into(), since_ms: seen, listed: true });
        assert_eq!(w.headline(), "Fountain runner offline: runner-1 (its fountain-runner unit is active)");
        // The unit stopped (someone meant it), or no systemd: no news.
        assert_eq!(judge(&rs, Some(&u), Some(false), None, seen + 900_000, GRACE), None);
        assert_eq!(judge(&rs, Some(&u), None, None, seen + 900_000, GRACE), None);
        // Fountain doesn't list it at all: counted from when the view saw that.
        let w = judge(&[], Some(&u), Some(true), Some(1_000), 1_000 + 300_000, GRACE).unwrap();
        assert!(matches!(w, Want::Offline { listed: false, .. }));
        assert_eq!(judge(&[], Some(&u), Some(true), Some(1_000), 2_000, GRACE), None);
    }

    #[test]
    fn a_stale_runner_online_would_win_placement() {
        // The recorded runners of M43's spike: one was online then. Here it's
        // another machine's, beside this host's own.
        let mut rows = fixture("runner-view-runners.json");
        let mut old = fixture("runners.json");
        old[0]["name"] = "laptop".into();
        rows.extend(old);
        let rs = runners(rows);
        let u = unit1();
        let now = crate::gate::rfc3339_ms("2026-10-04T01:06:00Z").unwrap();
        let w = judge(&rs, Some(&u), Some(true), None, now, GRACE).unwrap();
        assert_eq!(w, Want::Others { names: vec!["laptop".into()], this: "runner-1".into() });
        assert_eq!(w.headline(), "Another Fountain runner is online: laptop (it would win placement over runner-1)");
        // Offline ones don't count.
        let mut rows = fixture("runner-view-runners.json");
        rows.extend(fixture("runners.json").into_iter().skip(1));
        assert_eq!(judge(&runners(rows), Some(&u), Some(true), None, now, GRACE), None);
    }

    #[test]
    fn sandboxes_map_to_directories() {
        let rs = runners(fixture("runner-view-runners.json"));
        let sb: Vec<Sandbox> = api::rows(fixture("runner-view-sandboxes.json")).items;
        let id = &rs[0].id;
        let mine = rows(&sb, id, Some("/srv/fountain/sandboxes"));
        assert_eq!(mine.len(), 2, "{mine:?}");
        for r in &mine {
            assert_eq!(r.path.as_deref(), Some(format!("/srv/fountain/sandboxes/{}", r.name).as_str()));
            assert!(r.name.starts_with(&format!("runner-{}-", compact(id))));
            assert_eq!(r.conversations.len(), 1);
        }
        // Fountain's root is ignored: only the unit's.
        assert!(rows(&sb, id, None).iter().all(|r| r.path.is_none()));
        // A parked one that lost its runner: by its name, under the root.
        let parked: Sandbox = serde_json::from_value(serde_json::json!({
            "id": "s9", "sprite_name": format!("runner-{}-abcd1234", compact(id)), "status": "suspended",
            "provider": "runner", "runner": { "id": null, "path": null }, "conversations": []
        }))
        .unwrap();
        let r = rows(std::slice::from_ref(&parked), id, Some("/srv/fountain/sandboxes/"));
        assert_eq!(r[0].path.as_deref(), Some(format!("/srv/fountain/sandboxes/{}", parked.sprite_name).as_str()));
        assert!(r[0].parked);
        // Odd paths and names are refused: a path Fountain gives that isn't
        // the root's own, a name that climbs.
        let odd = |name: &str, path: &str| -> Sandbox {
            serde_json::from_value(serde_json::json!({
                "id": "s8", "sprite_name": name, "runner": { "id": id, "path": path }
            }))
            .unwrap()
        };
        assert_eq!(sandbox_dir(&odd("../etc", "/srv/../etc"), Some("/srv")), None);
        assert_eq!(sandbox_dir(&odd("runner-x-1", "/etc"), Some("/srv")), None);
        assert_eq!(sandbox_dir(&odd("runner-x-1", "/srv/runner-x-2"), Some("/srv")), None);
        assert_eq!(
            sandbox_dir(&odd("runner-x-1", "/srv/runner-x-1/"), Some("/srv")).as_deref(),
            Some("/srv/runner-x-1")
        );
        assert_eq!(sandbox_dir(&parked, None), None);
        assert_eq!(sandbox_dir(&parked, Some("relative")), None);
    }

    #[test]
    fn the_shell_and_sudo_forms() {
        assert_eq!(quote("/srv/a-b_c.d"), "/srv/a-b_c.d");
        assert_eq!(quote("it's $(x)"), r#"'it'\''s $(x)'"#);
        assert_eq!(quote(""), "''");
        let cmd = shell_command("/srv/fountain/sandboxes", "/srv/fountain/sandboxes/runner-ab-12");
        assert!(cmd.starts_with("exec sudo -n -u fountain /bin/bash -c 'real=$(cd -P -- \"$2\""), "{cmd}");
        assert!(
            cmd.ends_with(
                "cd -- \"$real\" && HOME=/home/fountain INPUTRC=/dev/null HISTFILE=/dev/null exec bash --noprofile --norc' _ /srv/fountain/sandboxes /srv/fountain/sandboxes/runner-ab-12"
            ),
            "{cmd}"
        );
        assert!(!cmd.contains("bash -l"));
        assert_eq!(
            sudo_argv("echo hi", &["/x y".into()]),
            ["sudo", "-n", "-u", "fountain", "/bin/bash", "-c", "echo hi", "_", "/x y"]
        );
        let out = "ok\nrepo EMPTY\t/s/r1\nrepo 0123456789abcdef\t/s/a/b c\nrepo $(rm)\t/s/x\n";
        assert_eq!(
            parse_checkouts(out).unwrap(),
            [("/s/r1".to_owned(), EMPTY_TREE.to_owned()), ("/s/a/b c".to_owned(), "0123456789abcdef".to_owned())]
        );
        assert!(parse_checkouts("err fountain has no directory /x\n").unwrap_err().contains("no directory"));
    }
}
