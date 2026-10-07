//! M44: wear a Fountain agent locally. A Claude Code agent block on this
//! host, configured as one of the account's agents: its system prompt, its
//! skills and its MCP servers (S24's `wear.py`, in Rust).
//!
//! **The bundle** is cached under `~/.cache/arugula/fountain/<agent
//! id>/<updated_at>/` (rebuilt once a day, for the GitHub skills):
//! - `plugin/`: a Claude Code plugin (`fountain-<name>`) whose `skills/`
//!   holds the agent's skills: inline ones written out, GitHub ones copied
//!   from a shallow clone in a shared cache (`…/fountain/github/`), fetched
//!   again once a day. A GitHub skill with a `name` is that directory of
//!   the repository; without one, every directory with a `SKILL.md`.
//! - `system.md`: the system prompt, after a preamble saying it runs
//!   locally.
//! - `bundle.json`: what's in it. Never a secret: the MCP servers' values
//!   are only ever in memory.
//!
//! **MCP servers** get Fountain's substitution ([`substitute`]): `${VAR}`
//! (an UPPER_SNAKE name) is replaced, `$$` is a literal `$`, a value isn't
//! expanded again, and every unset name is reported. A variable's value
//! comes, in order, from:
//! 1. Infisical: in the agent-specs checkout (its `.infisical.json`), the
//!    agent's environment in `dist/fountain.yaml` maps the variable to an
//!    `infisical:///<env>/<KEY>` URI (or to a literal value), read with
//!    `infisical secrets get KEY --env ENV --path PATH --plain --silent`
//!    there. A variable it doesn't map is tried as itself in env `dev`.
//! 2. The user's shell environment (#74).
//! 3. Helpers: `GITHUB_TOKEN` (and `GH_TOKEN`) from `gh auth token`.
//!
//! A server with an unset variable is left out, and so is one that needs an
//! OAuth sign-in, which a headless Claude Code can't do (no auth header,
//! and a known OAuth host, or a 401 with `WWW-Authenticate` when probed).
//! So is a Fountain connection, which only Fountain's sandboxes have. The
//! block's header names each, and why.
//!
//! **Not wearable:** a non-`claude` agent, and one whose metadata says
//! `arugula.local: false` (the orchestrators, written for Fountain).
//!
//! `ARUGULA_INFISICAL_BIN`, `ARUGULA_GH_BIN` and
//! `ARUGULA_FOUNTAIN_GIT_BASE` (instead of `https://github.com/`) are for
//! tests, which never reach the real ones.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::LazyLock,
    time::Duration,
};

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::{info, warn};

use super::{api::Agent, catalog};
use crate::{review::Runner, store::now_ms};

/// What the system prompt starts with: it was written for a sandbox.
pub const PREAMBLE: &str = "You are running as the Fountain agent \"{name}\", but locally, in Claude Code on the user's own \
machine, not in a Fountain sandbox. Where the instructions below mention /home/sprite, /workspace, vaults or spawning \
Fountain conversations, they describe the sandbox; here, work in the current directory with the user's own tools and \
credentials.\n\n---\n\n";

/// A bundle older than this is built again (GitHub skills move).
const FRESH: Duration = Duration::from_secs(24 * 3600);

/// A bundle version this old is removed (no block runs that long on one).
const OLD: Duration = Duration::from_secs(7 * 24 * 3600);

/// How long one `infisical`, `gh` or `git` may take.
const TOOL_TIMEOUT: Duration = Duration::from_secs(60);

/// How long an OAuth probe may take.
const PROBE_TIMEOUT: Duration = Duration::from_secs(4);

/// MCP hosts known to want an OAuth sign-in (S24: `mem0`).
const OAUTH_HOSTS: &[&str] = &["mcp.mem0.ai"];

static REF: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\$\$|\$\{([A-Z_][A-Z0-9_]*)\}").unwrap());

// ---------------------------------------------------------------- substitution

/// Add the variables `s` references to `out` (`$$` is none).
pub fn refs(s: &str, out: &mut BTreeSet<String>) {
    for c in REF.captures_iter(s) {
        if let Some(m) = c.get(1) {
            out.insert(m.as_str().to_owned());
        }
    }
}

/// Every variable referenced anywhere in `v` (strings at any depth).
pub fn refs_in(v: &Value, out: &mut BTreeSet<String>) {
    match v {
        Value::String(s) => refs(s, out),
        Value::Array(l) => l.iter().for_each(|x| refs_in(x, out)),
        Value::Object(m) => m.values().for_each(|x| refs_in(x, out)),
        _ => {}
    }
}

/// Fountain's substitution of one string (`Managoat.Substitution`): each
/// `${VAR}` from `vars`, `$$` a literal `$`, once (a value isn't expanded
/// again). With any variable missing, every missing name, sorted.
pub fn substitute(s: &str, vars: &BTreeMap<String, String>) -> Result<String, BTreeSet<String>> {
    let mut missing = BTreeSet::new();
    refs(s, &mut missing);
    missing.retain(|k| !vars.contains_key(k));
    if !missing.is_empty() {
        return Err(missing);
    }
    Ok(REF
        .replace_all(s, |c: &regex::Captures| match c.get(1) {
            None => "$".to_owned(),
            Some(m) => vars[m.as_str()].clone(),
        })
        .into_owned())
}

/// [`substitute`] over a whole value: strings at any depth (not keys);
/// every missing name across all of it.
pub fn substitute_value(v: &Value, vars: &BTreeMap<String, String>) -> Result<Value, BTreeSet<String>> {
    let mut missing = BTreeSet::new();
    let out = walk(v, vars, &mut missing);
    if missing.is_empty() { Ok(out) } else { Err(missing) }
}

fn walk(v: &Value, vars: &BTreeMap<String, String>, missing: &mut BTreeSet<String>) -> Value {
    match v {
        Value::String(s) => match substitute(s, vars) {
            Ok(s) => Value::String(s),
            Err(m) => {
                missing.extend(m);
                v.clone()
            }
        },
        Value::Array(l) => Value::Array(l.iter().map(|x| walk(x, vars, missing)).collect()),
        Value::Object(m) => Value::Object(m.iter().map(|(k, x)| (k.clone(), walk(x, vars, missing))).collect()),
        v => v.clone(),
    }
}

// ---------------------------------------------------------------- MCP servers

/// A server (as Fountain keeps it: Claude Code's config shape) as ACP's
/// `session/new` takes it: http `{type, name, url, headers: [{name,
/// value}]}`, stdio `{name, command, args, env: [{name, value}]}`. Or why
/// it can't be.
pub fn acp_server(name: &str, s: &Value) -> Result<Value, String> {
    let pairs = |v: &Value| -> Vec<Value> {
        v.as_object()
            .into_iter()
            .flatten()
            .filter_map(|(k, v)| v.as_str().map(|v| json!({ "name": k, "value": v })))
            .collect()
    };
    if let Some(url) = s["url"].as_str() {
        let kind = match s["type"].as_str().unwrap_or("http") {
            "http" | "streamable-http" | "streamableHttp" => "http",
            "sse" => "sse",
            other => return Err(format!("its type {other:?} isn't one Claude Code takes")),
        };
        return Ok(json!({ "type": kind, "name": name, "url": url, "headers": pairs(&s["headers"]) }));
    }
    if let Some(command) = s["command"].as_str() {
        let args: Vec<&str> = s["args"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        return Ok(json!({ "name": name, "command": command, "args": args, "env": pairs(&s["env"]) }));
    }
    if s.get("connection").is_some() {
        return Err("it's a Fountain connection, which only Fountain's sandboxes have".into());
    }
    Err("it has neither a url nor a command".into())
}

/// Whether an http server's headers carry credentials of their own.
fn has_auth(acp: &Value) -> bool {
    acp["headers"].as_array().into_iter().flatten().any(|h| {
        let n = h["name"].as_str().unwrap_or("").to_ascii_lowercase();
        n == "authorization" || n.contains("api-key") || n.contains("apikey") || n.contains("token")
    })
}

/// A URL on a host known to want an OAuth sign-in.
pub fn oauth_host(url: &str) -> Option<String> {
    let host = url::Url::parse(url).ok()?.host_str()?.to_ascii_lowercase();
    OAUTH_HOSTS.contains(&host.as_str()).then_some(host)
}

/// What a server answered an unauthenticated `initialize`: an OAuth
/// sign-in is what it wants when it's a 401 that says how to authenticate.
pub fn says_oauth(status: u16, www_authenticate: bool) -> bool {
    status == 401 && www_authenticate
}

/// Ask an http server, without credentials, to initialize (briefly). An
/// answer that doesn't come counts as no.
async fn probe_oauth(url: &str) -> bool {
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "arugula", "version": env!("CARGO_PKG_VERSION") } } });
    let req = crate::forge::http()
        .post(url)
        .header("accept", "application/json, text/event-stream")
        .json(&body)
        .timeout(PROBE_TIMEOUT)
        .send();
    match tokio::time::timeout(PROBE_TIMEOUT, req).await {
        Ok(Ok(r)) => says_oauth(r.status().as_u16(), r.headers().contains_key("www-authenticate")),
        _ => false,
    }
}

// ---------------------------------------------------------------- refusals

/// Why `a` can't be worn here, if it can't.
pub fn refusal(a: &Agent) -> Option<String> {
    if a.runtime != "claude" {
        let rt = if a.runtime.is_empty() {
            "an agent of another runtime".to_owned()
        } else {
            format!("a {} agent", a.runtime)
        };
        return Some(format!("{} is {rt}: Run here wears claude agents only; Run on Fountain", a.name));
    }
    if !catalog::local_ok(a) {
        return Some(format!(
            "{} is for Fountain only (metadata arugula.local: false; it's written for Fountain's sandboxes): Run on Fountain",
            a.name
        ));
    }
    None
}

// ---------------------------------------------------------------- agent-specs

/// What `dist/fountain.yaml` says: each Environment's and Vault's secrets
/// (`key` → `value`: an `infisical://` URI or a literal), and each Agent's
/// environment. A small reader for what chant writes, not YAML at large.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Specs {
    /// `(kind, name)` → key → value.
    pub secrets: BTreeMap<(String, String), BTreeMap<String, String>>,
    /// An agent's name → its environment's.
    pub environments: BTreeMap<String, String>,
}

/// A scalar as YAML writes it: double-quoted (JSON's escapes), single-quoted
/// (`''`), or plain.
fn scalar(s: &str) -> String {
    let s = s.trim();
    if s.starts_with('"') {
        return serde_json::from_str::<String>(s).unwrap_or_else(|_| s.trim_matches('"').to_owned());
    }
    if let Some(inner) = s.strip_prefix('\'').and_then(|x| x.strip_suffix('\'')) {
        return inner.replace("''", "'");
    }
    s.split(" #").next().unwrap_or(s).trim().to_owned()
}

pub fn parse_specs(yaml: &str) -> Specs {
    let mut out = Specs::default();
    for doc in yaml.split("\n---") {
        let mut kind = String::new();
        let mut name = String::new();
        let mut env = None;
        let mut secrets: BTreeMap<String, String> = BTreeMap::new();
        let (mut in_spec, mut in_secrets) = (false, false);
        let mut key: Option<String> = None;
        for line in doc.lines() {
            if line.trim().is_empty() || line.trim_start().starts_with('#') {
                continue;
            }
            let indent = line.len() - line.trim_start().len();
            let t = line.trim();
            if indent == 0 {
                in_spec = t == "spec:";
                in_secrets = false;
                if let Some(k) = t.strip_prefix("kind:") {
                    kind = scalar(k);
                }
                continue;
            }
            if !in_spec {
                continue;
            }
            if indent == 2 {
                in_secrets = t == "secrets:";
                if let Some(n) = t.strip_prefix("name:") {
                    name = scalar(n);
                } else if let Some(e) = t.strip_prefix("environment:") {
                    env = Some(scalar(e));
                }
                continue;
            }
            if !in_secrets {
                continue;
            }
            let item = t.strip_prefix("- ").unwrap_or(t);
            if let Some(k) = item.strip_prefix("key:") {
                key = Some(scalar(k));
            } else if let (Some(v), Some(k)) = (item.strip_prefix("value:"), key.as_ref()) {
                secrets.insert(k.clone(), scalar(v));
            }
        }
        if name.is_empty() {
            continue;
        }
        match kind.as_str() {
            "Environment" | "Vault" => {
                out.secrets.insert((kind, name), secrets);
            }
            "Agent" => {
                if let Some(e) = env {
                    out.environments.insert(name, e);
                }
            }
            _ => {}
        }
    }
    out
}

/// Where an `infisical://[project]/<env>/<path…>/<KEY>` URI points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InfisicalRef {
    pub project: Option<String>,
    pub env: String,
    pub path: String,
    pub key: String,
}

impl InfisicalRef {
    pub fn parse(uri: &str) -> Option<Self> {
        let rest = uri.strip_prefix("infisical://")?;
        let (project, rest) = rest.split_once('/')?;
        let segs: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
        let (key, mid) = segs.split_last()?;
        let (env, path) = mid.split_first()?;
        Some(Self {
            project: (!project.is_empty()).then(|| project.to_owned()),
            env: (*env).to_owned(),
            path: format!("/{}", path.join("/")),
            key: (*key).to_owned(),
        })
    }

    /// As the header says where a value came from.
    pub fn label(&self) -> String {
        let path = if self.path == "/" { String::new() } else { format!("{}/", self.path.trim_matches('/')) };
        format!("Infisical {}/{path}{}", self.env, self.key)
    }
}

// ---------------------------------------------------------------- secrets

/// The values found for some variables, and where each came from.
#[derive(Default)]
pub struct Found {
    pub values: BTreeMap<String, String>,
    pub from: BTreeMap<String, String>,
}

impl std::fmt::Debug for Found {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Found").field("from", &self.from).finish_non_exhaustive()
    }
}

/// Where the variables are looked up: the host's environment (the daemon's
/// with the user's shell's over it), and the agent-specs checkout.
pub struct Lookup<'a> {
    pub env: &'a [(String, String)],
    pub home: &'a Path,
    pub specs: Option<PathBuf>,
    /// The agent's environment's name, for the agent-specs mapping.
    pub environment: Option<String>,
    /// Its vault's, whose secrets win over the environment's (as on
    /// Fountain).
    pub vault: Option<String>,
}

impl Lookup<'_> {
    fn var(&self, k: &str) -> Option<String> {
        self.env
            .iter()
            .rev()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.clone())
            .or_else(|| std::env::var(k).ok())
            .filter(|v| !v.is_empty())
    }
}

/// A tool's output, run on this host with `env` (stdin closed; within
/// [`TOOL_TIMEOUT`]): stdout if it exited 0, else why not.
async fn run(argv: &[String], cwd: Option<&Path>, env: &[(String, String)]) -> Result<String, String> {
    let (bin, args) = argv.split_first().ok_or("nothing to run")?;
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(args)
        .envs(env.iter().map(|(k, v)| (k, v)))
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    let out = tokio::time::timeout(TOOL_TIMEOUT, cmd.output())
        .await
        .map_err(|_| format!("{bin} took too long"))?
        .map_err(|e| format!("can't run {bin}: {e}"))?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    let err = String::from_utf8_lossy(&out.stderr);
    let err = err.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    Err(format!("{bin} failed{}", if err.is_empty() { String::new() } else { format!(": {err}") }))
}

/// A setting from the host's environment (the block's, then the daemon's).
fn setting(env: &[(String, String)], var: &str, default: &str) -> String {
    env.iter()
        .rev()
        .find(|(k, _)| k == var)
        .map(|(_, v)| v.clone())
        .or_else(|| std::env::var(var).ok())
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| default.to_owned())
}

/// `~` and `~/…` against `home`.
pub fn expand(p: &str, home: &Path) -> PathBuf {
    match p.trim() {
        "~" => home.to_owned(),
        p => p.strip_prefix("~/").map(|r| home.join(r)).unwrap_or_else(|| PathBuf::from(p)),
    }
}

/// The values of `names`, from Infisical, then the shell environment, then
/// helpers. Names none of them has are left out.
pub async fn resolve(names: &BTreeSet<String>, l: &Lookup<'_>) -> Found {
    let mut found = Found::default();
    if names.is_empty() {
        return found;
    }
    // 1. Infisical, through agent-specs' mapping.
    if let Some(specs) = l.specs.as_ref().filter(|d| d.join(".infisical.json").is_file()) {
        let parsed =
            std::fs::read_to_string(specs.join("dist/fountain.yaml")).map(|y| parse_specs(&y)).unwrap_or_default();
        let of = |kind: &str, name: &Option<String>| {
            name.as_ref().and_then(|n| parsed.secrets.get(&(kind.to_owned(), n.clone()))).cloned().unwrap_or_default()
        };
        let mut mapped = of("Environment", &l.environment);
        mapped.extend(of("Vault", &l.vault));
        let infisical = setting(l.env, "ARUGULA_INFISICAL_BIN", "infisical");
        let jobs = names.iter().map(|name| {
            let (specs, infisical, mapped) = (specs.clone(), infisical.clone(), &mapped);
            async move {
                let (target, fallback) = match mapped.get(name) {
                    Some(v) if v.starts_with("infisical://") => (InfisicalRef::parse(v)?, false),
                    // A literal in agent-specs (a git identity): public.
                    Some(v) => return Some((name.clone(), v.clone(), "agent-specs".to_owned())),
                    None => {
                        (InfisicalRef { project: None, env: "dev".into(), path: "/".into(), key: name.clone() }, true)
                    }
                };
                let mut argv = vec![
                    infisical,
                    "secrets".into(),
                    "get".into(),
                    target.key.clone(),
                    "--env".into(),
                    target.env.clone(),
                    "--path".into(),
                    target.path.clone(),
                    "--plain".into(),
                    "--silent".into(),
                ];
                if let Some(p) = &target.project {
                    argv.extend(["--projectId".into(), p.clone()]);
                }
                match run(&argv, Some(&specs), l.env).await {
                    Ok(v) if !v.trim().is_empty() => {
                        let from = if fallback {
                            format!("{} (not mapped in agent-specs: tried as itself)", target.label())
                        } else {
                            target.label()
                        };
                        Some((name.clone(), v.trim_end_matches(['\n', '\r']).to_owned(), from))
                    }
                    Ok(_) => None,
                    Err(e) => {
                        info!(var = name, error = e, "not in Infisical");
                        None
                    }
                }
            }
        });
        for (name, value, from) in futures_util::future::join_all(jobs).await.into_iter().flatten() {
            found.values.insert(name.clone(), value);
            found.from.insert(name, from);
        }
    }
    // 2. The shell environment.
    for name in names {
        if found.values.contains_key(name) {
            continue;
        }
        if let Some(v) = l.var(name) {
            found.values.insert(name.clone(), v);
            found.from.insert(name.clone(), "shell environment".into());
        }
    }
    // 3. Helpers.
    for name in ["GITHUB_TOKEN", "GH_TOKEN"] {
        if names.contains(name) && !found.values.contains_key(name) {
            let gh = setting(l.env, "ARUGULA_GH_BIN", "gh");
            match run(&[gh, "auth".into(), "token".into()], Some(l.home), l.env).await {
                Ok(t) if !t.trim().is_empty() => {
                    found.values.insert(name.to_owned(), t.trim().to_owned());
                    found.from.insert(name.to_owned(), "gh auth token".into());
                }
                Ok(_) => {}
                Err(e) => info!(error = e, "gh auth token"),
            }
        }
    }
    found
}

// ---------------------------------------------------------------- the bundle

/// What a bundle holds (`bundle.json`): nothing secret.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Contents {
    pub agent: String,
    pub id: String,
    pub updated_at: Option<String>,
    pub built_ms: u64,
    pub plugin: String,
    /// The skills it has, by name.
    pub skills: Vec<String>,
    /// Skills that didn't come (`owner/repo:name` or `…:*`), and why.
    pub skills_missing: Vec<String>,
}

/// The bundle on disk, and the system prompt.
#[derive(Debug, Clone)]
pub struct Bundle {
    pub dir: PathBuf,
    pub plugin: PathBuf,
    pub system: String,
    pub contents: Contents,
}

/// Where Arugula caches bundles: `$XDG_CACHE_HOME` (else `~/.cache`)
/// `/arugula/fountain`.
pub fn cache_root(env: &[(String, String)], home: &Path) -> PathBuf {
    let xdg = env
        .iter()
        .rev()
        .find(|(k, _)| k == "XDG_CACHE_HOME")
        .map(|(_, v)| v.clone())
        .or_else(|| std::env::var("XDG_CACHE_HOME").ok())
        .filter(|v| v.starts_with('/'));
    let base = xdg.map(PathBuf::from).unwrap_or_else(|| home.join(".cache"));
    // (#505) Bundles cached before the rename are used where they are.
    arugula_core::rename::kept(base.join("arugula/fountain"), base.join("illogical/fountain"))
}

/// A name safe as one path component.
fn component(s: &str) -> String {
    let s: String = s.chars().map(|c| if c.is_ascii_alphanumeric() || "._-@".contains(c) { c } else { '-' }).collect();
    let s = s.trim_matches('.').to_owned();
    if s.is_empty() { "_".into() } else { s }
}

fn slug(s: &str) -> String {
    let s: String = s.to_ascii_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let s = s.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-");
    if s.is_empty() { "agent".into() } else { s }
}

/// A file's age, if it's there.
fn age(p: &Path) -> Option<Duration> {
    std::fs::metadata(p).ok()?.modified().ok()?.elapsed().ok()
}

/// `owner/repo`, shallow-cloned into the shared cache (fetched again once
/// it's a day old; an offline fetch keeps what's there).
async fn github(
    cache: &Path,
    source: &str,
    git_ref: Option<&str>,
    env: &[(String, String)],
) -> Result<PathBuf, String> {
    static SOURCE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$").unwrap());
    if !SOURCE.is_match(source) || source.contains("..") {
        return Err(format!("{source:?} isn't owner/repo"));
    }
    // A ref is a ref, never an option.
    if let Some(r) = git_ref
        && (r.is_empty() || r.starts_with('-') || r.chars().any(|c| c.is_control() || c.is_whitespace()))
    {
        return Err(format!("{r:?} isn't a git ref"));
    }
    let name = match git_ref {
        Some(r) => format!("{}@{}", source.replace('/', "__"), component(r)),
        None => source.replace('/', "__"),
    };
    let dir = cache.join("github").join(name);
    let stamp = dir.join(".git/arugula-fetched");
    let base = setting(env, "ARUGULA_FOUNTAIN_GIT_BASE", "https://github.com/");
    let url = format!("{base}{source}");
    if !dir.join(".git").is_dir() {
        std::fs::create_dir_all(cache.join("github")).map_err(|e| e.to_string())?;
        let tmp = cache.join("github").join(format!(".clone-{}-{}", std::process::id(), now_ms()));
        let mut argv: Vec<String> = vec!["git".into(), "clone".into(), "-q".into(), "--depth".into(), "1".into()];
        if let Some(r) = git_ref {
            argv.extend(["--branch".into(), r.to_owned()]);
        }
        argv.extend(["--end-of-options".into(), url.clone(), tmp.display().to_string()]);
        let r = run(&argv, None, env).await;
        if let Err(e) = r {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(format!("couldn't clone {source}: {e}"));
        }
        if std::fs::rename(&tmp, &dir).is_err() {
            // Another wear cloned it first.
            let _ = std::fs::remove_dir_all(&tmp);
        }
        let _ = std::fs::write(&stamp, b"");
    } else if age(&stamp).is_none_or(|a| a > FRESH) {
        let d = dir.display().to_string();
        let what = git_ref.unwrap_or("HEAD").to_owned();
        let fetched = run(
            &[
                "git".into(),
                "-C".into(),
                d.clone(),
                "fetch".into(),
                "-q".into(),
                "--depth".into(),
                "1".into(),
                "--end-of-options".into(),
                "origin".into(),
                what,
            ],
            None,
            env,
        )
        .await;
        match fetched {
            Ok(_) => {
                let reset = ["git", "-C", &d, "reset", "-q", "--hard", "FETCH_HEAD"].map(str::to_owned);
                if let Err(e) = run(&reset, None, env).await {
                    warn!(source, error = e, "skills: reset after fetch");
                }
            }
            Err(e) => warn!(source, error = e, "skills: fetch (keeping what's cached)"),
        }
        let _ = std::fs::write(&stamp, b"");
    }
    Ok(dir)
}

/// Each directory under `root` with a `SKILL.md` (not in `.git`), by its
/// name, in a stable order.
fn skill_dirs(root: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    let mut stack = vec![root.to_owned()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        let mut subs: Vec<PathBuf> = rd
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()) && e.file_name() != ".git")
            .map(|e| e.path())
            .collect();
        subs.sort();
        if d != root && d.join("SKILL.md").is_file() {
            out.push(d);
        }
        stack.extend(subs.into_iter().rev());
    }
    out.sort_by(|a, b| a.file_name().cmp(&b.file_name()).then(a.cmp(b)));
    out
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let t = e.file_type()?;
        let dest = to.join(e.file_name());
        if t.is_dir() {
            if e.file_name() != ".git" {
                copy_dir(&e.path(), &dest)?;
            }
        } else if t.is_file() {
            std::fs::copy(e.path(), &dest)?;
        }
        // Symlinks are left out: they could point anywhere.
    }
    Ok(())
}

/// Build `a`'s bundle under `cache` (or take the one there, if it's
/// fresh and whole): the plugin with its skills, and the system prompt.
pub async fn bundle(a: &Agent, cache: &Path, env: &[(String, String)]) -> Result<Bundle, String> {
    // `<id>/<updated_at>/` holds versions (`v<built_ms>-<pid>/`) and
    // `current`, naming the newest. A new one never replaces a folder a
    // running block may be reading its skills from.
    let parent = cache.join(component(&a.id)).join(component(a.updated_at.as_deref().unwrap_or("unknown")));
    let system = format!("{}{}", PREAMBLE.replace("{name}", &a.name), a.system.as_deref().unwrap_or(""));
    if let Ok(cur) = std::fs::read_to_string(parent.join("current"))
        && let dir = parent.join(component(cur.trim()))
        && let Ok(text) = std::fs::read_to_string(dir.join("bundle.json"))
        && let Ok(c) = serde_json::from_str::<Contents>(&text)
        // A day, skills that didn't come too (they're tried again then).
        && now_ms().saturating_sub(c.built_ms) < FRESH.as_millis() as u64
        && dir.join("plugin/skills").is_dir()
    {
        let plugin = dir.join("plugin");
        return Ok(Bundle { dir, plugin, system, contents: c });
    }
    std::fs::create_dir_all(&parent).map_err(|e| format!("can't make {}: {e}", parent.display()))?;
    let version = format!("v{}-{}", now_ms(), std::process::id());
    let dir = parent.join(&version);
    let plugin = dir.join("plugin");
    let tmp = parent.join(format!(".build-{version}"));
    let _ = std::fs::remove_dir_all(&tmp);
    let skills = tmp.join("plugin/skills");
    std::fs::create_dir_all(&skills).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(tmp.join("plugin/.claude-plugin")).map_err(|e| e.to_string())?;
    let plugin_name = format!("fountain-{}", slug(&a.name));
    let manifest = json!({
        "name": plugin_name,
        "description": format!("Skills of the Fountain agent {}, worn locally by Arugula", a.name),
        "version": "0.0.0",
    });
    std::fs::write(
        tmp.join("plugin/.claude-plugin/plugin.json"),
        serde_json::to_vec_pretty(&manifest).unwrap_or_default(),
    )
    .map_err(|e| e.to_string())?;
    let mut c = Contents {
        agent: a.name.clone(),
        id: a.id.clone(),
        updated_at: a.updated_at.clone(),
        built_ms: now_ms(),
        plugin: plugin_name,
        ..Contents::default()
    };
    for s in &a.skills {
        match (&s.content, &s.source) {
            (Some(content), _) => {
                let Some(name) = s.name.as_deref().map(component).filter(|n| n != "_") else {
                    c.skills_missing.push("an inline skill without a name".into());
                    continue;
                };
                if skills.join(&name).exists() {
                    continue;
                }
                std::fs::create_dir_all(skills.join(&name)).map_err(|e| e.to_string())?;
                std::fs::write(skills.join(&name).join("SKILL.md"), content).map_err(|e| e.to_string())?;
                c.skills.push(name);
            }
            (None, Some(source)) => {
                let want = s.name.clone();
                let label = format!("{source}:{}", want.as_deref().unwrap_or("*"));
                let repo = match github(cache, source, s.git_ref.as_deref(), env).await {
                    Ok(r) => r,
                    Err(e) => {
                        c.skills_missing.push(format!("{label} ({e})"));
                        continue;
                    }
                };
                let (skills2, want2) = (skills.clone(), want.clone());
                let got = tokio::task::spawn_blocking(move || -> std::io::Result<Vec<String>> {
                    let mut got = vec![];
                    for d in skill_dirs(&repo) {
                        let name = d.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                        if want2.as_ref().is_some_and(|w| *w != name) {
                            continue;
                        }
                        let dest = skills2.join(component(&name));
                        if dest.exists() {
                            continue;
                        }
                        copy_dir(&d, &dest)?;
                        got.push(component(&name));
                        if want2.is_some() {
                            break;
                        }
                    }
                    Ok(got)
                })
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| format!("copying {source}'s skills: {e}"))?;
                if got.is_empty() {
                    c.skills_missing.push(format!("{label} (not in the repository)"));
                }
                c.skills.extend(got);
            }
            (None, None) => c.skills_missing.push(format!("{} (neither inline nor from GitHub)", s.label())),
        }
    }
    std::fs::write(tmp.join("system.md"), &system).map_err(|e| e.to_string())?;
    std::fs::write(tmp.join("bundle.json"), serde_json::to_vec_pretty(&c).unwrap_or_default())
        .map_err(|e| e.to_string())?;
    if let Err(e) = std::fs::rename(&tmp, &dir) {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(format!("can't keep the bundle in {}: {e}", dir.display()));
    }
    let pointer = parent.join(format!(".current-{version}"));
    std::fs::write(&pointer, &version)
        .and_then(|_| std::fs::rename(&pointer, parent.join("current")))
        .map_err(|e| format!("can't point at the bundle: {e}"))?;
    // Older versions go once nothing could still be on them (a week).
    for e in std::fs::read_dir(&parent).into_iter().flatten().flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        if n != version && n.starts_with('v') && age(&e.path()).is_some_and(|a| a > OLD) {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
    Ok(Bundle { dir, plugin, system, contents: c })
}

// ---------------------------------------------------------------- wearing

/// A server that came along, and where its variables came from.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ServerInfo {
    pub name: String,
    /// http, sse or stdio.
    pub kind: String,
    /// `NAME from WHERE` for each variable it uses.
    pub vars: Vec<String>,
}

/// One that didn't, and why.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LeftOut {
    pub name: String,
    pub why: String,
}

/// What the block shows: nothing secret.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Info {
    pub agent: String,
    pub id: String,
    pub model: Option<String>,
    pub plugin: String,
    pub bundle: String,
    pub skills: Vec<String>,
    pub skills_missing: Vec<String>,
    pub servers: Vec<ServerInfo>,
    pub left_out: Vec<LeftOut>,
}

impl Info {
    /// One paragraph for `capture --text`.
    pub fn text(&self) -> String {
        let list = |v: &[String]| if v.is_empty() { "none".to_owned() } else { v.join(", ") };
        let servers: Vec<String> = self.servers.iter().map(|s| s.name.clone()).collect();
        let mut out = format!(
            "As the Fountain agent {}, locally. Skills: {}. MCP servers: {}.",
            self.agent,
            list(&self.skills),
            list(&servers)
        );
        let mut missing: Vec<String> = self.left_out.iter().map(|l| format!("{} ({})", l.name, l.why)).collect();
        missing.extend(self.skills_missing.iter().map(|s| format!("skill {s}")));
        if !missing.is_empty() {
            out.push_str(&format!(" Didn't carry over: {}.", missing.join("; ")));
        }
        out
    }
}

/// An agent, worn: what `session/new` gets. The servers' values (secrets
/// among them) are in memory only.
pub struct Worn {
    pub info: Info,
    pub system: String,
    pub plugin: PathBuf,
    /// ACP's shape, each credential a reference into `env`.
    pub servers: Vec<Value>,
    /// What the references are: the adapter's environment.
    pub env: Vec<(String, String)>,
    /// Every value that went into them which may be secret, to keep out of
    /// logs.
    pub secrets: Vec<String>,
}

impl std::fmt::Debug for Worn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Worn").field("info", &self.info).finish_non_exhaustive()
    }
}

impl Worn {
    /// Put it on `session/new`'s `_meta`: the system prompt, the plugin and
    /// the model, keeping `settingSources: []`.
    pub fn dress(&self, meta: &mut Value) {
        crate::agent::defs::wear_meta(meta, &self.system, &self.plugin, self.info.model.as_deref());
    }
}

/// Its model, as Claude Code names it (`anthropic/` taken off; another
/// provider's isn't Claude's).
pub fn model(a: &Agent) -> Option<String> {
    let m = a.model.trim();
    let m = m.strip_prefix("anthropic/").unwrap_or(m);
    (!m.is_empty() && !m.contains('/')).then(|| m.to_owned())
}

/// What the session gets of the agent's servers.
#[derive(Default)]
pub struct Served {
    /// ACP's shape, every credential a `${ARUGULA_FTN_…}` reference.
    pub list: Vec<Value>,
    /// What those references are: the adapter's environment (owner-only,
    /// unlike a command line).
    pub env: Vec<(String, String)>,
    pub infos: Vec<ServerInfo>,
    pub left: Vec<LeftOut>,
    /// Every value that may be secret, to keep out of logs.
    pub secrets: Vec<String>,
}

/// An environment variable's name for one value of a server (stable, so a
/// restart names it the same).
fn env_name(server: &str, part: &str, key: &str, taken: &[(String, String)]) -> String {
    let clean = |s: &str| -> String {
        s.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_uppercase() } else { '_' }).collect()
    };
    let base = if key.is_empty() {
        format!("ARUGULA_FTN_{}_{part}", clean(server))
    } else {
        format!("ARUGULA_FTN_{}_{part}_{}", clean(server), clean(key))
    };
    let mut name = base.clone();
    let mut n = 2;
    while taken.iter().any(|(k, _)| *k == name) {
        name = format!("{base}_{n}");
        n += 1;
    }
    name
}

/// What Claude Code's own `${…}` expansion would take hold of.
fn expandable(s: &str) -> bool {
    s.contains("${")
}

const LITERAL: &str =
    "it holds a literal ${…} (Fountain's $$ escape), which Claude Code would expand, so it can't be passed on as it is";

/// One server as the session gets it: `resolved` (substituted) in ACP's
/// shape, with each header and env value (and a URL that had a variable)
/// moved into `env` and replaced by a reference that Claude Code expands
/// itself (S24/M44: it does, in headers, URLs and a stdio server's env).
/// The SDK puts the whole config on `claude`'s command line, where any
/// local user can read it: so only references go there. A variable in a
/// stdio server's command or arguments would land on that server's own
/// command line, so such a server is left out.
fn by_reference(
    name: &str,
    raw: &Value,
    resolved: &Value,
    env: &mut Vec<(String, String)>,
) -> Result<(Value, Value), String> {
    // Why not, from the recipe as written (never from a resolved value).
    acp_server(name, raw)?;
    let full = acp_server(name, resolved)?;
    let mut acp = full.clone();
    let mut added: Vec<(String, String)> = vec![];
    let move_value = |part: &str, key: &str, v: &str, added: &mut Vec<(String, String)>| -> Result<String, String> {
        if expandable(v) {
            return Err(LITERAL.into());
        }
        let all: Vec<(String, String)> = env.iter().chain(added.iter()).cloned().collect();
        let n = env_name(name, part, key, &all);
        added.push((n.clone(), v.to_owned()));
        Ok(format!("${{{n}}}"))
    };
    if acp.get("url").is_some() {
        let url = full["url"].as_str().unwrap_or_default();
        let mut had = BTreeSet::new();
        refs(raw["url"].as_str().unwrap_or_default(), &mut had);
        if !had.is_empty() {
            acp["url"] = json!(move_value("URL", "", url, &mut added)?);
        } else if expandable(url) {
            return Err(LITERAL.into());
        }
        for h in acp["headers"].as_array_mut().into_iter().flatten() {
            let k = h["name"].as_str().unwrap_or_default().to_owned();
            let v = h["value"].as_str().unwrap_or_default().to_owned();
            h["value"] = json!(move_value("H", &k, &v, &mut added)?);
        }
    } else {
        let mut had = BTreeSet::new();
        refs(raw["command"].as_str().unwrap_or_default(), &mut had);
        for a in raw["args"].as_array().into_iter().flatten() {
            refs(a.as_str().unwrap_or_default(), &mut had);
        }
        if !had.is_empty() {
            let vars: Vec<String> = had.iter().map(|v| format!("${{{v}}}")).collect();
            return Err(format!(
                "{} in its command line would be readable by anyone on this machine (ps)",
                vars.join(", ")
            ));
        }
        let cmd = std::iter::once(&full["command"]).chain(full["args"].as_array().into_iter().flatten());
        if cmd.filter_map(Value::as_str).any(expandable) {
            return Err(LITERAL.into());
        }
        for e in acp["env"].as_array_mut().into_iter().flatten() {
            let k = e["name"].as_str().unwrap_or_default().to_owned();
            let v = e["value"].as_str().unwrap_or_default().to_owned();
            e["value"] = json!(move_value("E", &k, &v, &mut added)?);
        }
    }
    env.extend(added);
    Ok((acp, full))
}

/// Turn the agent's servers into what the session gets: substituted, each
/// credential passed by reference ([`by_reference`]), those that can't come
/// left out with why.
pub async fn servers(a: &Agent, found: &Found, probe: bool) -> Served {
    let mut out = Served::default();
    let mut probes = vec![];
    let mut fulls: Vec<Value> = vec![];
    let mut keys: Vec<Vec<String>> = vec![];
    for (name, s) in &a.mcp_servers {
        if name == crate::mcp::SERVER_NAME {
            out.left.push(LeftOut { name: name.clone(), why: "Arugula's own server has that name".into() });
            continue;
        }
        let raw = serde_json::to_value(s).unwrap_or_default();
        let mut used = BTreeSet::new();
        refs_in(&raw, &mut used);
        let resolved = match substitute_value(&raw, &found.values) {
            Ok(v) => v,
            Err(missing) => {
                let names: Vec<String> = missing.into_iter().collect();
                let why = match names.as_slice() {
                    [one] => format!("${{{one}}} isn't set (not in Infisical, the shell environment or a helper)"),
                    _ => format!(
                        "{} aren't set (not in Infisical, the shell environment or a helper)",
                        names.iter().map(|n| format!("${{{n}}}")).collect::<Vec<_>>().join(", ")
                    ),
                };
                out.left.push(LeftOut { name: name.clone(), why });
                continue;
            }
        };
        let mut env = out.env.clone();
        let (acp, full) = match by_reference(name, &raw, &resolved, &mut env) {
            Ok(v) => v,
            Err(why) => {
                out.left.push(LeftOut { name: name.clone(), why });
                continue;
            }
        };
        let kind = acp["type"].as_str().unwrap_or("stdio").to_owned();
        if kind != "stdio" && !has_auth(&acp) {
            let url = full["url"].as_str().unwrap_or("").to_owned();
            if let Some(h) = oauth_host(&url) {
                out.left.push(LeftOut { name: name.clone(), why: format!("it needs an OAuth sign-in ({h})") });
                continue;
            }
            if probe {
                probes.push((out.list.len(), url));
            }
        }
        // Values that may be secret: resolved ones, any typed in literally
        // (as M43's recipes hide them), and everything moved to the env.
        for v in used.iter().filter_map(|k| found.values.get(k)) {
            out.secrets.push(v.clone());
        }
        for (_, v) in s.headers.iter().chain(s.env.iter()) {
            if !super::api::is_reference(v) {
                out.secrets.push(v.clone());
            }
        }
        let vars = used.iter().map(|k| format!("{k} from {}", found.from.get(k).map_or("?", String::as_str))).collect();
        keys.push(env[out.env.len()..].iter().map(|(k, _)| k.clone()).collect());
        out.env = env;
        out.infos.push(ServerInfo { name: name.clone(), kind, vars });
        out.list.push(acp);
        fulls.push(full);
    }
    // Ask the ones without credentials whether they want an OAuth sign-in.
    let answers = futures_util::future::join_all(probes.iter().map(|(_, url)| probe_oauth(url))).await;
    let gone: BTreeSet<usize> = probes.iter().zip(answers).filter(|(_, oauth)| *oauth).map(|((i, _), _)| *i).collect();
    for i in gone.iter().rev() {
        out.list.remove(*i);
        let full = fulls.remove(*i);
        let info = out.infos.remove(*i);
        // Its references go with it.
        let mine = keys.remove(*i);
        out.env.retain(|(k, _)| !mine.contains(k));
        let host =
            url::Url::parse(full["url"].as_str().unwrap_or("")).ok().and_then(|u| u.host_str().map(str::to_owned));
        out.left.push(LeftOut {
            name: info.name,
            why: format!("it needs an OAuth sign-in{}", host.map(|h| format!(" ({h})")).unwrap_or_default()),
        });
    }
    out.left.sort_by(|a, b| a.name.cmp(&b.name));
    out.secrets.extend(out.env.iter().map(|(_, v)| v.clone()));
    out.secrets.retain(|s| s.len() >= 6);
    out.secrets.sort();
    out.secrets.dedup();
    // A reason never carries a value.
    for l in &mut out.left {
        if let Some(clean) = scrub(&l.why, &out.secrets) {
            l.why = clean;
        }
    }
    out
}

/// Read `which` from the account (with `profile`'s login on `runner`), and
/// say why it can't be worn if it can't.
pub async fn find(runner: &Runner, profile: Option<&str>, which: &str) -> Result<(Agent, super::login::Login), String> {
    let got = super::agents_for(runner, profile).await?;
    let a = super::find(&got.agents, which)
        .cloned()
        .ok_or_else(|| format!("no agent {which:?} on this Fountain account"))?;
    if let Some(why) = refusal(&a) {
        return Err(why);
    }
    Ok((a, got.login))
}

/// Wear `which` on this host: read it (fresh), build or take its bundle,
/// and resolve its MCP servers. `specs` is the agent-specs checkout
/// (`~/…` allowed; the default one if it's there).
pub async fn wear(
    runner: &Runner,
    profile: Option<&str>,
    specs: Option<&str>,
    vault: Option<&str>,
    which: &str,
) -> Result<Worn, String> {
    let Runner::Local { env, home } = runner else {
        return Err("a worn Fountain agent runs on this host, not on a machine".into());
    };
    let (listed, login) = find(runner, profile, which).await?;
    let client = super::api::Client::new(&login.base_url, &login.key);
    // Fresh, in case it changed since the list was read.
    let a = match client.agent(&listed.id).await {
        Ok(a) => a,
        Err(e) => {
            info!(agent = listed.name, error = %e, "reading the agent again; using the list's");
            listed
        }
    };
    if let Some(why) = refusal(&a) {
        return Err(why);
    }
    let cache = cache_root(env, home);
    let b = bundle(&a, &cache, env).await?;
    // The variables: through agent-specs' mapping for its environment.
    let mut names = BTreeSet::new();
    for s in a.mcp_servers.values() {
        refs_in(&serde_json::to_value(s).unwrap_or_default(), &mut names);
    }
    let specs_dir = specs.map(|s| expand(s, home)).or_else(|| {
        let d = expand(super::DEFAULT_SPECS, home);
        d.is_dir().then_some(d)
    });
    let mut environment = None;
    if !names.is_empty() {
        if let Some(id) = &a.environment_id
            && let Ok(envs) = client.environments().await
        {
            environment = envs.into_iter().find(|e| e.id == *id).map(|e| e.name).filter(|n| !n.is_empty());
        }
        if environment.is_none()
            && let Some(d) = &specs_dir
            && let Ok(y) = std::fs::read_to_string(d.join("dist/fountain.yaml"))
        {
            environment = parse_specs(&y).environments.get(&a.name).cloned();
        }
    }
    // Its vault: the one asked for, else the only one it may use.
    let mut vault = vault.map(str::to_owned);
    if vault.is_none() && !names.is_empty() {
        let allowed: Vec<&str> = a
            .extra
            .get("allowed_vault_ids")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        if let [one] = allowed.as_slice()
            && let Ok(vaults) = client.vaults().await
        {
            vault = vaults.into_iter().find(|v| v.id == *one).map(|v| v.name).filter(|n| !n.is_empty());
        }
    }
    let lookup = Lookup { env, home, specs: specs_dir, environment, vault };
    let found = resolve(&names, &lookup).await;
    let served = servers(&a, &found, true).await;
    info!(
        agent = a.name,
        skills = b.contents.skills.len(),
        servers = served.infos.len(),
        left_out = served.left.len(),
        "wearing a Fountain agent"
    );
    Ok(Worn {
        info: Info {
            agent: a.name.clone(),
            id: a.id.clone(),
            model: model(&a),
            plugin: b.contents.plugin.clone(),
            bundle: b.dir.display().to_string(),
            skills: b.contents.skills.clone(),
            skills_missing: b.contents.skills_missing.clone(),
            servers: served.infos,
            left_out: served.left,
        },
        system: b.system,
        plugin: b.plugin,
        servers: served.list,
        env: served.env,
        secrets: served.secrets,
    })
}

/// Replace every secret in a JSON line with `<redacted>` (as it's escaped
/// in JSON, and as it is).
pub fn scrub(line: &str, secrets: &[String]) -> Option<String> {
    let mut out: Option<String> = None;
    for s in secrets {
        let escaped = serde_json::to_string(s).unwrap_or_default();
        let escaped = escaped.trim_matches('"');
        for needle in [escaped, s.as_str()] {
            if needle.len() < 6 {
                continue;
            }
            let cur = out.as_deref().unwrap_or(line);
            if cur.contains(needle) {
                out = Some(cur.replace(needle, crate::fountain::api::REDACTED));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bundles cached before the rename are used where they are (#505).
    /// Unix: the runner's XDG_CACHE_HOME is a Unix path.
    #[cfg(unix)]
    #[test]
    fn bundles_cached_before_the_rename_stay_put() {
        let tmp = std::env::temp_dir().join(format!("arugula-cache-root-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let env = vec![("XDG_CACHE_HOME".to_owned(), tmp.display().to_string())];
        assert_eq!(cache_root(&env, Path::new("/nowhere")), tmp.join("arugula/fountain"));
        std::fs::create_dir_all(tmp.join("illogical/fountain")).unwrap();
        assert_eq!(cache_root(&env, Path::new("/nowhere")), tmp.join("illogical/fountain"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    fn vars(kv: &[(&str, &str)]) -> BTreeMap<String, String> {
        kv.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    /// `Managoat.Substitution`'s cases.
    #[test]
    fn substitution_as_fountain_does_it() {
        let ok = |s: &str, v: &[(&str, &str)]| substitute(s, &vars(v)).unwrap();
        assert_eq!(ok("hello ${NAME}", &[("NAME", "world")]), "hello world");
        assert_eq!(ok("${FIRST} ${LAST}", &[("FIRST", "John"), ("LAST", "Doe")]), "John Doe");
        assert_eq!(ok("${X} and ${X}", &[("X", "42")]), "42 and 42");
        assert_eq!(ok("plain text", &[]), "plain text");
        assert_eq!(ok("", &[]), "");
        // $$ is a literal $, next to a reference too; $${VAR} escapes one.
        assert_eq!(ok("$$", &[]), "$");
        assert_eq!(ok("$$${AMOUNT}", &[("AMOUNT", "100")]), "$100");
        assert_eq!(ok("$$$$", &[]), "$$");
        assert_eq!(ok("$${KEEP}", &[]), "${KEEP}");
        // Once: a value isn't expanded again.
        assert_eq!(ok("${A}", &[("A", "${B}"), ("B", "no")]), "${B}");
        // Only UPPER_SNAKE names are references.
        assert_eq!(ok("${lower} $HOME ${}", &[]), "${lower} $HOME ${}");
        // Missing: every one, sorted; nothing half-done.
        let err = |s: &str, v: &[(&str, &str)]| substitute(s, &vars(v)).unwrap_err().into_iter().collect::<Vec<_>>();
        assert_eq!(err("${MISSING}", &[]), ["MISSING"]);
        assert_eq!(err("${C} ${A} ${B}", &[]), ["A", "B", "C"]);
        assert_eq!(err("${A} ${B}", &[("A", "present")]), ["B"]);
        // Recursively, across a whole value.
        let v = json!({ "outer": { "inner": "${VAL}" }, "list": ["hello ${NAME}", 1], "num": 42 });
        assert_eq!(
            substitute_value(&v, &vars(&[("VAL", "42"), ("NAME", "world")])).unwrap(),
            json!({ "outer": { "inner": "42" }, "list": ["hello world", 1], "num": 42 })
        );
        let v = json!({ "a": "${FOO}", "b": ["${BAR}"] });
        assert_eq!(substitute_value(&v, &vars(&[])).unwrap_err().into_iter().collect::<Vec<_>>(), ["BAR", "FOO"]);
    }

    #[test]
    fn servers_in_acps_shape() {
        let http = acp_server(
            "github",
            &json!({ "type": "http", "url": "https://api.githubcopilot.com/mcp/", "headers": { "Authorization": "Bearer t" } }),
        )
        .unwrap();
        assert_eq!(
            http,
            json!({ "type": "http", "name": "github", "url": "https://api.githubcopilot.com/mcp/", "headers": [{ "name": "Authorization", "value": "Bearer t" }] })
        );
        assert!(has_auth(&http));
        let stdio =
            acp_server("chant", &json!({ "command": "npx", "args": ["@x/chant", "serve"], "env": { "K": "v" } }))
                .unwrap();
        assert_eq!(
            stdio,
            json!({ "name": "chant", "command": "npx", "args": ["@x/chant", "serve"], "env": [{ "name": "K", "value": "v" }] })
        );
        assert!(acp_server("gmail", &json!({ "connection": "c" })).unwrap_err().contains("Fountain connection"));
        assert!(acp_server("x", &json!({ "type": "ws", "url": "wss://x" })).is_err());
    }

    #[test]
    fn oauth_detection() {
        assert_eq!(oauth_host("https://mcp.mem0.ai/mcp").as_deref(), Some("mcp.mem0.ai"));
        assert_eq!(oauth_host("https://mcp.context7.com/mcp"), None);
        assert!(says_oauth(401, true));
        assert!(!says_oauth(401, false), "a bare 401 is a bad key, not a sign-in");
        assert!(!says_oauth(200, true) && !says_oauth(406, false));
    }

    fn agents() -> Vec<Agent> {
        let v: Value = serde_json::from_str(include_str!("../../tests/fixtures/fountain/agents.json")).unwrap();
        super::super::api::rows(v["data"].as_array().unwrap().clone()).items
    }

    fn named(name: &str) -> Agent {
        agents().into_iter().find(|a| a.name == name).unwrap()
    }

    #[test]
    fn who_can_be_worn() {
        assert_eq!(refusal(&named("games")), None);
        assert_eq!(refusal(&named("pr-reviewer")), None);
        let mut orch = named("orchestrator");
        orch.metadata.insert("arugula.local".into(), json!(false));
        assert!(refusal(&orch).unwrap().contains("for Fountain only"));
        orch.metadata.insert("arugula.local".into(), json!("false"));
        assert!(refusal(&orch).is_some(), "the string too");
        let codex = agents().into_iter().find(|a| a.runtime == "codex").unwrap();
        assert!(refusal(&codex).unwrap().contains("is a codex agent"));
        assert_eq!(model(&named("pr-reviewer")).as_deref(), Some("claude-sonnet-5"));
        assert_eq!(model(&Agent { model: "openai/gpt".into(), ..Agent::default() }), None);
    }

    /// pr-reviewer as the fixture has it: github through a variable,
    /// context7 open, mem0 OAuth. Credentials go by reference.
    #[tokio::test]
    async fn servers_resolve_or_are_left_out() {
        let pr = named("pr-reviewer");
        // The fixture's variables are all ${X}.
        let found = Found { values: vars(&[("X", "fake-secret-value-123")]), from: vars(&[("X", "gh auth token")]) };
        let out = servers(&pr, &found, false).await;
        let names: Vec<&str> = out.list.iter().map(|s| s["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["context7", "github"]);
        assert_eq!(out.list[1]["headers"][0]["value"], "${ARUGULA_FTN_GITHUB_H_AUTHORIZATION}");
        assert_eq!(
            out.env,
            [("ARUGULA_FTN_GITHUB_H_AUTHORIZATION".to_owned(), "Bearer fake-secret-value-123".to_owned())]
        );
        assert!(!serde_json::to_string(&out.list).unwrap().contains("fake-secret"), "only references");
        assert_eq!(out.infos[1].vars, ["X from gh auth token"]);
        assert_eq!(out.left, [LeftOut { name: "mem0".into(), why: "it needs an OAuth sign-in (mcp.mem0.ai)".into() }]);
        assert_eq!(out.secrets, ["Bearer fake-secret-value-123", "fake-secret-value-123"]);
        // Unset: left out, saying which.
        let out = servers(&pr, &Found::default(), false).await;
        assert_eq!(out.list.len(), 1);
        assert_eq!(out.left[0].name, "github");
        assert!(out.left[0].why.contains("${X} isn't set"), "{:?}", out.left);
        assert!(out.secrets.is_empty() && out.env.is_empty());
        // A Fountain connection, Arugula's own name, a variable on a
        // command line, a literal ${…}, a URL with a variable, and a type
        // that's wrong (said from the recipe, not its values).
        let mut odd = Agent { name: "odd".into(), ..Agent::default() };
        let mut add = |n: &str, v: Value| {
            odd.mcp_servers.insert(n.into(), serde_json::from_value(v).unwrap());
        };
        add("gmail", json!({ "connection": "c" }));
        add("arugula", json!({ "command": "x" }));
        add("argv", json!({ "command": "tool", "args": ["--key", "${X}"] }));
        add("escaped", json!({ "type": "http", "url": "https://h.example.com/mcp", "headers": { "A": "$${KEEP}" } }));
        add(
            "in-url",
            json!({ "type": "http", "url": "https://h.example.com/mcp?k=${X}", "headers": { "Authorization": "t" } }),
        );
        add("weird", json!({ "type": "${X}", "url": "https://h.example.com/mcp" }));
        add("stdio-env", json!({ "command": "tool", "args": ["serve"], "env": { "TOKEN": "${X}" } }));
        let out = servers(&odd, &found, false).await;
        let left: Vec<(&str, &str)> = out.left.iter().map(|l| (l.name.as_str(), l.why.as_str())).collect();
        assert_eq!(left.iter().map(|l| l.0).collect::<Vec<_>>(), ["argv", "arugula", "escaped", "gmail", "weird"]);
        assert!(left[0].1.contains("${X} in its command line"), "{left:?}");
        assert!(left[2].1.contains("literal ${"), "{left:?}");
        assert_eq!(left[4].1, "its type \"${X}\" isn't one Claude Code takes", "the recipe's, not the value");
        let url = out.list.iter().find(|s| s["name"] == "in-url").unwrap();
        assert_eq!(url["url"], "${ARUGULA_FTN_IN_URL_URL}");
        let stdio = out.list.iter().find(|s| s["name"] == "stdio-env").unwrap();
        assert_eq!(stdio["env"][0]["value"], "${ARUGULA_FTN_STDIO_ENV_E_TOKEN}");
        assert!(out.env.iter().any(|(k, v)| k == "ARUGULA_FTN_IN_URL_URL" && v.ends_with("k=fake-secret-value-123")));
        assert!(!serde_json::to_string(&out.list).unwrap().contains("fake-secret"));
        assert!(!format!("{:?}", out.left).contains("fake-secret"));
    }

    /// Infisical through agent-specs: the environment's mapping, the
    /// vault's over it, and a variable neither maps tried as itself.
    // Unix: a fake CLI made executable by its mode.
    #[cfg(unix)]
    #[tokio::test]
    async fn resolving_through_environment_then_vault() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = std::env::temp_dir().join(format!("arugula-resolve-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let specs = tmp.join("specs");
        std::fs::create_dir_all(specs.join("dist")).unwrap();
        std::fs::write(specs.join(".infisical.json"), "{}").unwrap();
        std::fs::write(
            specs.join("dist/fountain.yaml"),
            "kind: Environment\nspec:\n  name: e\n  secrets:\n    - key: TOKEN\n      value: \"infisical:///dev/ENV_KEY\"\n    - key: ONLY_ENV\n      value: \"infisical:///dev/ONLY_ENV_KEY\"\n---\nkind: Vault\nspec:\n  name: v\n  secrets:\n    - key: TOKEN\n      value: \"infisical:///dev/VAULT_KEY\"\n",
        )
        .unwrap();
        let fake = tmp.join("infisical");
        std::fs::write(&fake, "#!/bin/sh\necho \"value-of-$3\"\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let env = vec![("ARUGULA_INFISICAL_BIN".to_owned(), fake.display().to_string())];
        let names: BTreeSet<String> = ["TOKEN", "ONLY_ENV", "OTHER"].map(str::to_owned).into();
        let lookup = |vault: Option<&str>| Lookup {
            env: &env,
            home: &tmp,
            specs: Some(specs.clone()),
            environment: Some("e".into()),
            vault: vault.map(str::to_owned),
        };
        let f = resolve(&names, &lookup(Some("v"))).await;
        assert_eq!(f.values["TOKEN"], "value-of-VAULT_KEY", "the vault wins");
        assert_eq!(f.from["TOKEN"], "Infisical dev/VAULT_KEY");
        assert_eq!(f.values["ONLY_ENV"], "value-of-ONLY_ENV_KEY");
        assert_eq!(f.values["OTHER"], "value-of-OTHER");
        assert_eq!(f.from["OTHER"], "Infisical dev/OTHER (not mapped in agent-specs: tried as itself)");
        let f = resolve(&names, &lookup(None)).await;
        assert_eq!(f.values["TOKEN"], "value-of-ENV_KEY");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn agent_specs_mapping() {
        let yaml = "apiVersion: fountain.dev/v1\nkind: Agent\nmetadata:\n  name: pr-reviewer\nspec:\n  name: pr-reviewer\n  runtime: claude\n  environment: eng\n  mcp_servers:\n    github:\n      headers:\n        Authorization: \"Bearer ${GITHUB_TOKEN}\"\n---\napiVersion: fountain.dev/v1\nkind: Environment\nmetadata:\n  name: eng\nspec:\n  name: eng\n  packages:\n    apt:\n      - jq\n  secrets:\n    - key: GITHUB_TOKEN\n      value: \"infisical:///dev/GITHUB_TOKEN\"\n    - key: DEEP\n      value: 'infisical://proj/prod/apps/x/DEEP_KEY'\n    - key: GIT_AUTHOR_NAME\n      value: someone\n  metadata:\n    managed-by: chant\n---\nkind: Vault\nspec:\n  name: v\n  secrets:\n    - key: GITHUB_TOKEN\n      value: \"infisical:///dev/OTHER\"\n";
        let s = parse_specs(yaml);
        assert_eq!(s.environments["pr-reviewer"], "eng");
        let eng = &s.secrets[&("Environment".to_owned(), "eng".to_owned())];
        assert_eq!(eng["GITHUB_TOKEN"], "infisical:///dev/GITHUB_TOKEN");
        assert_eq!(eng["GIT_AUTHOR_NAME"], "someone");
        assert_eq!(s.secrets[&("Vault".to_owned(), "v".to_owned())]["GITHUB_TOKEN"], "infisical:///dev/OTHER");
        let r = InfisicalRef::parse(&eng["GITHUB_TOKEN"]).unwrap();
        assert_eq!(r, InfisicalRef { project: None, env: "dev".into(), path: "/".into(), key: "GITHUB_TOKEN".into() });
        assert_eq!(r.label(), "Infisical dev/GITHUB_TOKEN");
        let deep = InfisicalRef::parse(&eng["DEEP"]).unwrap();
        assert_eq!(
            (deep.project.as_deref(), deep.env.as_str(), deep.path.as_str(), deep.key.as_str()),
            (Some("proj"), "prod", "/apps/x", "DEEP_KEY")
        );
        assert_eq!(deep.label(), "Infisical prod/apps/x/DEEP_KEY");
        assert_eq!(InfisicalRef::parse("infisical:///dev"), None);
    }

    #[test]
    fn scrubbing() {
        let secrets = vec!["s3cret-\"quoted\"-value".to_owned(), "short".to_owned()];
        let line = json!({ "a": "x s3cret-\"quoted\"-value y", "b": "short" }).to_string();
        let out = scrub(&line, &secrets).unwrap();
        assert!(!out.contains("s3cret") && out.contains("<redacted>"), "{out}");
        assert!(out.contains("short"), "too short to be scrubbed (would wreck the JSON)");
        serde_json::from_str::<Value>(&out).unwrap();
        assert_eq!(scrub("{}", &secrets), None);
    }

    /// A git repository of skills under `base/owner/repo`.
    fn skills_repo(base: &Path, source: &str, skills: &[&str]) {
        let dir = base.join(source);
        std::fs::create_dir_all(&dir).unwrap();
        for s in skills {
            let d = dir.join("skills").join(s);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("SKILL.md"), format!("---\nname: {s}\ndescription: {s}\n---\n")).unwrap();
            std::fs::write(d.join("extra.txt"), "x").unwrap();
        }
        std::fs::write(dir.join("README.md"), "not a skill").unwrap();
        let git = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false"])
                .args(args)
                .current_dir(&dir)
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "git {args:?}");
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "skills"]);
    }

    #[tokio::test]
    async fn bundles_from_fixtures() {
        let tmp = std::env::temp_dir().join(format!("arugula-wear-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let git = tmp.join("git");
        skills_repo(&git, "acme/skills", &["code-review", "iterate-pr", "other"]);
        skills_repo(&git, "acme/more", &["alpha", "beta"]);
        let env = vec![("ARUGULA_FOUNTAIN_GIT_BASE".to_owned(), format!("file://{}/", git.display()))];
        let mut a = named("games");
        a.skills.extend([
            serde_json::from_value(json!({ "source": "acme/skills", "name": "code-review" })).unwrap(),
            serde_json::from_value(json!({ "source": "acme/more" })).unwrap(),
            serde_json::from_value(json!({ "source": "acme/skills", "name": "not-there" })).unwrap(),
            serde_json::from_value(json!({ "source": "acme/nope" })).unwrap(),
        ]);
        let cache = tmp.join("cache/arugula/fountain");
        let b = bundle(&a, &cache, &env).await.unwrap();
        assert_eq!(b.contents.skills, ["love2d", "pixijs", "screenshots-in-prs", "code-review", "alpha", "beta"]);
        assert_eq!(b.contents.skills_missing.len(), 2, "{:?}", b.contents.skills_missing);
        assert!(b.contents.skills_missing[0].starts_with("acme/skills:not-there (not in the repository)"));
        assert!(b.contents.skills_missing[1].starts_with("acme/nope:* (couldn't clone"));
        let skills = b.plugin.join("skills");
        assert!(skills.join("love2d/SKILL.md").is_file() && skills.join("code-review/extra.txt").is_file());
        assert!(!skills.join("iterate-pr").exists() && !skills.join("README.md").exists());
        let manifest: Value =
            serde_json::from_slice(&std::fs::read(b.plugin.join(".claude-plugin/plugin.json")).unwrap()).unwrap();
        assert_eq!(manifest["name"], "fountain-games");
        assert!(b.system.starts_with("You are running as the Fountain agent \"games\", but locally"));
        let versions = cache.join(&a.id).join("2026-09-02T09-40-03Z");
        assert_eq!(b.dir.parent(), Some(versions.as_path()), "{}", b.dir.display());
        assert_eq!(
            std::fs::read_to_string(versions.join("current")).unwrap(),
            b.dir.file_name().unwrap().to_string_lossy()
        );
        assert!(cache.join("github/acme__skills/.git").is_dir(), "one shared clone");
        // Fresh: taken as it is, skills that didn't come too (for a day).
        std::fs::write(skills.join("love2d/marker"), "").unwrap();
        let again = bundle(&a, &cache, &env).await.unwrap();
        assert_eq!(again.dir, b.dir);
        assert!(again.plugin.join("skills/love2d/marker").is_file());
        // A day old: a new version, and the old one stays for blocks on it.
        let mut c = again.contents.clone();
        c.built_ms -= FRESH.as_millis() as u64 + 1;
        std::fs::write(again.dir.join("bundle.json"), serde_json::to_vec(&c).unwrap()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let rebuilt = bundle(&a, &cache, &env).await.unwrap();
        assert_ne!(rebuilt.dir, b.dir);
        assert!(!rebuilt.plugin.join("skills/love2d/marker").exists());
        assert!(b.plugin.join("skills/love2d/marker").is_file(), "the old version is kept");
        assert_eq!(
            std::fs::read_to_string(versions.join("current")).unwrap(),
            rebuilt.dir.file_name().unwrap().to_string_lossy()
        );
        // A ref is never an option.
        let mut opt = named("games");
        opt.skills =
            vec![serde_json::from_value(json!({ "source": "acme/skills", "ref": "--upload-pack=x" })).unwrap()];
        opt.id = "opt".into();
        let o = bundle(&opt, &cache, &env).await.unwrap();
        assert!(o.contents.skills_missing[0].contains("isn't a git ref"), "{:?}", o.contents.skills_missing);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
