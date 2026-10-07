//! The catalog's rules (M43): where an agent comes from, the cards a
//! client draws, and the filters.
//!
//! **Where it comes from:**
//! - **agent-specs**: `metadata.managed-by: chant` (declared in the
//!   agent-specs repository, chant's fountain lexicon);
//! - **an app**: `switchyard`, `salon`, `paddock`, `drydock`, `part-of` or
//!   `attemptId` in its metadata, a name ending in a UUID, or a name an app
//!   gives (`Mend: github.com/…`, `Cantor audit: …`);
//! - **hand-made**: everything else.
//!
//! **Filters** (kept in the block's config): words searched in the name,
//! description, skills and MCP servers (all must match); and chips for
//! source, runtime and sandbox provider (any of the chosen ones). Every
//! agent is listed: app-made ones are only filtered, never hidden by
//! default.

use std::{collections::BTreeMap, sync::LazyLock};

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::api::{Agent, Environment};

/// Where an agent comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Source {
    AgentSpecs,
    Hand,
    App,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::AgentSpecs => "agent-specs",
            Source::Hand => "hand",
            Source::App => "app",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "agent-specs" | "agent_specs" | "specs" | "chant" | "curated" => Some(Source::AgentSpecs),
            "hand" | "hand-made" | "handmade" => Some(Source::Hand),
            "app" | "app-made" | "apps" => Some(Source::App),
            _ => None,
        }
    }
}

/// Metadata keys an app sets on the agents it makes.
const APP_KEYS: [&str; 6] = ["switchyard", "salon", "paddock", "drydock", "part-of", "attemptId"];

static UUID_END: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$").unwrap()
});
/// `Mend: github.com/o/r`, `Cantor audit: …`, `Sage: o/r`.
static APP_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^([A-Z][A-Za-z]+)(?: [a-z]+)?: \S").unwrap());

/// Where `a` comes from, and for an app's, which app.
pub fn source(a: &Agent) -> (Source, Option<String>) {
    if a.metadata.get("managed-by").and_then(Value::as_str) == Some("chant") {
        return (Source::AgentSpecs, None);
    }
    if let Some(k) = APP_KEYS.iter().find(|k| a.metadata.contains_key(**k)) {
        return (Source::App, Some((*k).to_owned()));
    }
    if let Some(c) = APP_NAME.captures(&a.name) {
        return (Source::App, Some(c[1].to_ascii_lowercase()));
    }
    if UUID_END.is_match(&a.name) {
        return (Source::App, None);
    }
    (Source::Hand, None)
}

/// `metadata.arugula.local` (flat or nested): `false` (or `"false"`, as
/// some metadata keeps every value a string) says it's for Fountain only,
/// not to be worn here (M44). Agents written before the rename say it as
/// `illogical.local` (#505).
pub fn local_ok(a: &Agent) -> bool {
    let said = ["arugula", "illogical"]
        .into_iter()
        .find_map(|n| a.metadata.get(&format!("{n}.local")).or_else(|| a.metadata.get(n).and_then(|v| v.get("local"))));
    match said {
        Some(Value::Bool(false)) => false,
        Some(Value::String(s)) => !s.trim().eq_ignore_ascii_case("false"),
        _ => true,
    }
}

/// How a sandbox provider is named on a chip (null: Fountain's default).
pub fn provider(a: &Agent) -> String {
    a.sandbox_provider.clone().filter(|p| !p.is_empty()).unwrap_or_else(|| "default".into())
}

/// The filters, as kept in the block's config.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filter {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub query: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<Source>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtimes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<String>,
}

impl Filter {
    pub fn is_empty(&self) -> bool {
        self.query.trim().is_empty() && self.sources.is_empty() && self.runtimes.is_empty() && self.providers.is_empty()
    }

    /// Change it from a call's args: each of `query`, `source`, `runtime`
    /// and `provider` given replaces what was (a string or a list; empty
    /// or null clears it). `clear: true` first clears everything.
    pub fn apply(&mut self, args: &Value) -> Result<(), String> {
        if args["clear"] == true {
            *self = Filter::default();
        }
        let list = |v: &Value| -> Vec<String> {
            match v {
                Value::String(s) => s.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned).collect(),
                Value::Array(a) => a.iter().filter_map(Value::as_str).map(str::to_owned).collect(),
                _ => vec![],
            }
        };
        if let Some(q) = args.get("query") {
            self.query = q.as_str().unwrap_or_default().to_owned();
        }
        for key in ["source", "sources"] {
            if let Some(v) = args.get(key) {
                let mut s = list(v)
                    .iter()
                    .map(|s| Source::parse(s).ok_or_else(|| format!("no source {s:?}: agent-specs, hand or app")))
                    .collect::<Result<Vec<_>, _>>()?;
                s.sort();
                s.dedup();
                self.sources = s;
            }
        }
        for (keys, into) in
            [(["runtime", "runtimes"], &mut self.runtimes), (["provider", "providers"], &mut self.providers)]
        {
            for key in keys {
                if let Some(v) = args.get(key) {
                    let mut l: Vec<String> = list(v).into_iter().map(|s| s.to_ascii_lowercase()).collect();
                    l.sort();
                    l.dedup();
                    *into = l;
                }
            }
        }
        Ok(())
    }

    pub fn matches(&self, a: &Agent) -> bool {
        if !self.sources.is_empty() && !self.sources.contains(&source(a).0) {
            return false;
        }
        if !self.runtimes.is_empty() && !self.runtimes.iter().any(|r| r.eq_ignore_ascii_case(&a.runtime)) {
            return false;
        }
        if !self.providers.is_empty() && !self.providers.iter().any(|p| p.eq_ignore_ascii_case(&provider(a))) {
            return false;
        }
        let words: Vec<String> = self.query.split_whitespace().map(str::to_lowercase).collect();
        if words.is_empty() {
            return true;
        }
        let hay = haystack(a);
        words.iter().all(|w| hay.contains(w.as_str()))
    }

    /// The filter as words, for `capture --text` and the CLI.
    pub fn describe(&self) -> String {
        let mut parts = vec![];
        if !self.query.trim().is_empty() {
            parts.push(format!("{:?}", self.query.trim()));
        }
        if !self.sources.is_empty() {
            parts.push(format!("source {}", self.sources.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("|")));
        }
        if !self.runtimes.is_empty() {
            parts.push(format!("runtime {}", self.runtimes.join("|")));
        }
        if !self.providers.is_empty() {
            parts.push(format!("provider {}", self.providers.join("|")));
        }
        parts.join(", ")
    }
}

/// What a search looks in: name, description, skills, MCP servers.
fn haystack(a: &Agent) -> String {
    let mut s = format!("{}\n{}\n", a.name, a.description.as_deref().unwrap_or_default());
    for k in &a.skills {
        s.push_str(&k.label());
        s.push('\n');
        if let Some(src) = &k.source {
            s.push_str(src);
            s.push('\n');
        }
    }
    for n in a.mcp_servers.keys() {
        s.push_str(n);
        s.push('\n');
    }
    s.to_lowercase()
}

/// One agent as a client draws it (and MCP's `fountain_agents` returns it).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Card {
    pub id: String,
    pub name: String,
    pub description: String,
    pub runtime: String,
    pub model: String,
    pub skills: Vec<String>,
    pub mcp: Vec<String>,
    /// Its environment's name (its id if the name isn't known).
    pub environment: Option<String>,
    pub provider: String,
    pub mode: Option<String>,
    pub conversations: u64,
    pub updated_at: Option<String>,
    pub source: Option<Source>,
    /// Which app made it.
    pub app: Option<String>,
    /// Whether *Run here* could wear it (M44): `claude` agents not marked
    /// `arugula.local: false`.
    pub local: bool,
    /// Why not, when it's a claude agent that can't.
    pub local_why: Option<String>,
}

/// Descriptions on a card stop here.
const DESCRIPTION: usize = 400;

pub fn card(a: &Agent, envs: &BTreeMap<String, String>) -> Card {
    let (src, app) = source(a);
    let claude = a.runtime == "claude";
    let ok = local_ok(a);
    let mut description = a.description.clone().unwrap_or_default().trim().to_owned();
    if description.chars().count() > DESCRIPTION {
        description = description.chars().take(DESCRIPTION).collect::<String>() + "…";
    }
    Card {
        id: a.id.clone(),
        name: a.name.clone(),
        description,
        runtime: a.runtime.clone(),
        model: a.model.clone(),
        skills: a.skills.iter().map(|s| s.label()).collect(),
        mcp: a.mcp_servers.keys().cloned().collect(),
        environment: a.environment_id.as_ref().map(|id| envs.get(id).cloned().unwrap_or_else(|| id.clone())),
        provider: provider(a),
        mode: a.sandbox_mode.clone(),
        conversations: a.conversation_count,
        updated_at: a.updated_at.clone(),
        source: Some(src),
        app,
        local: claude && ok,
        local_why: (claude && !ok).then(|| "it's for Fountain only (metadata arugula.local: false)".to_owned()),
    }
}

/// Environment names by id.
pub fn env_names(envs: &[Environment]) -> BTreeMap<String, String> {
    envs.iter().map(|e| (e.id.clone(), e.name.clone())).collect()
}

/// How many agents each chip would show (among all of them).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Counts {
    pub source: BTreeMap<String, usize>,
    pub runtime: BTreeMap<String, usize>,
    pub provider: BTreeMap<String, usize>,
}

pub fn counts(agents: &[Agent]) -> Counts {
    let mut c = Counts::default();
    for a in agents {
        *c.source.entry(source(a).0.as_str().to_owned()).or_default() += 1;
        *c.runtime.entry(a.runtime.clone()).or_default() += 1;
        *c.provider.entry(provider(a)).or_default() += 1;
    }
    c
}

/// Agents in the order a catalog lists them: agent-specs, hand-made, then
/// apps'; by name within each.
pub fn sort(agents: &mut [Agent]) {
    agents.sort_by_key(|a| (source(a).0, a.name.to_lowercase()));
}

/// One line per agent, for `capture --text` and MCP's `fountain_agents`.
pub fn line(c: &Card) -> String {
    let mut s = format!("{} [{}", c.name, c.runtime);
    if !c.model.is_empty() {
        s.push_str(&format!(" {}", c.model));
    }
    s.push(']');
    let src = match (c.source, &c.app) {
        (Some(Source::App), Some(app)) => format!("app: {app}"),
        (Some(src), _) => src.as_str().to_owned(),
        (None, _) => String::new(),
    };
    s.push_str(&format!(" ({src})"));
    if !c.skills.is_empty() {
        s.push_str(&format!(" skills: {}", c.skills.join(", ")));
    }
    if !c.mcp.is_empty() {
        s.push_str(&format!(" mcp: {}", c.mcp.join(", ")));
    }
    if let Some(d) = c.description.lines().find(|l| !l.trim().is_empty()) {
        s.push_str(&format!(" — {}", d.trim()));
    }
    s
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn agents() -> Vec<Agent> {
        let v: Value = serde_json::from_str(include_str!("../../tests/fixtures/fountain/agents.json")).unwrap();
        serde_json::from_value(v["data"].clone()).unwrap()
    }

    fn named<'a>(all: &'a [Agent], name: &str) -> &'a Agent {
        all.iter().find(|a| a.name == name).unwrap_or_else(|| panic!("no {name}"))
    }

    #[test]
    fn where_each_comes_from() {
        let all = agents();
        let c = counts(&all);
        assert_eq!(c.source["agent-specs"], 23, "S24 counted 23 curated agents");
        assert_eq!(c.source.values().sum::<usize>(), all.len());
        assert_eq!(source(named(&all, "pr-reviewer")), (Source::AgentSpecs, None));
        assert_eq!(source(named(&all, "games")), (Source::Hand, None));
        assert_eq!(source(named(&all, "hud-playground")), (Source::Hand, None));
        assert_eq!(source(named(&all, "Switchyard · games")), (Source::App, Some("switchyard".into())));
        assert_eq!(source(named(&all, "Salon · Opus 5")).1.as_deref(), Some("salon"));
        assert_eq!(source(named(&all, "Paddock")).1.as_deref(), Some("paddock"));
        assert_eq!(source(named(&all, "Mend: github.com/example-org/lists")), (Source::App, Some("mend".into())));
        assert_eq!(source(named(&all, "Cantor audit: github.com/example-org/mend")).1.as_deref(), Some("cantor"));
        assert_eq!(source(named(&all, "example-builder-repair-9a3ff2a0-f2a1-496d-a72d-0da82f4beee4")).0, Source::App);
        // A key an app sets wins over the name.
        let a = Agent {
            name: "x".into(),
            metadata: json!({ "attemptId": "1" }).as_object().unwrap().clone(),
            ..Agent::default()
        };
        assert_eq!(source(&a), (Source::App, Some("attemptId".into())));
        let a = Agent {
            name: "y".into(),
            metadata: json!({ "part-of": "p" }).as_object().unwrap().clone(),
            ..Agent::default()
        };
        assert_eq!(source(&a).0, Source::App);
        // Plain words before a colon aren't an app.
        assert_eq!(source(&Agent { name: "notes: mine".into(), ..Agent::default() }).0, Source::Hand);
    }

    #[test]
    fn filters() {
        let all = agents();
        let shown = |f: &Filter| all.iter().filter(|a| f.matches(a)).map(|a| a.name.clone()).collect::<Vec<_>>();
        assert_eq!(shown(&Filter::default()).len(), all.len(), "everything, app-made too, by default");
        let mut f = Filter::default();
        f.apply(&json!({ "source": "agent-specs" })).unwrap();
        assert_eq!(shown(&f).len(), 23);
        // designer, found by its skill.
        f.apply(&json!({ "query": "frontend-design" })).unwrap();
        assert_eq!(shown(&f), ["designer"]);
        f.apply(&json!({ "source": [] })).unwrap();
        assert!(shown(&f).contains(&"designer".to_owned()));
        // Words all match, anywhere: name, description, skills, servers.
        let f = Filter { query: "PR github".into(), ..Filter::default() };
        assert!(shown(&f).contains(&"pr-reviewer".to_owned()));
        let f = Filter { query: "context7".into(), ..Filter::default() };
        assert!(shown(&f).iter().all(|n| named(&all, n).mcp_servers.contains_key("context7")));
        let f = Filter { query: "agent-skills".into(), ..Filter::default() };
        assert!(shown(&f).contains(&"pr-reviewer".to_owned()), "a GitHub skill's source");
        // Chips: any of the chosen, within a kind; all kinds at once.
        let mut f = Filter::default();
        f.apply(&json!({ "runtime": "codex,opencode" })).unwrap();
        assert!(shown(&f).iter().all(|n| ["codex", "opencode"].contains(&named(&all, n).runtime.as_str())));
        assert_eq!(shown(&f).len(), 7);
        let mut f = Filter::default();
        f.apply(&json!({ "provider": ["runner"] })).unwrap();
        assert_eq!(shown(&f).len(), 3, "S24: three agents on the runner provider");
        f.apply(&json!({ "provider": "default" })).unwrap();
        assert_eq!(shown(&f).len(), 95);
        f.apply(&json!({ "clear": true, "source": "hand,app" })).unwrap();
        assert_eq!((f.providers.len(), shown(&f).len()), (0, all.len() - 23));
        assert!(f.apply(&json!({ "source": "nope" })).is_err());
        assert_eq!(f.describe(), "source hand|app");
    }

    #[test]
    fn cards() {
        let all = agents();
        let envs = BTreeMap::from([(named(&all, "games").environment_id.clone().unwrap(), "games-env".to_owned())]);
        let c = card(named(&all, "games"), &envs);
        assert_eq!(c.skills, ["love2d", "pixijs", "screenshots-in-prs"]);
        assert_eq!((c.environment.as_deref(), c.provider.as_str(), c.local), (Some("games-env"), "default", true));
        let c = card(named(&all, "pr-reviewer"), &envs);
        assert_eq!(c.mcp, ["context7", "github", "mem0"]);
        assert!(
            c.skills.contains(&"code-review".to_owned()) && c.skills.contains(&"example-org/agent-skills/*".to_owned())
        );
        assert!(line(&c).starts_with("pr-reviewer [claude anthropic/"), "{}", line(&c));
        // Not claude: no Run here, and no reason needed.
        let codex = all.iter().find(|a| a.runtime == "codex").unwrap();
        assert_eq!((card(codex, &envs).local, card(codex, &envs).local_why), (false, None));
        // Marked for Fountain only (M44 adds it to the orchestrators).
        let mut orch = named(&all, "orchestrator").clone();
        orch.metadata.insert("arugula.local".into(), json!(false));
        let c = card(&orch, &envs);
        assert!(!c.local && c.local_why.unwrap().contains("Fountain only"));
        orch.metadata.remove("arugula.local");
        orch.metadata.insert("arugula".into(), json!({ "local": false }));
        assert!(!card(&orch, &envs).local);
        orch.metadata.remove("arugula");
        orch.metadata.insert("arugula.local".into(), json!("false"));
        assert!(!card(&orch, &envs).local, "as a string too");
        orch.metadata.insert("arugula.local".into(), json!("true"));
        assert!(card(&orch, &envs).local);
        orch.metadata.remove("arugula.local");
        // As agents written before the rename say it (#505).
        orch.metadata.insert("illogical.local".into(), json!(false));
        assert!(!card(&orch, &envs).local);
        orch.metadata.remove("illogical.local");
        orch.metadata.insert("illogical".into(), json!({ "local": "false" }));
        assert!(!card(&orch, &envs).local);
    }

    #[test]
    fn order() {
        let mut all = agents();
        sort(&mut all);
        assert_eq!(source(&all[0]).0, Source::AgentSpecs);
        assert_eq!(source(all.last().unwrap()).0, Source::App);
    }
}
