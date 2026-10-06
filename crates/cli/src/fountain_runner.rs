//! `illogical fountain runner` (M45): this machine as the account's
//! Fountain runner, the steps that don't need root.
//!
//! The root half is `scripts/fountain-runner-setup.sh` (a `fountain` user,
//! its sandboxes, a sudoers rule and the `fountain-runner` systemd unit).
//! After it:
//!
//! - `install` creates the runner's API key from your own Fountain login
//!   (`fountain keys create`), writes it as the `fountain` user's
//!   credentials through the sudoers rule (bash as `fountain`) without ever
//!   printing it, and starts the unit;
//! - `status` says whether the unit runs, and what Fountain's
//!   `GET /api/runners` says about every runner on the account;
//! - `adopt` moves agents to the runner provider (`PATCH /api/agents/:id`).
//!   An agent chant manages (agent-specs) is refused, since chant's next
//!   converge would put it back: it says what to change there instead.
//!
//! Reads and writes go to Fountain's HTTP API with your CLI login
//! (`FOUNTAIN_API_KEY`, or `~/.fountain/credentials` and its profile), and
//! a User-Agent of illogical's own (managoat.com refuses some defaults).

use std::{
    collections::HashMap,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, bail};
use clap::Subcommand;
use serde_json::{Value, json};

use crate::http::{Target, Url, enc, send};

/// The systemd unit the setup script writes.
pub const UNIT: &str = "fountain-runner";
const UNIT_FILE: &str = "/etc/systemd/system/fountain-runner.service";
/// The runner's own user, and where its key goes.
const USER: &str = "fountain";
const CREDENTIALS: &str = "/home/fountain/.fountain/credentials";
const SETUP: &str = "sudo bash scripts/fountain-runner-setup.sh \
--fountain \"$(command -v fountain)\" --node \"$(node -p process.execPath)\"";
const SETUP_URL: &str =
    "https://raw.githubusercontent.com/arugula-salad/illogical/main/scripts/fountain-runner-setup.sh";

#[derive(Subcommand)]
pub enum RunnerCmd {
    /// Make the runner's key and start the unit.
    ///
    /// After `sudo bash scripts/fountain-runner-setup.sh`: create the key
    /// (never printed), write it for the `fountain` user and start the unit.
    /// A key that's there already is kept.
    Install {
        /// The key's name on Fountain.
        #[arg(long, default_value = "illogical-runner")]
        key_name: String,
        /// Make a new key even if the runner has one (revoke the old one
        /// with `fountain keys revoke`).
        #[arg(long)]
        new_key: bool,
    },
    /// The unit (`systemctl is-active`) and every runner Fountain knows.
    Status,
    /// Put agents on the runner provider.
    ///
    /// `sandbox_provider: runner`. One that chant manages is refused, with
    /// what to change in agent-specs instead.
    Adopt {
        /// Agent names or ids.
        #[arg(required = true)]
        agents: Vec<String>,
        /// The agent-specs checkout, to name the file of an agent chant
        /// manages [default: ~/agent-specs, if it's there].
        #[arg(long, env = "ILLOGICAL_AGENT_SPECS")]
        specs: Option<PathBuf>,
    },
}

pub fn run(cmd: &RunnerCmd, json_out: bool) -> anyhow::Result<i32> {
    match cmd {
        RunnerCmd::Install { key_name, new_key } => install(key_name, *new_key),
        RunnerCmd::Status => status(json_out),
        RunnerCmd::Adopt { agents, specs } => adopt(agents, specs.clone(), json_out),
    }
}

// ---- the login and the API ----

/// A Fountain login: the CLI's, kept in memory.
struct Login {
    key: String,
    base: String,
}

impl Login {
    /// `FOUNTAIN_API_KEY`/`FOUNTAIN_BASE_URL`, else the profile
    /// (`FOUNTAIN_PROFILE`, else `default`) in `~/.fountain/credentials`.
    fn load() -> anyhow::Result<Self> {
        let profile = std::env::var("FOUNTAIN_PROFILE").ok().filter(|p| !p.is_empty());
        let profile = profile.as_deref().unwrap_or("default");
        let home = std::env::var_os("HOME").context("no HOME")?;
        let path = Path::new(&home).join(".fountain/credentials");
        let attrs = match std::fs::read_to_string(&path) {
            Ok(text) => parse_profiles(&text).remove(profile).unwrap_or_default(),
            Err(_) => HashMap::new(),
        };
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let key = env("FOUNTAIN_API_KEY").or_else(|| attrs.get("api_key").cloned()).with_context(|| {
            format!(
                "no Fountain login (FOUNTAIN_API_KEY, or profile {profile} in {}): run `fountain auth login`",
                path.display()
            )
        })?;
        let base = env("FOUNTAIN_BASE_URL")
            .or_else(|| attrs.get("base_url").cloned())
            .unwrap_or_else(|| "https://managoat.com".into());
        Ok(Self { key, base: base.trim_end_matches('/').to_owned() })
    }

    /// `METHOD /api<path>`, as JSON either way.
    fn api(&self, method: &str, path: &str, body: Option<&Value>) -> anyhow::Result<Value> {
        let url = Url::parse(&self.base)?;
        // Anything after the host (a self-hosted Fountain under a path).
        let rest = self.base.split("://").nth(1).unwrap_or_default();
        let under = rest.find('/').map_or("", |i| &rest[i..]);
        let auth = format!("Bearer {}", self.key);
        let agent = format!("illogical/{}", env!("CARGO_PKG_VERSION"));
        let body = body.map(Value::to_string).unwrap_or_default();
        let headers = [
            ("Authorization", auth.as_str()),
            ("User-Agent", agent.as_str()),
            ("Accept", "application/json"),
            ("Content-Type", "application/json"),
        ];
        let res = send(&Target::Url(url), method, &format!("{under}/api{path}"), &headers, body.as_bytes())
            .with_context(|| format!("{method} {}/api{path}", self.base))?;
        let status = res.status;
        let text = res.text()?;
        if !(200..300).contains(&status) {
            let short: String = text.chars().take(300).collect();
            bail!("Fountain answered {method} /api{path} with HTTP {status}: {short}");
        }
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).with_context(|| format!("{method} /api{path}: not JSON"))
    }
}

/// `~/.fountain/credentials`: `[profile]` sections of `key = value`, with
/// optional double quotes (the CLI's own reader, `credentials.ParseAll`).
fn parse_profiles(text: &str) -> HashMap<String, HashMap<String, String>> {
    let mut out: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut section = None;
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = Some(name.to_owned());
            continue;
        }
        let (Some(s), Some((k, v))) = (&section, line.split_once('=')) else { continue };
        let v = v.trim();
        let v = v.strip_prefix('"').and_then(|v| v.strip_suffix('"')).unwrap_or(v);
        out.entry(s.clone()).or_default().insert(k.trim().to_owned(), v.to_owned());
    }
    out
}

// ---- install ----

fn on_path(cmd: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(cmd).is_file()))
}

/// `sudo -n` (never a password prompt) running bash as the runner's user,
/// which the setup's sudoers rule allows.
fn as_runner(script: &str) -> Command {
    let mut c = Command::new("sudo");
    c.args(["-n", "-u", USER, "/bin/bash", "-c", script]);
    c
}

fn install(key_name: &str, new_key: bool) -> anyhow::Result<i32> {
    for need in ["fountain", "sudo", "systemctl"] {
        if !on_path(need) {
            bail!("needs {need} on PATH");
        }
    }
    let user_exists = Command::new("id").arg(USER).stdout(Stdio::null()).stderr(Stdio::null()).status()?.success();
    let rule = user_exists
        && as_runner("true").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success());
    if !user_exists || !Path::new(UNIT_FILE).exists() || !rule {
        eprintln!(
            "The runner's root setup hasn't run here yet (or its sudoers rule is missing). Run once, from a checkout of illogical:"
        );
        eprintln!();
        eprintln!("  {SETUP}");
        eprintln!();
        eprintln!("(The script is also at {SETUP_URL}.) Then run `illogical fountain runner install` again.");
        return Ok(1);
    }
    let login = Login::load()?;
    let has_key = as_runner(&format!("test -s {CREDENTIALS}")).status()?.success();
    if has_key && !new_key {
        println!("The runner has a key already ({CREDENTIALS}); keeping it (--new-key makes another).");
    } else {
        let key = create_key(key_name)?;
        let creds = credentials(&key, &login.base);
        let mut child = as_runner(&format!(
            "umask 077 && mkdir -p {dir} && cat > {CREDENTIALS}.new && mv -f {CREDENTIALS}.new {CREDENTIALS}",
            dir = Path::new(CREDENTIALS).parent().unwrap().display()
        ))
        .stdin(Stdio::piped())
        .spawn()?;
        child.stdin.take().unwrap().write_all(creds.as_bytes())?;
        if !child.wait()?.success() {
            bail!(
                "couldn't write {CREDENTIALS}; key {key_name} was made on Fountain and isn't used: `fountain keys list`, then `fountain keys revoke <id>`"
            );
        }
        println!("Wrote key {key_name} to {CREDENTIALS} (0600, owned by {USER}).");
    }
    // `enable` is the setup's (it isn't in the sudoers rule); a restart
    // starts it, or picks up a new key.
    let ok = Command::new("sudo").args(["-n", "systemctl", "restart", UNIT]).status()?.success();
    if !ok {
        bail!("`sudo -n systemctl restart {UNIT}` failed (is the setup's sudoers rule in place?)");
    }
    println!("Started {UNIT}. `illogical fountain runner status` shows it once it connects.");
    Ok(0)
}

/// `fountain keys create NAME`, its output kept from the terminal: the key
/// is on a line of its own, starting with the `Prefix:` it prints.
fn create_key(name: &str) -> anyhow::Result<String> {
    let out = Command::new("fountain")
        .args(["keys", "create", name])
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .context("running fountain keys create")?;
    if !out.status.success() {
        bail!("`fountain keys create {name}` failed");
    }
    created_key(&String::from_utf8_lossy(&out.stdout)).with_context(|| {
        format!("couldn't find the key in `fountain keys create`'s output; revoke {name} (`fountain keys list`, `fountain keys revoke <id>`) and make one in the dashboard")
    })
}

fn created_key(out: &str) -> Option<String> {
    let prefix = out.lines().find_map(|l| l.trim().strip_prefix("Prefix:")).map(str::trim).filter(|p| !p.is_empty())?;
    out.lines()
        .map(str::trim)
        .find(|l| l.starts_with(prefix) && l.len() > prefix.len() && !l.contains(char::is_whitespace))
        .map(str::to_owned)
}

/// The runner's credentials file: its key as the default profile.
fn credentials(key: &str, base: &str) -> String {
    format!("[default]\napi_key = \"{key}\"\nbase_url = \"{base}\"\n")
}

// ---- status ----

/// The runner's name, from the unit's `--name` (else this host's name).
fn runner_name(unit: Option<&str>) -> String {
    let from_unit = unit.and_then(|u| {
        let exec = u.lines().find_map(|l| l.trim().strip_prefix("ExecStart="))?;
        let mut words = exec.split_whitespace();
        words.find(|w| *w == "--name")?;
        words.next().map(str::to_owned)
    });
    from_unit.unwrap_or_else(|| {
        hostname().map(|h| h.split('.').next().unwrap_or_default().to_lowercase()).unwrap_or_default()
    })
}

#[cfg(unix)]
pub fn hostname() -> Option<String> {
    nix::unistd::gethostname().ok().and_then(|h| h.into_string().ok())
}

/// Windows keeps the machine's name in the environment.
#[cfg(not(unix))]
pub fn hostname() -> Option<String> {
    std::env::var("COMPUTERNAME").ok()
}

fn status(json_out: bool) -> anyhow::Result<i32> {
    let active = Command::new("systemctl")
        .args(["is-active", UNIT])
        .stderr(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_else(|_| "unknown".into());
    let name = runner_name(std::fs::read_to_string(UNIT_FILE).ok().as_deref());
    let runners = Login::load()?.api("GET", "/runners", None)?;
    let list = runners["data"].as_array().cloned().unwrap_or_default();
    if json_out {
        println!("{}", json!({ "unit": active, "name": name, "runners": list }));
        return Ok(0);
    }
    println!("{UNIT}: {active} (runner {name})");
    if list.is_empty() {
        println!("Fountain lists no runners.");
    }
    for r in &list {
        println!("{}", runner_line(r, &name));
    }
    for r in list.iter().filter(|r| r["online"] == true && r["name"] != name.as_str()) {
        println!(
            "Another runner is online ({}): Fountain may put a conversation there instead.",
            r["name"].as_str().unwrap_or("?")
        );
    }
    Ok(0)
}

fn runner_line(r: &Value, this: &str) -> String {
    let s = |k: &str| r[k].as_str().unwrap_or("?").to_owned();
    format!(
        "  {}{}  {}  v{}  {}/{}  last seen {}  root {}",
        s("name"),
        if r["name"] == this { " (this host)" } else { "" },
        if r["online"] == true { "online" } else { "offline" },
        s("version").trim_start_matches('v'),
        s("os"),
        s("arch"),
        s("last_seen_at"),
        s("root"),
    )
}

// ---- adopt ----

#[derive(Debug, PartialEq)]
enum Plan {
    /// agent-specs owns it: change it there.
    Chant,
    Already,
    Move(String),
}

fn plan(agent: &Value) -> Plan {
    if agent["metadata"]["managed-by"] == "chant" {
        Plan::Chant
    } else if agent["sandbox_provider"] == "runner" {
        Plan::Already
    } else {
        Plan::Move(agent["id"].as_str().unwrap_or_default().to_owned())
    }
}

fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.chars()
            .enumerate()
            .all(|(i, c)| if [8, 13, 18, 23].contains(&i) { c == '-' } else { c.is_ascii_hexdigit() })
}

fn find_agent(login: &Login, name: &str) -> anyhow::Result<Value> {
    if is_uuid(name) {
        return Ok(login.api("GET", &format!("/agents/{name}"), None)?["data"].clone());
    }
    let list = login.api("GET", &format!("/agents?search={}", enc(name)), None)?;
    list["data"]
        .as_array()
        .and_then(|a| a.iter().find(|a| a["name"] == name))
        .cloned()
        .with_context(|| format!("no agent named {name}"))
}

/// What to tell someone about an agent chant manages.
fn chant_refusal(name: &str, file: Option<&Path>) -> String {
    let file = file.map_or_else(
        || format!("its file under src/agents/ (the one with name: \"{name}\")"),
        |f| f.display().to_string(),
    );
    format!(
        "{name} is managed by chant (agent-specs), so it isn't changed here: chant's next apply would put it back.\n  \
         Set sandboxProvider: \"runner\" in {file},\n  then `make apply` in agent-specs (under `infisical run`)."
    )
}

/// The agent-specs file that declares `name: "<agent>"`.
fn spec_file(specs: &Path, agent: &str) -> Option<PathBuf> {
    let needle = format!("name: \"{agent}\"");
    let mut stack = vec![specs.join("src/agents")];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).ok()?.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "ts")
                && std::fs::read_to_string(&p).is_ok_and(|t| t.contains(&needle))
            {
                return Some(p);
            }
        }
    }
    None
}

fn adopt(agents: &[String], specs: Option<PathBuf>, json_out: bool) -> anyhow::Result<i32> {
    let login = Login::load()?;
    let specs = specs.or_else(|| {
        let d = Path::new(&std::env::var_os("HOME")?).join("agent-specs");
        d.is_dir().then_some(d)
    });
    let mut code = 0;
    let mut results = Vec::new();
    for name in agents {
        let agent = match find_agent(&login, name) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("{name}: {e:#}");
                results.push(json!({ "agent": name, "result": "error", "error": format!("{e:#}") }));
                code = 1;
                continue;
            }
        };
        let shown = agent["name"].as_str().unwrap_or(name);
        match plan(&agent) {
            Plan::Chant => {
                let file = specs.as_deref().and_then(|s| spec_file(s, shown));
                eprintln!("{}", chant_refusal(shown, file.as_deref()));
                results.push(json!({ "agent": shown, "result": "managed_by_chant", "file": file }));
                code = 1;
            }
            Plan::Already => {
                println!("{shown} is on the runner provider already.");
                results.push(json!({ "agent": shown, "result": "already" }));
            }
            Plan::Move(id) => {
                match login.api("PATCH", &format!("/agents/{id}"), Some(&json!({ "sandbox_provider": "runner" }))) {
                    Ok(_) => {
                        println!("{shown} now runs on the runner provider.");
                        results.push(json!({ "agent": shown, "result": "moved" }));
                    }
                    Err(e) => {
                        eprintln!("{shown}: {e:#}");
                        results.push(json!({ "agent": shown, "result": "error", "error": format!("{e:#}") }));
                        code = 1;
                    }
                }
            }
        }
    }
    if json_out {
        println!("{}", Value::Array(results));
    }
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_profiles_like_the_fountain_cli_reads_them() {
        let text = "[default]\napi_key = ftn_a\nbase_url = \"https://example.test\"\n\n# a comment\n[other]\napi_key=\"ftn_b\"\n";
        let p = parse_profiles(text);
        assert_eq!(p["default"]["api_key"], "ftn_a");
        assert_eq!(p["default"]["base_url"], "https://example.test");
        assert_eq!(p["other"]["api_key"], "ftn_b");
        let written = credentials("ftn_new", "https://example.test");
        assert_eq!(parse_profiles(&written)["default"]["api_key"], "ftn_new");
    }

    #[test]
    fn the_key_is_found_in_keys_creates_output() {
        // `fountain keys create` (CLI v0.21.0, cli/internal/cmd/keys.go).
        let out = "\n╭──╮\n│  Save this key — it will not be shown again.  │\n╰──╯\n\nftn_test_0000000000000000\n\nName:   geek-runner\nPrefix: ftn_test_0000\n\n";
        assert_eq!(created_key(out).as_deref(), Some("ftn_test_0000000000000000"));
        assert_eq!(created_key("something else\n"), None);
    }

    #[test]
    fn adopt_refuses_an_agent_chant_manages() {
        let chant = json!({ "id": "a", "name": "home-cloud-steward", "metadata": { "managed-by": "chant" }, "sandbox_provider": "sprites" });
        assert_eq!(plan(&chant), Plan::Chant);
        let msg = chant_refusal("home-cloud-steward", Some(Path::new("/specs/src/agents/x/home-cloud-steward.ts")));
        assert!(msg.contains("managed by chant") && msg.contains("sandboxProvider: \"runner\""), "{msg}");
        assert!(msg.contains("/specs/src/agents/x/home-cloud-steward.ts"), "{msg}");
        let mine = json!({ "id": "b", "name": "hud-playground", "metadata": {}, "sandbox_provider": "sprites" });
        assert_eq!(plan(&mine), Plan::Move("b".into()));
        let moved = json!({ "id": "c", "name": "fireball-smoke", "sandbox_provider": "runner" });
        assert_eq!(plan(&moved), Plan::Already);
    }

    #[test]
    fn the_runner_is_named_by_its_unit() {
        let unit = "[Service]\nExecStart=/usr/local/bin/fountain runner --name geek --root /home/fountain/sandboxes\n";
        assert_eq!(runner_name(Some(unit)), "geek");
        assert!(!runner_name(None).is_empty());
        assert!(is_uuid("0e30e590-d7d5-421f-95f6-d3a83a387b13"));
        assert!(!is_uuid("hud-playground"));
    }

    /// The setup script's `--dry-run`: the unit and the sudoers rule it
    /// renders, without root.
    #[test]
    #[cfg(unix)]
    fn the_setup_script_renders_the_unit_and_the_sudoers_rule() {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/fountain-runner-setup.sh");
        let tmp = std::env::temp_dir().join(format!("ilg-fountain-setup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        // A node install with npm beside it, and a fountain CLI.
        let node = tmp.join("node/bin/node");
        std::fs::create_dir_all(tmp.join("node/lib/node_modules/npm/bin")).unwrap();
        std::fs::create_dir_all(node.parent().unwrap()).unwrap();
        std::fs::write(tmp.join("node/lib/node_modules/npm/bin/npm-cli.js"), "").unwrap();
        std::fs::write(tmp.join("node/lib/node_modules/npm/bin/npx-cli.js"), "").unwrap();
        let fountain = tmp.join("fountain");
        for exe in [&node, &fountain] {
            std::fs::write(exe, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(exe, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        }
        let root = tmp.join("root");
        // The interim hardening drop-in: the unit replaces it.
        let dropin = root.join("etc/systemd/system/fountain-runner.service.d");
        std::fs::create_dir_all(&dropin).unwrap();
        std::fs::write(dropin.join("10-protect-proc.conf"), "[Service]\nProtectProc=invisible\n").unwrap();
        let setup = |extra: &[&str]| {
            Command::new("bash")
                .arg(&script)
                .args(["--user", "sam", "--name", "geek", "--dry-run"])
                .arg(&root)
                .arg("--fountain")
                .arg(&fountain)
                .arg("--node")
                .arg(&node)
                .args(extra)
                .env_remove("SUDO_USER")
                .output()
                .unwrap()
        };
        let out = setup(&[]);
        let said = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{said}{}", String::from_utf8_lossy(&out.stderr));
        let removes = [
            format!("+ rm -f {}", dropin.join("10-protect-proc.conf").display()),
            format!("+ rmdir --ignore-fail-on-non-empty {}", dropin.display()),
        ];
        for r in &removes {
            assert!(said.lines().any(|l| l == r), "{r} in\n{said}");
        }
        // ...and on --uninstall.
        let out = setup(&["--uninstall"]);
        let undone = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{undone}{}", String::from_utf8_lossy(&out.stderr));
        for r in &removes {
            assert!(undone.lines().any(|l| l == r), "{r} in\n{undone}");
        }
        assert!(said.contains("+ usermod --append --groups fountain sam"), "{said}");
        assert!(said.contains("+ install -d -m 2750 -o fountain -g fountain"), "{said}");

        let unit = std::fs::read_to_string(root.join("etc/systemd/system/fountain-runner.service")).unwrap();
        for line in [
            "User=fountain",
            "Restart=always",
            "UMask=0027",
            "ExecStart=/usr/local/bin/fountain runner --name geek --root /home/fountain/sandboxes",
            "Environment=PATH=/opt/fountain-node/bin:/usr/local/bin:/usr/bin:/bin",
            "ConditionPathExists=/home/fountain/.fountain/credentials",
            // Sandbox agents can't read other users' /proc/PID/cmdline.
            "ProtectProc=invisible",
        ] {
            assert!(unit.lines().any(|l| l == line), "{line} in\n{unit}");
        }
        assert_eq!(runner_name(Some(&unit)), "geek");
        // Not ProcSubset=pid: node reads /proc/cpuinfo and meminfo.
        assert!(!unit.lines().any(|l| l.starts_with("ProcSubset")), "{unit}");

        let sudoers = std::fs::read_to_string(root.join("etc/sudoers.d/illogical-fountain")).unwrap();
        let rules: Vec<&str> = sudoers.lines().filter(|l| !l.starts_with('#')).collect();
        assert_eq!(rules.len(), 2, "{sudoers}");
        assert_eq!(rules[0], "sam ALL=(fountain) NOPASSWD: /bin/bash");
        let (head, cmds) = rules[1].split_once("NOPASSWD: ").unwrap();
        assert_eq!(head, "sam ALL=(root) ");
        let verbs: Vec<&str> = cmds
            .split(", ")
            .map(|c| {
                let w: Vec<&str> = c.split(' ').collect();
                assert!(w[0].ends_with("/systemctl") && w[2] == "fountain-runner" && w.len() == 3, "{c}");
                w[1]
            })
            .collect();
        assert_eq!(verbs, ["start", "stop", "restart", "status"]);
        assert!(root.join("opt/fountain-node/bin/npx").symlink_metadata().is_ok());
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
