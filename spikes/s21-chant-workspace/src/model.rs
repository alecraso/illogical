//! S21: a chant workspace as one illogical block, read through chant's read
//! contract and nothing else.
//!
//! Shared by the spike's CLI (`src/main.rs`) and the throwaway daemon block
//! on the `s21-chant-workspace` branch (`crates/daemon/src/workspace.rs`
//! includes this file), so both show exactly the same thing.
//!
//! Two halves:
//!
//! - [`SCRIPT`] + [`READER`]: one `sh -c` on the block's host (one exec on a
//!   VM). It finds a chant, runs the reads in parallel with node, and prints
//!   one JSON document: what it ran, how long each took, and what each said.
//! - [`compose`]: pure. Turns that document into the block's state: member
//!   cards, records, pending gates, and why the block wants you.

use serde::Serialize;
use serde_json::Value;

/// `sh -c SCRIPT sh ROOT READER ENV`: the reads, as one JSON document on
/// stdout, whatever happens. Without node on the daemon's PATH it takes the
/// PATH an interactive shell has (node from mise, nvm or fnm is often set
/// up only in `.bashrc`), as VS Code resolves the shell environment.
pub const SCRIPT: &str = r#"cd "$1" 2>/dev/null || { printf '{"error":"no such directory"}'; exit 0; }
command -v node >/dev/null 2>&1 || PATH=$("${SHELL:-sh}" -ic 'printf %s "$PATH"' 2>/dev/null </dev/null) && export PATH
command -v node >/dev/null 2>&1 || { printf '{"error":"node is not on PATH on this host, even in an interactive shell"}'; exit 0; }
exec node -e "$2" "$1" "$3""#;

/// The reads. `process.argv`: root, env.
pub const READER: &str = r#"
const { execFile } = require("child_process"), fs = require("fs"), path = require("path");
const [root, env] = process.argv.slice(1);
function which() {
  if (process.env.CHANT) return [process.env.CHANT, "env"];
  for (let d = path.resolve(root); ; d = path.dirname(d)) {
    const p = path.join(d, "node_modules/.bin/chant");
    if (fs.existsSync(p)) return [p, d === path.resolve(root) ? "workspace" : "above"];
    if (d === path.dirname(d)) break;
  }
  for (const d of (process.env.PATH || "").split(":")) {
    const p = path.join(d, "chant");
    try { fs.accessSync(p, fs.constants.X_OK); return [p, "path"]; } catch {}
  }
  return [null, null];
}
const declared = fs.existsSync(path.join(root, "chant.workspace.json"));
const [chant, how] = which();
function run(args) {
  const t = Date.now();
  return new Promise((done) => execFile(chant, args, { cwd: root, maxBuffer: 64 << 20, timeout: 25000 }, (e, out, err) => {
    let json = null;
    try { json = JSON.parse(out); } catch {}
    done({ code: e ? (typeof e.code === "number" ? e.code : -1) : 0, ms: Date.now() - t, json, err: String(err).replace(/\x1b\[[0-9;]*m/g, "").trim().slice(-600) });
  }));
}
(async () => {
  const doc = { root: path.resolve(root), declared, chant, how, env };
  if (!declared || !chant) return console.log(JSON.stringify(doc));
  const t = Date.now();
  const [ls, check, records, status] = await Promise.all([
    run(["workspace", "ls", "--json"]),
    run(["workspace", "check", "--format", "json"]),
    run(["workspace", "records", "--current", "--json"]),
    run(["workspace", "status", env, "--json"]),
  ]);
  doc.ms = Date.now() - t;
  doc.reads = { ls, check, records, status };
  console.log(JSON.stringify(doc));
})();
"#;

/// `sh -c FINGERPRINT sh ROOT`: one line that changes when anything a read
/// would see might have: a commit, the `chant/lifecycle` ref (releases and
/// gates), or the working tree (the declaration, records, member sources).
/// About 0.1 CPU-seconds where a full read is about 7 (four chant
/// processes, each loading chant's TypeScript through tsx), so the block
/// polls this and reads only when it changes.
pub const FINGERPRINT: &str = r#"cd "$1" 2>/dev/null || { echo gone; exit 0; }
{ git rev-parse -q --verify HEAD; git rev-parse -q --verify refs/heads/chant/lifecycle; git status --porcelain=v1; git diff HEAD; } 2>/dev/null | cksum"#;

/// What the block draws and `describe` returns.
#[derive(Debug, Clone, Default, Serialize)]
pub struct State {
    pub root: String,
    pub name: Option<String>,
    /// Which chant answered, and how it was found: `workspace`
    /// (`node_modules/.bin` here), `above` (a parent's), `path`, `env`
    /// (`$CHANT`).
    pub chant: Option<String>,
    pub how: Option<String>,
    pub version: Option<String>,
    /// The environment `status` read gates and releases for.
    pub env: String,
    pub members: Vec<Member>,
    pub records: Vec<Record>,
    /// Why there are no records, when there aren't.
    pub records_note: Option<String>,
    pub gates: Vec<Gate>,
    /// Declaration findings for no member (or the workspace as a whole).
    pub diagnostics: Vec<Diagnostic>,
    /// Each read: how long, and whether it answered.
    pub reads: Vec<Read>,
    /// All of them, in parallel.
    pub ms: u64,
    /// The block can't show the workspace at all, and why.
    pub error: Option<String>,
    pub loading: bool,
    pub updated_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Member {
    pub name: String,
    pub dir: String,
    /// The member's directory on the host, for opening panes there.
    pub path: String,
    pub kind: String,
    pub because: Option<String>,
    pub roles: Vec<String>,
    /// A nested workspace: one card here, its own block when opened.
    pub nested: bool,
    /// Why chant couldn't read it.
    pub unreadable: Option<String>,
    pub errors: usize,
    pub warnings: usize,
    pub diagnostics: Vec<Diagnostic>,
    pub releases: usize,
    pub gates: usize,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Diagnostic {
    pub rule: String,
    pub severity: String,
    pub message: String,
    pub file: Option<String>,
    pub line: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Record {
    pub kind: String,
    pub id: String,
    pub title: Option<String>,
    pub state: Option<String>,
    pub ready: Option<bool>,
    pub blocked_by: Vec<String>,
    /// Pin drift and the like.
    pub warnings: Vec<String>,
    pub valid: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Gate {
    pub member: String,
    /// The member's directory on the host: where `approve` runs.
    pub path: String,
    pub op: String,
    pub gate: String,
    pub since: Option<String>,
    pub expires: Option<String>,
    pub needed: u64,
    pub approvals: usize,
    /// chant's own command for it.
    pub approve: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Read {
    pub name: String,
    pub ms: u64,
    pub code: i64,
    /// A JSON document came back.
    pub ok: bool,
    /// chant's own words when it didn't.
    pub note: Option<String>,
}

fn s(v: &Value) -> Option<String> {
    v.as_str().map(str::to_owned)
}

fn join(root: &str, dir: &str) -> String {
    if dir == "." || dir.is_empty() { root.to_owned() } else { format!("{}/{dir}", root.trim_end_matches('/')) }
}

/// The reader's document as block state.
pub fn compose(raw: &Value, env: &str) -> State {
    let mut st = State { env: env.to_owned(), root: s(&raw["root"]).unwrap_or_default(), ..Default::default() };
    st.chant = s(&raw["chant"]);
    st.how = s(&raw["how"]);
    if let Some(e) = s(&raw["error"]) {
        st.error = Some(e);
        return st;
    }
    if raw["declared"] == false {
        st.error = Some("no chant.workspace.json here: `chant workspace init` proposes one".into());
        return st;
    }
    if st.chant.is_none() {
        st.error = Some(
            "no chant here: not in node_modules/.bin (run npm install) and not on PATH".into(),
        );
        return st;
    }
    st.ms = raw["ms"].as_u64().unwrap_or(0);
    let reads = &raw["reads"];
    for name in ["ls", "check", "records", "status"] {
        let r = &reads[name];
        let ok = !r["json"].is_null();
        let code = r["code"].as_i64().unwrap_or(-1);
        let note = (!ok || code != 0)
            .then(|| s(&r["err"]).filter(|e| !e.is_empty()))
            .flatten()
            .map(|e| e.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_owned());
        st.reads.push(Read { name: name.into(), ms: r["ms"].as_u64().unwrap_or(0), code, ok, note });
    }
    st.version = s(&reads["ls"]["json"]["chant"]);

    // Members, from `ls`. Without it there's nothing to draw.
    let ls = &reads["ls"]["json"];
    if ls.is_null() {
        let why = st.reads.iter().find(|r| r.name == "ls").and_then(|r| r.note.clone());
        st.error = Some(format!("chant couldn't list the workspace: {}", why.unwrap_or_else(|| "no answer".into())));
        return st;
    }
    st.name = s(&ls["workspace"]["name"]);
    let root = st.root.clone();
    for m in ls["members"].as_array().into_iter().flatten() {
        let dir = s(&m["dir"]).unwrap_or_default();
        let kind = s(&m["kind"]).unwrap_or_default();
        st.members.push(Member {
            name: s(&m["name"]).unwrap_or_default(),
            path: join(&root, &dir),
            dir,
            nested: kind == "workspace",
            kind,
            because: s(&m["because"]),
            roles: m["roles"].as_array().into_iter().flatten().filter_map(s).collect(),
            unreadable: (m["readable"] == false)
                .then(|| s(&m["reason"]["message"]).or_else(|| s(&m["reason"]["code"])).unwrap_or_default()),
            ..Default::default()
        });
    }

    // Declaration findings, onto the member they name. `WSP009` ("kind
    // other, which chant does not read") is the declaration working as
    // meant, not a problem: it's the card's `because`.
    let check = &reads["check"]["json"];
    let diags = check["declaration"]["diagnostics"].as_array().into_iter().flatten().chain(check["findings"].as_array().into_iter().flatten());
    for d in diags {
        let rule = s(&d["ruleId"]).or_else(|| s(&d["code"])).unwrap_or_default();
        if rule == "WSP009" {
            continue;
        }
        let diag = Diagnostic {
            severity: s(&d["severity"]).unwrap_or_else(|| "warning".into()),
            message: s(&d["message"]).unwrap_or_default(),
            file: s(&d["file"]),
            line: d["line"].as_u64(),
            rule,
        };
        let on = s(&d["entity"]).or_else(|| s(&d["member"]));
        match st.members.iter_mut().find(|m| Some(&m.name) == on.as_ref()) {
            Some(m) => {
                match diag.severity.as_str() {
                    "error" => m.errors += 1,
                    "warning" => m.warnings += 1,
                    _ => {}
                }
                m.diagnostics.push(diag);
            }
            None => st.diagnostics.push(diag),
        }
    }

    // Releases and pending gates, from `status`.
    let status = &reads["status"]["json"];
    for sm in status["members"].as_array().into_iter().flatten() {
        let name = s(&sm["name"]).unwrap_or_default();
        let Some(m) = st.members.iter_mut().find(|m| m.name == name) else { continue };
        m.releases = sm["environments"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|e| e["releases"].as_array().map_or(0, Vec::len))
            .sum();
        for g in sm["gates"].as_array().into_iter().flatten() {
            if g["state"].as_str().is_some_and(|s| s != "pending") {
                continue;
            }
            m.gates += 1;
            st.gates.push(Gate {
                member: name.clone(),
                path: m.path.clone(),
                op: s(&g["component"]).unwrap_or_default(),
                gate: s(&g["name"]).unwrap_or_default(),
                since: s(&g["recordedAt"]),
                expires: s(&g["expiresAt"]),
                needed: g["needed"].as_u64().unwrap_or(1),
                approvals: g["approvals"].as_array().map_or(0, Vec::len),
                approve: s(&g["approve"]),
            });
        }
    }

    // Records: the current ones of every kind the declaration names.
    let rec = &reads["records"]["json"];
    let kinds: Vec<&Value> = match rec["kinds"].as_array() {
        Some(k) => k.iter().collect(),
        None if !rec.is_null() => vec![rec],
        None => vec![],
    };
    if kinds.is_empty() {
        st.records_note = Some(
            st.reads.iter().find(|r| r.name == "records").and_then(|r| r.note.clone()).unwrap_or_else(|| "no record kinds".into()),
        );
    }
    for k in kinds {
        let kind = s(&k["kind"]["name"]).or_else(|| s(&k["kind"])).or_else(|| s(&k["declared"]["name"])).unwrap_or_default();
        for r in k["records"].as_array().into_iter().flatten() {
            st.records.push(Record {
                kind: kind.clone(),
                id: s(&r["id"]).unwrap_or_default(),
                title: s(&r["data"]["title"]),
                state: s(&r["state"]),
                ready: r["ready"].as_bool(),
                blocked_by: r["blockedBy"].as_array().into_iter().flatten().filter_map(|b| s(&b["id"])).collect(),
                warnings: r["warnings"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|w| s(&w["message"]).or_else(|| s(&w["code"])).or_else(|| s(w)))
                    .collect(),
                valid: r["valid"].as_bool().unwrap_or(true),
            });
        }
    }
    st
}

impl State {
    /// Why it wants you, if it does: a gate waiting, a member chant can't
    /// read, an error in the declaration, a record whose pin drifted.
    pub fn attention(&self) -> Option<String> {
        if let Some(e) = &self.error {
            return Some(e.clone());
        }
        if let Some(g) = self.gates.first() {
            let more = if self.gates.len() > 1 { format!(" (+{} more)", self.gates.len() - 1) } else { String::new() };
            return Some(format!("{}: {} waits at gate {}{more}", g.member, g.op, g.gate));
        }
        if let Some(m) = self.members.iter().find(|m| m.unreadable.is_some() || m.errors > 0) {
            return Some(format!("{}: {}", m.name, m.unreadable.clone().unwrap_or_else(|| format!("{} errors", m.errors))));
        }
        self.records.iter().find(|r| !r.warnings.is_empty()).map(|r| format!("{}: {}", r.id, r.warnings[0]))
    }

    /// For `capture --text`, the TUI and the spike's CLI.
    pub fn text(&self) -> String {
        let mut out = format!(
            "{} ({})  chant {} via {}  {} ms\n",
            self.name.as_deref().unwrap_or("workspace"),
            self.root,
            self.version.as_deref().unwrap_or("?"),
            self.how.as_deref().unwrap_or("-"),
            self.ms
        );
        if let Some(e) = &self.error {
            out += &format!("\n  ! {e}\n");
            return out;
        }
        if !self.gates.is_empty() {
            out += "\nwaiting on you\n";
            for g in &self.gates {
                out += &format!("  ⏸ {}/{} gate {}  ({}/{})  {}\n", g.member, g.op, g.gate, g.approvals, g.needed, g.approve.as_deref().unwrap_or(""));
            }
        }
        out += &format!("\nmembers ({})\n", self.members.len());
        for m in &self.members {
            let mut tags = vec![m.kind.clone()];
            if m.errors > 0 {
                tags.push(format!("{} errors", m.errors));
            }
            if m.warnings > 0 {
                tags.push(format!("{} warnings", m.warnings));
            }
            if m.gates > 0 {
                tags.push(format!("{} gates", m.gates));
            }
            if m.releases > 0 {
                tags.push(format!("{} releases", m.releases));
            }
            if let Some(u) = &m.unreadable {
                tags.push(format!("unreadable: {u}"));
            }
            out += &format!("  {:<28} {:<36} {}\n", m.name, m.dir, tags.join(", "));
        }
        if !self.records.is_empty() {
            out += &format!("\nrecords ({})\n", self.records.len());
            for r in &self.records {
                let blocked = if r.blocked_by.is_empty() { String::new() } else { format!("  blocked by {}", r.blocked_by.join(", ")) };
                out += &format!(
                    "  {:<10} {:<12} {}{blocked}\n",
                    r.id,
                    r.state.as_deref().unwrap_or("-"),
                    r.title.as_deref().unwrap_or("")
                );
                for w in &r.warnings {
                    out += &format!("             ! {w}\n");
                }
            }
        } else if let Some(n) = &self.records_note {
            out += &format!("\nrecords: {n}\n");
        }
        out += "\nreads:";
        for r in &self.reads {
            out += &format!(" {} {}ms{}", r.name, r.ms, if r.ok { "" } else { " ✗" });
        }
        out.push('\n');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Relative to this file, so the daemon's copy of these tests finds them.
    fn fixture(text: &str) -> Value {
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn the_reference_workspace() {
        let st = compose(&fixture(include_str!("../fixtures/reference-raw.json")), "local");
        assert_eq!(st.error, None);
        assert_eq!(st.name.as_deref(), Some("reference"));
        assert_eq!(st.members.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), ["app", "delivery", "design-client", "design"]);
        let delivery = st.members.iter().find(|m| m.name == "delivery").unwrap();
        assert_eq!((delivery.kind.as_str(), delivery.gates, delivery.path.as_str()), ("chant", 1, "/scratch/refws/delivery"));
        // The approved gate isn't waiting; the new one is.
        assert_eq!(st.gates.len(), 1);
        assert_eq!((st.gates[0].op.as_str(), st.gates[0].gate.as_str()), ("release", "approve-release"));
        assert_eq!(st.attention().as_deref(), Some("delivery: release waits at gate approve-release"));
        let w2 = st.records.iter().find(|r| r.id == "W-002").unwrap();
        assert_eq!(w2.blocked_by, ["W-001"]);
        assert!(st.reads.iter().all(|r| r.ok));
    }

    #[test]
    fn no_chant_installed() {
        let st = compose(&fixture(include_str!("../fixtures/no-chant-raw.json")), "local");
        assert!(st.error.as_deref().unwrap().starts_with("no chant here"));
        assert!(st.members.is_empty());
    }
}
