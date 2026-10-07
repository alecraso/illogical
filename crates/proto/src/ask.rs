//! Questions and forms from agents (M6c): what a client draws as a card, and
//! how a card's answer becomes what the agent wants back.
//!
//! An agent block gets them over ACP as `elicitation/create`; Claude Code
//! in a terminal block gets them through its `PreToolUse` hook on
//! `AskUserQuestion` (`arugula ask`). Either way a client sees an [`Ask`]
//! and answers with the same `content`: Claude's AskUserQuestion form, whose
//! fields are `question_<n>` (an option label, or a list of them for a
//! multi-select) and `question_<n>_custom` (the "Other" box), or for any
//! other form whatever its JSON Schema asks for.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// The tool whose questions get a question card.
pub const ASK_USER_QUESTION: &str = "AskUserQuestion";

/// An open question or form, as clients draw it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ask {
    /// What `answer` and `decline` take.
    pub id: String,
    pub kind: AskKind,
    /// The question (one), a generic "Please answer…" (several), or the
    /// form's or link's message.
    pub message: String,
    /// `questions`: Claude's AskUserQuestion input, as the tool has it
    /// (`question`, `header`, `multiSelect`, `options[{label, description,
    /// preview?}]`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub questions: Option<Value>,
    /// `form`: the requested JSON Schema, as the agent sent it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<Value>,
    /// `url`: the link to open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// `url`: it was opened (answered `accept`); the card waits for the
    /// agent to say it's finished.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub accepted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// `agent` (an agent block), `hook` (Claude Code in a terminal), or
    /// whatever raised it on a block through `/ask` (M35: `hud`, a studio
    /// box's agent).
    pub source: String,
    /// Who asks, as the card and the swarm's rail name it ("hud asks"),
    /// when that isn't the terminal's own agent (M35).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    pub at_ms: u64,
    /// `permission` (M29): the tool Claude Code asks to use, its input
    /// (`command`, `file_path`, `old_string`/`new_string`, …) and Claude's
    /// own "always allow" suggestions, as its `PermissionRequest` hook has
    /// them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggestions: Option<Value>,
    /// `permission`: Claude Code's session (and subagent), whose next step
    /// closes the card if the terminal answered it first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AskKind {
    /// Claude's AskUserQuestion: a question card.
    Questions,
    /// Any other form, drawn from its schema.
    Form,
    /// A link to open (an MCP server's sign-in).
    Url,
    /// A tool Claude Code in a terminal asks to use (M29): allow once,
    /// allow always (one of its suggestions), or deny with a message.
    Permission,
}

/// Who answered a card, and how (M29): the first answer wins, and every
/// other client's card closes saying so ("Allowed by Sam, 14:02").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answered {
    /// The ask it answered.
    pub id: String,
    /// `allowed`, `allowed always`, `denied`, `answered`, `skipped`, or
    /// what the terminal did (`allowed in the terminal`, `closed`).
    pub how: String,
    /// Principal id (`owner`, `account:…`), or `terminal`.
    pub who: String,
    pub name: String,
    pub at_ms: u64,
    /// What was asked, in a line.
    pub headline: String,
}

/// What a permission card says: `Bash: cargo test`, `Edit: src/main.rs`.
pub fn permission_message(tool: &str, input: &Value) -> String {
    let what = ["command", "file_path", "path", "url", "pattern", "description"]
        .iter()
        .find_map(|k| input[*k].as_str())
        .map(|s| s.lines().next().unwrap_or("").to_owned());
    match what {
        Some(w) if !w.is_empty() => format!("{tool}: {w}"),
        _ => tool.to_owned(),
    }
}

/// What `arugula hook` prints for Claude Code's `PermissionRequest`
/// when the card allows it; `always`: one of its suggestions, as a rule
/// to keep.
pub fn permit_allow(always: Option<&Value>) -> Value {
    let mut decision = json!({ "behavior": "allow" });
    if let Some(s) = always {
        decision["updatedPermissions"] = json!([s]);
    }
    json!({ "hookSpecificOutput": { "hookEventName": "PermissionRequest", "decision": decision } })
}

/// ...and when it denies it, with what the agent reads.
pub fn permit_deny(message: &str) -> Value {
    json!({ "hookSpecificOutput": { "hookEventName": "PermissionRequest",
        "decision": { "behavior": "deny", "message": message } } })
}

impl Ask {
    /// What a notification or a "needs you" list says about it.
    pub fn headline(&self) -> String {
        if self.kind == AskKind::Permission {
            return self.message.clone();
        }
        match self.questions.as_ref().and_then(|q| q.as_array()).and_then(|q| q.first()) {
            Some(q) => q["question"].as_str().unwrap_or(&self.message).to_owned(),
            None => self.message.clone(),
        }
    }

    /// For a push notification that can be answered from its buttons: one
    /// single-select question with one or two options. `{id, field,
    /// options}`.
    pub fn push_choice(&self) -> Option<Value> {
        let qs = self.questions.as_ref()?.as_array()?;
        let [q] = qs.as_slice() else { return None };
        if q["multiSelect"].as_bool() == Some(true) {
            return None;
        }
        let labels: Vec<&str> = q["options"].as_array()?.iter().filter_map(|o| o["label"].as_str()).collect();
        if labels.is_empty() || labels.len() > 2 {
            return None;
        }
        Some(json!({ "id": self.id, "field": "question_0", "options": labels }))
    }
}

/// The question at `n` in AskUserQuestion's field layout, with its "Other"
/// text.
fn picks(content: &Value, n: usize) -> (Vec<String>, Option<String>) {
    let pick = &content[format!("question_{n}")];
    let picks = match pick {
        Value::String(s) if !s.is_empty() => vec![s.clone()],
        Value::Array(a) => a.iter().filter_map(|v| v.as_str()).filter(|s| !s.is_empty()).map(str::to_owned).collect(),
        _ => vec![],
    };
    let custom =
        content[format!("question_{n}_custom")].as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
    (picks, custom)
}

/// One item of a multi-select answer, quoted as Claude's adapter does when
/// it would be ambiguous.
fn item(s: &str) -> String {
    if s.contains(", ") || s.contains('"') { Value::String(s.to_owned()).to_string() } else { s.to_owned() }
}

/// A card's answer as Claude Code's AskUserQuestion takes it: `answers`
/// keyed by question text (option labels; "Other" text alone is the answer;
/// a multi-select's picks and text comma-joined), and `annotations` with
/// the "Other" text next to a single-select pick as a note.
pub fn answers(questions: &Value, content: &Value) -> (Map<String, Value>, Map<String, Value>) {
    let (mut answers, mut notes) = (Map::new(), Map::new());
    for (n, q) in questions.as_array().into_iter().flatten().enumerate() {
        let Some(text) = q["question"].as_str() else { continue };
        let (picks, custom) = picks(content, n);
        let answer = if q["multiSelect"].as_bool() == Some(true) {
            let mut all: Vec<String> = picks.iter().map(|p| item(p)).collect();
            all.extend(custom.as_deref().map(item));
            (!all.is_empty()).then(|| all.join(", "))
        } else {
            match (picks.first(), custom) {
                (Some(p), Some(c)) => {
                    notes.insert(text.to_owned(), json!({ "notes": c }));
                    Some(p.clone())
                }
                (Some(p), None) => Some(p.clone()),
                (None, c) => c,
            }
        };
        if let Some(a) = answer {
            answers.insert(text.to_owned(), Value::String(a));
        }
    }
    (answers, notes)
}

/// What `arugula ask` prints for Claude Code: allow the tool, with the
/// answers in its input, so it never shows its picker.
pub fn hook_output(questions: &Value, content: &Value) -> Value {
    let (answers, annotations) = answers(questions, content);
    let mut input = json!({ "questions": questions, "answers": answers });
    if !annotations.is_empty() {
        input["annotations"] = Value::Object(annotations);
    }
    json!({ "hookSpecificOutput": {
        "hookEventName": "PreToolUse",
        "permissionDecision": "allow",
        "updatedInput": input,
    } })
}

/// What `arugula ask` prints when the card was skipped.
pub fn hook_declined() -> Value {
    json!({ "hookSpecificOutput": {
        "hookEventName": "PreToolUse",
        "permissionDecision": "deny",
        "permissionDecisionReason": "The user did not answer the questions.",
    } })
}

/// An answer in a line, for transcripts and history.
pub fn summary(ask: &Ask, content: &Value) -> String {
    match (&ask.kind, &ask.questions) {
        (AskKind::Questions, Some(qs)) => {
            let (answers, notes) = answers(qs, content);
            if answers.is_empty() {
                return "(no answer)".into();
            }
            answers
                .iter()
                .map(|(q, a)| {
                    let note = notes.get(q).and_then(|n| n["notes"].as_str()).map(|n| format!(" ({n})"));
                    format!("{q} → {}{}", a.as_str().unwrap_or(""), note.unwrap_or_default())
                })
                .collect::<Vec<_>>()
                .join("; ")
        }
        (AskKind::Url, _) => "opened the link".into(),
        _ => match content.as_object() {
            Some(m) if !m.is_empty() => m
                .iter()
                .map(|(k, v)| match v {
                    Value::String(s) => format!("{k}: {s}"),
                    v => format!("{k}: {v}"),
                })
                .collect::<Vec<_>>()
                .join(", "),
            _ => "(no answer)".into(),
        },
    }
}

/// AskUserQuestion's questions, read back from its form when the tool
/// call's input isn't known: `question_<n>` fields with `oneOf` (single) or
/// `items.anyOf` (multi) options.
pub fn questions_from_schema(message: &str, schema: &Value) -> Option<Value> {
    let props = schema["properties"].as_object()?;
    let mut out = vec![];
    for n in 0.. {
        let Some(f) = props.get(&format!("question_{n}")) else { break };
        let multi = f["type"] == "array";
        let opts = if multi { &f["items"]["anyOf"] } else { &f["oneOf"] };
        let options: Vec<Value> = opts
            .as_array()?
            .iter()
            .map(|o| {
                let mut opt = json!({
                    "label": o["const"].as_str().or(o["title"].as_str()).unwrap_or(""),
                    "description": o["description"].as_str().unwrap_or(""),
                });
                if let Some(p) = o["_meta"]["_claude/askUserQuestionOption"]["preview"].as_str() {
                    opt["preview"] = json!(p);
                }
                opt
            })
            .collect();
        let question = f["description"].as_str().unwrap_or(message);
        out.push(json!({
            "question": question,
            "header": f["title"].as_str().unwrap_or(""),
            "multiSelect": multi,
            "options": options,
        }));
    }
    (!out.is_empty()).then_some(Value::Array(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn three() -> Value {
        json!([
            { "question": "Which colour do you prefer?", "header": "Colour", "multiSelect": false,
              "options": [{ "label": "Red", "description": "warm" }, { "label": "Blue", "description": "cool" }] },
            { "question": "Which fruits do you like?", "header": "Fruit", "multiSelect": true,
              "options": [{ "label": "Apple" }, { "label": "Pear" }, { "label": "Plum" }] },
            { "question": "Which pet do you prefer?", "header": "Pet", "multiSelect": false,
              "options": [{ "label": "Cat" }, { "label": "Dog" }] },
        ])
    }

    /// The answers S13 recorded from claude-agent-acp for the same content.
    #[test]
    fn answers_as_claude_takes_them() {
        let content = json!({ "question_0": "Red", "question_0_custom": "dark red please",
            "question_1": ["Apple", "Plum"], "question_1_custom": "kiwi", "question_2_custom": "a parrot" });
        let out = hook_output(&three(), &content);
        let input = &out["hookSpecificOutput"]["updatedInput"];
        assert_eq!(out["hookSpecificOutput"]["permissionDecision"], "allow");
        assert_eq!(input["questions"], three());
        assert_eq!(
            input["answers"],
            json!({ "Which colour do you prefer?": "Red", "Which fruits do you like?": "Apple, Plum, kiwi",
                    "Which pet do you prefer?": "a parrot" })
        );
        assert_eq!(input["annotations"], json!({ "Which colour do you prefer?": { "notes": "dark red please" } }));
        // Unanswered questions aren't in it; ambiguous items are quoted.
        let (a, n) = answers(&three(), &json!({ "question_1": ["Apple", "a, b"], "question_1_custom": " " }));
        assert_eq!(Value::Object(a), json!({ "Which fruits do you like?": "Apple, \"a, b\"" }));
        assert!(n.is_empty());
    }

    #[test]
    fn summaries_and_notification_choices() {
        let ask = Ask {
            id: "0".into(),
            kind: AskKind::Questions,
            message: "Please answer the following questions.".into(),
            questions: Some(three()),
            schema: None,
            url: None,
            accepted: false,
            tool_call_id: None,
            source: "agent".into(),
            agent: None,
            at_ms: 0,
            tool: None,
            input: None,
            suggestions: None,
            session: None,
        };
        assert_eq!(ask.headline(), "Which colour do you prefer?");
        assert_eq!(ask.push_choice(), None, "three questions open the block");
        let s = summary(&ask, &json!({ "question_0": "Red", "question_0_custom": "dark" }));
        assert_eq!(s, "Which colour do you prefer? → Red (dark)");
        let one = Ask { questions: Some(json!([three()[0]])), ..ask.clone() };
        assert_eq!(one.push_choice(), Some(json!({ "id": "0", "field": "question_0", "options": ["Red", "Blue"] })));
        let multi = Ask { questions: Some(json!([three()[1]])), ..ask.clone() };
        assert_eq!(multi.push_choice(), None);
        let form = Ask { kind: AskKind::Form, questions: None, ..ask };
        assert_eq!(summary(&form, &json!({ "size": "M", "qty": 2 })), "qty: 2, size: M");
    }

    /// S18's recorded `PermissionRequest` inputs, and the replies Claude
    /// Code took.
    #[test]
    fn permission_cards_and_replies() {
        let fixture = |n: &str| -> Value {
            let p = format!("{}/../daemon/tests/fixtures/{n}", env!("CARGO_MANIFEST_DIR"));
            serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
        };
        let p = fixture("s18-hook-permission.json");
        assert_eq!(permission_message(p["tool_name"].as_str().unwrap(), &p["tool_input"]), "Bash: touch a.txt");
        let e = fixture("s18-hook-permission-accept-edits.json");
        let always = &e["permission_suggestions"][0];
        let out = permit_allow(Some(always));
        assert_eq!(out["hookSpecificOutput"]["decision"]["behavior"], "allow");
        assert_eq!(out["hookSpecificOutput"]["decision"]["updatedPermissions"][0]["rules"][0]["toolName"], "Bash");
        assert!(permit_allow(None)["hookSpecificOutput"]["decision"].get("updatedPermissions").is_none());
        let no = permit_deny("Sam said no");
        assert_eq!(no["hookSpecificOutput"]["decision"], json!({ "behavior": "deny", "message": "Sam said no" }));
        assert_eq!(
            permission_message("Edit", &json!({ "file_path": "src/a.rs", "old_string": "x" })),
            "Edit: src/a.rs"
        );
        assert_eq!(permission_message("WebSearch", &json!({})), "WebSearch");
    }

    #[test]
    fn questions_read_back_from_the_form() {
        let schema = json!({ "type": "object", "properties": {
            "question_0": { "type": "string", "title": "Colour", "description": "Which colour?", "oneOf": [
                { "const": "Red", "title": "Red", "description": "warm",
                  "_meta": { "_claude/askUserQuestionOption": { "preview": "┌─┐" } } },
                { "const": "Blue", "title": "Blue", "description": "cool" }] },
            "question_0_custom": { "type": "string", "title": "Other" },
            "question_1": { "type": "array", "title": "Fruit", "items": { "anyOf": [{ "const": "Apple", "title": "Apple" }] } },
        } });
        let qs = questions_from_schema("Please answer", &schema).unwrap();
        assert_eq!(qs[0]["question"], "Which colour?");
        assert_eq!(qs[0]["options"][0]["preview"], "┌─┐");
        assert_eq!(qs[1]["question"], "Please answer");
        assert_eq!(qs[1]["multiSelect"], true);
        assert!(questions_from_schema("x", &json!({ "properties": { "size": {} } })).is_none());
    }
}
