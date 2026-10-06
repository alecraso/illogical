//! An agent run as a list of entries (your prompts, the agent's messages and
//! thoughts, its tool calls), built from ACP `session/update`s, and its
//! Markdown rendering for `capture --text`, history and search.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Output kept per tool call in the state clients get (all of it is in the
/// log and in `capture --text`).
pub const OUTPUT_IN_STATE: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
// Tool calls are most of a transcript anyway; boxing them buys nothing.
#[allow(clippy::large_enum_variant)]
pub enum Entry {
    /// What you sent.
    User {
        text: String,
        /// Images sent with it, by name in the block's folder (M71; see
        /// [`super::images`]).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<String>,
        at_ms: u64,
        #[serde(default, skip_serializing_if = "is_false")]
        forgotten: bool,
    },
    /// The agent's reply, streamed in chunks.
    Agent {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(default, skip_serializing_if = "is_false")]
        forgotten: bool,
    },
    Thought {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(default, skip_serializing_if = "is_false")]
        forgotten: bool,
    },
    Tool(Tool),
    /// Something the block itself says: the agent stopped, a request was
    /// answered by a rule, a turn ended in an error.
    Note {
        text: String,
        at_ms: u64,
        #[serde(default, skip_serializing_if = "is_false")]
        forgotten: bool,
    },
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tool {
    pub id: String,
    /// The agent's name for the tool (`AskUserQuestion`), when it says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub title: String,
    /// ACP's kind: execute, edit, read, fetch, think, …
    pub kind: String,
    /// pending, in_progress, completed or failed.
    pub status: String,
    /// The command line, for commands.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// What the command printed (`_meta.terminal_output`), ANSI and all.
    pub output: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit: Option<i32>,
    /// Text content the agent attached (a description, a diff, an error).
    pub text: String,
    /// Paths it touches (on the agent's machine, or in its sandbox).
    pub locations: Vec<String>,
    /// AskUserQuestion's questions (its `rawInput.questions`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub questions: Option<Value>,
    pub started_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_ms: Option<u64>,
    /// An opened conversation's entry that isn't on the branch Continue
    /// resumes (#79): the agent doesn't remember it.
    #[serde(skip_serializing_if = "is_false")]
    pub forgotten: bool,
}

impl Entry {
    /// Not on the branch a resume follows (#79).
    #[cfg(test)]
    pub fn forgotten(&self) -> bool {
        match self {
            Entry::User { forgotten, .. }
            | Entry::Agent { forgotten, .. }
            | Entry::Thought { forgotten, .. }
            | Entry::Note { forgotten, .. } => *forgotten,
            Entry::Tool(t) => t.forgotten,
        }
    }

    pub fn forget(&mut self) {
        match self {
            Entry::User { forgotten, .. }
            | Entry::Agent { forgotten, .. }
            | Entry::Thought { forgotten, .. }
            | Entry::Note { forgotten, .. } => *forgotten = true,
            Entry::Tool(t) => t.forgotten = true,
        }
    }
}

impl Tool {
    pub fn finished(&self) -> bool {
        self.status == "completed" || self.status == "failed"
    }

    /// What history calls it: the command, else its title (a question
    /// tool: its first question).
    pub fn label(&self) -> String {
        if let Some(q) = self.first_question() {
            return format!("asked: {q}");
        }
        self.command.clone().unwrap_or_else(|| self.title.clone())
    }

    pub fn is_question(&self) -> bool {
        self.name.as_deref() == Some(illogical_proto::ask::ASK_USER_QUESTION)
    }

    fn first_question(&self) -> Option<&str> {
        self.questions.as_ref()?.get(0)?["question"].as_str()
    }
}

#[derive(Debug, Clone, Default)]
pub struct Transcript {
    pub entries: Vec<Entry>,
    tools: HashMap<String, usize>,
}

/// What an update did, for whoever keeps history.
#[derive(Debug, Clone, PartialEq)]
pub enum Applied {
    Nothing,
    /// A tool call reached `completed` or `failed`.
    ToolFinished(String),
}

fn text_of(content: &Value) -> Option<&str> {
    match content["type"].as_str() {
        Some("text") => content["text"].as_str(),
        _ => None,
    }
}

impl Transcript {
    /// A transcript made elsewhere (an imported conversation, M33).
    pub fn from_entries(entries: Vec<Entry>) -> Self {
        let tools = entries
            .iter()
            .enumerate()
            .filter_map(|(i, e)| match e {
                Entry::Tool(t) => Some((t.id.clone(), i)),
                _ => None,
            })
            .collect();
        Self { entries, tools }
    }

    pub fn user(&mut self, text: &str, images: Vec<String>, at_ms: u64) {
        self.entries.push(Entry::User { text: text.to_owned(), images, at_ms, forgotten: false });
    }

    pub fn note(&mut self, text: impl Into<String>, at_ms: u64) {
        self.entries.push(Entry::Note { text: text.into(), at_ms, forgotten: false });
    }

    pub fn tool(&self, id: &str) -> Option<&Tool> {
        match self.entries.get(*self.tools.get(id)?) {
            Some(Entry::Tool(t)) => Some(t),
            _ => None,
        }
    }

    /// The tool running now, if any.
    pub fn current_tool(&self) -> Option<&Tool> {
        self.entries.iter().rev().find_map(|e| match e {
            Entry::Tool(t) if !t.finished() => Some(t),
            _ => None,
        })
    }

    /// One `session/update`'s `update` object.
    pub fn apply(&mut self, u: &Value, at_ms: u64) -> Applied {
        match u["sessionUpdate"].as_str().unwrap_or("") {
            "agent_message_chunk" => self.chunk(u, at_ms, false),
            "agent_thought_chunk" => self.chunk(u, at_ms, true),
            "user_message_chunk" => {
                if let Some(t) = text_of(&u["content"]) {
                    match self.entries.last_mut() {
                        Some(Entry::User { text, .. }) => text.push_str(t),
                        _ => self.user(t, vec![], at_ms),
                    }
                }
                Applied::Nothing
            }
            "tool_call" | "tool_call_update" => self.tool_update(u, at_ms),
            _ => Applied::Nothing,
        }
    }

    fn chunk(&mut self, u: &Value, _at_ms: u64, thought: bool) -> Applied {
        let Some(t) = text_of(&u["content"]) else { return Applied::Nothing };
        let id = u["messageId"].as_str().map(str::to_owned);
        let same = |eid: &Option<String>| id.is_none() || *eid == id;
        match self.entries.last_mut() {
            Some(Entry::Agent { text, id: eid, .. }) if !thought && same(eid) => text.push_str(t),
            Some(Entry::Thought { text, id: eid, .. }) if thought && same(eid) => text.push_str(t),
            _ if thought => self.entries.push(Entry::Thought { text: t.to_owned(), id, forgotten: false }),
            _ => self.entries.push(Entry::Agent { text: t.to_owned(), id, forgotten: false }),
        }
        Applied::Nothing
    }

    fn tool_update(&mut self, u: &Value, at_ms: u64) -> Applied {
        let Some(id) = u["toolCallId"].as_str() else { return Applied::Nothing };
        let i = match self.tools.get(id) {
            Some(i) => *i,
            None => {
                self.entries.push(Entry::Tool(Tool {
                    id: id.to_owned(),
                    status: "pending".into(),
                    started_ms: at_ms,
                    ..Default::default()
                }));
                self.tools.insert(id.to_owned(), self.entries.len() - 1);
                self.entries.len() - 1
            }
        };
        let Some(Entry::Tool(t)) = self.entries.get_mut(i) else { return Applied::Nothing };
        let was_finished = t.finished();
        merge_tool(t, u);
        if t.finished() && !was_finished {
            t.ended_ms = Some(at_ms);
            return Applied::ToolFinished(id.to_owned());
        }
        Applied::Nothing
    }

    /// Fold in a transcript an agent replayed (`session/load`), which may
    /// have more than we saw: tool calls and messages by id take the
    /// replay's version, new ones are added. Your own prompts come from our
    /// log, not the replay (Fountain leaves them out).
    pub fn merge(&mut self, replay: Transcript) {
        for e in replay.entries {
            match e {
                Entry::User { .. } => {}
                Entry::Tool(t) => match self.tools.get(&t.id) {
                    Some(&i) => self.entries[i] = Entry::Tool(t),
                    None => {
                        self.tools.insert(t.id.clone(), self.entries.len());
                        self.entries.push(Entry::Tool(t));
                    }
                },
                Entry::Agent { ref id, ref text, .. } | Entry::Thought { ref id, ref text, .. } => {
                    let thought = matches!(e, Entry::Thought { .. });
                    let found = self.entries.iter_mut().find(|x| match (x, thought) {
                        (Entry::Agent { id: xi, text: xt, .. }, false)
                        | (Entry::Thought { id: xi, text: xt, .. }, true) => {
                            if id.is_some() {
                                xi == id
                            } else {
                                xt == text
                            }
                        }
                        _ => false,
                    });
                    match found {
                        Some(Entry::Agent { text: xt, .. } | Entry::Thought { text: xt, .. }) => {
                            if text.len() > xt.len() {
                                *xt = text.clone();
                            }
                        }
                        _ => self.entries.push(e),
                    }
                }
                Entry::Note { .. } => self.entries.push(e),
            }
        }
    }

    /// The transcript as Markdown: `capture --text`.
    pub fn markdown(&self) -> String {
        let mut out = String::new();
        for e in &self.entries {
            match e {
                Entry::User { text, images, .. } => {
                    out.push_str("## You\n\n");
                    out.push_str(text.trim_end());
                    for i in images {
                        out.push_str(&format!("\n\n[image {i}]"));
                    }
                    out.push_str("\n\n");
                }
                Entry::Agent { text, .. } => {
                    out.push_str(text.trim_end());
                    out.push_str("\n\n");
                }
                Entry::Thought { text, .. } => {
                    for line in text.trim_end().lines() {
                        out.push_str("> ");
                        out.push_str(line);
                        out.push('\n');
                    }
                    out.push('\n');
                }
                Entry::Tool(t) => {
                    let exit = t.exit.map(|e| format!(", exit {e}")).unwrap_or_default();
                    out.push_str(&format!("**{}** `{}` ({}{exit})\n", tool_kind(t), t.title, t.status));
                    if let Some(c) = &t.command
                        && *c != t.title
                    {
                        out.push_str(&format!("\n    $ {c}\n"));
                    }
                    for q in t.questions.as_ref().and_then(Value::as_array).into_iter().flatten() {
                        let options: Vec<&str> =
                            q["options"].as_array().into_iter().flatten().filter_map(|o| o["label"].as_str()).collect();
                        let header = q["header"].as_str().filter(|h| !h.is_empty()).map(|h| format!("{h}: "));
                        let many = if q["multiSelect"].as_bool() == Some(true) { ", any of" } else { "" };
                        out.push_str(&format!(
                            "\n- {}{} ({}{many})",
                            header.unwrap_or_default(),
                            q["question"].as_str().unwrap_or(""),
                            options.join(" / ")
                        ));
                    }
                    if t.questions.is_some() {
                        out.push('\n');
                    }
                    let body =
                        if t.output.is_empty() { t.text.clone() } else { crate::osc::strip(t.output.as_bytes()) };
                    if body.contains("```") {
                        // Already fenced (Fountain's ```console blocks).
                        out.push('\n');
                        out.push_str(body.trim_end());
                        out.push('\n');
                    } else if !body.trim().is_empty() {
                        out.push_str("\n```\n");
                        out.push_str(body.trim_end());
                        out.push_str("\n```\n");
                    }
                    out.push('\n');
                }
                Entry::Note { text, .. } => {
                    out.push_str(&format!("_{}_\n\n", text.trim_end()));
                }
            }
        }
        out
    }

    /// For the state clients get: the last `max` entries, with long tool
    /// output cut to its end.
    pub fn for_state(&self, max: usize) -> (usize, Vec<Entry>) {
        let skip = self.entries.len().saturating_sub(max);
        let entries = self.entries[skip..]
            .iter()
            .map(|e| match e {
                Entry::Tool(t) if t.output.len() > OUTPUT_IN_STATE => {
                    let mut t = t.clone();
                    let mut cut = t.output.len() - OUTPUT_IN_STATE;
                    while !t.output.is_char_boundary(cut) {
                        cut += 1;
                    }
                    t.output = format!("…\r\n{}", &t.output[cut..]);
                    Entry::Tool(t)
                }
                e => e.clone(),
            })
            .collect();
        (skip, entries)
    }
}

fn tool_kind(t: &Tool) -> &str {
    if t.is_question() {
        return "Asked";
    }
    match t.kind.as_str() {
        "execute" => "Ran",
        "edit" => "Edited",
        "read" => "Read",
        "delete" => "Deleted",
        "move" => "Moved",
        "search" => "Searched",
        "fetch" => "Fetched",
        "think" => "Thought",
        _ => "Tool",
    }
}

/// Fold one `tool_call`/`tool_call_update` into a tool: every field it has
/// replaces ours, and terminal output accumulates.
fn merge_tool(t: &mut Tool, u: &Value) {
    if let Some(s) = u["title"].as_str()
        // "Terminal" is the placeholder before the command is known.
        && !(s == "Terminal" && !t.title.is_empty())
    {
        t.title = s.to_owned();
    }
    if let Some(s) = u["kind"].as_str() {
        t.kind = s.to_owned();
    }
    if let Some(s) = u["name"].as_str().or(u["_meta"]["claudeCode"]["toolName"].as_str()) {
        t.name = Some(s.to_owned());
    }
    if let Some(q) = u["rawInput"]["questions"].as_array().filter(|q| !q.is_empty()) {
        t.questions = Some(Value::Array(q.clone()));
    }
    if let Some(s) = u["status"].as_str() {
        t.status = s.to_owned();
    }
    if let Some(c) = u["rawInput"]["command"].as_str() {
        t.command = Some(c.to_owned());
    } else if let Some(c) = u["rawInput"]["command"].as_array() {
        // codex: an argv
        t.command = Some(c.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" "));
    }
    if let Some(locs) = u["locations"].as_array() {
        t.locations = locs.iter().filter_map(|l| l["path"].as_str().map(str::to_owned)).collect();
    }
    if let Some(locs) = u["_meta"]["fountain.sandboxLocations"].as_array() {
        t.locations = locs.iter().filter_map(|l| l["path"].as_str().or(l.as_str()).map(str::to_owned)).collect();
    }
    if let Some(content) = u["content"].as_array() {
        let texts: Vec<String> = content
            .iter()
            .filter_map(|c| match c["type"].as_str() {
                Some("content") => text_of(&c["content"]).map(str::to_owned),
                Some("diff") => {
                    let path = c["path"].as_str().unwrap_or("");
                    Some(format!("diff {path}\n{}", c["newText"].as_str().unwrap_or("")))
                }
                _ => None,
            })
            .collect();
        if !texts.is_empty() {
            t.text = texts.join("\n");
        }
    }
    let meta = &u["_meta"];
    if let Some(d) = meta["terminal_output"]["data"].as_str() {
        t.output.push_str(d);
    }
    if let Some(d) = meta["terminal_output_delta"]["data"].as_str() {
        t.output.push_str(d);
    }
    if let Some(code) = meta["terminal_exit"]["exit_code"].as_i64() {
        t.exit = Some(code as i32);
    }
    // Without terminal output, a failure's reason is its raw output.
    if t.output.is_empty()
        && t.text.is_empty()
        && let Some(s) = u["rawOutput"].as_str()
    {
        t.text = s.to_owned();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn chunks_tools_and_markdown() {
        let mut t = Transcript::default();
        t.user("run ls", vec![], 1);
        t.apply(&json!({"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"Let me "}}), 2);
        t.apply(&json!({"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"look."}}), 2);
        t.apply(
            &json!({"sessionUpdate":"tool_call","toolCallId":"t1","title":"Terminal","kind":"execute","status":"pending"}),
            3,
        );
        t.apply(
            &json!({"sessionUpdate":"tool_call_update","toolCallId":"t1","title":"ls","rawInput":{"command":"ls"}}),
            3,
        );
        t.apply(
            &json!({"sessionUpdate":"tool_call_update","toolCallId":"t1","_meta":{"terminal_output":{"data":"\u{1b}[1mx.txt\u{1b}[0m"}}}),
            4,
        );
        let done = t.apply(
            &json!({"sessionUpdate":"tool_call_update","toolCallId":"t1","status":"completed","_meta":{"terminal_exit":{"exit_code":0}}}),
            5,
        );
        assert_eq!(done, Applied::ToolFinished("t1".into()));
        t.apply(
            &json!({"sessionUpdate":"agent_message_chunk","messageId":"m1","content":{"type":"text","text":"x."}}),
            6,
        );
        t.apply(
            &json!({"sessionUpdate":"agent_message_chunk","messageId":"m1","content":{"type":"text","text":"txt"}}),
            6,
        );
        assert_eq!(t.entries.len(), 4);
        let tool = t.tool("t1").unwrap();
        assert_eq!((tool.title.as_str(), tool.command.as_deref(), tool.exit), ("ls", Some("ls"), Some(0)));
        assert_eq!(tool.ended_ms, Some(5));
        let md = t.markdown();
        assert!(md.contains("## You\n\nrun ls"), "{md}");
        assert!(md.contains("> Let me look."), "{md}");
        assert!(md.contains("**Ran** `ls` (completed, exit 0)\n\n```\nx.txt\n```"), "{md}");
        assert!(md.ends_with("x.txt\n\n"), "{md}");
        assert!(t.current_tool().is_none());
    }

    #[test]
    fn a_replay_merges_by_id() {
        let mut ours = Transcript::default();
        ours.user("hi", vec![], 1);
        ours.apply(
            &json!({"sessionUpdate":"agent_message_chunk","messageId":"m1","content":{"type":"text","text":"Hel"}}),
            2,
        );
        ours.apply(
            &json!({"sessionUpdate":"tool_call","toolCallId":"t1","title":"sleep 20","status":"in_progress"}),
            3,
        );
        let mut replay = Transcript::default();
        for u in [
            json!({"sessionUpdate":"agent_message_chunk","messageId":"m1","content":{"type":"text","text":"Hello"}}),
            json!({"sessionUpdate":"tool_call","toolCallId":"t1","title":"sleep 20","status":"completed"}),
            json!({"sessionUpdate":"agent_message_chunk","messageId":"m2","content":{"type":"text","text":"Done."}}),
        ] {
            replay.apply(&u, 9);
        }
        ours.merge(replay);
        assert_eq!(ours.entries.len(), 4);
        assert!(matches!(&ours.entries[1], Entry::Agent { text, .. } if text == "Hello"));
        assert_eq!(ours.tool("t1").unwrap().status, "completed");
        assert!(matches!(&ours.entries[3], Entry::Agent { text, .. } if text == "Done."));
    }
}
