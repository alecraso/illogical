//! A Claude Code transcript (`~/.claude/projects/<slug>/<id>.jsonl`) as an
//! agent block's entries (M33).
//!
//! **File order, not the `parentUuid` chain** (S20): the chain isn't a clean
//! tree (compactions re-link their preserved tail, `away_summary` lines
//! parent the next prompt, parallel tool calls branch), and walking it drops
//! real exchanges. In file order a tool result never comes before its call.
//! A prompt sent again from an earlier point (a rewind) gets a note.
//!
//! Lines and blocks this doesn't know are skipped, so a newer Claude Code
//! shows less rather than breaking.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use crate::agent::transcript::{Entry, Tool};

/// The entries of a transcript. `finished`: nothing is running it, so a tool
/// call that never got its result was cut off.
pub fn entries(jsonl: &[u8], finished: bool) -> Vec<Entry> {
    let mut c = Converter::default();
    for line in jsonl.split(|b| *b == b'\n') {
        if line.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_slice::<Value>(line) {
            c.line(&v);
        }
    }
    if finished {
        for e in &mut c.out {
            if let Entry::Tool(t) = e
                && !t.finished()
            {
                t.status = "failed".into();
                if t.text.is_empty() && t.output.is_empty() {
                    t.text = "(no result)".into();
                }
            }
        }
    }
    c.out
}

#[derive(Default)]
struct Converter {
    out: Vec<Entry>,
    tools: HashMap<String, usize>,
    /// Lines that already have a user/assistant child: a prompt under one
    /// of these was sent again from an earlier point.
    parents: HashSet<String>,
}

impl Converter {
    fn line(&mut self, v: &Value) {
        if v["isSidechain"] == true {
            return;
        }
        let at = at_ms(v["timestamp"].as_str().unwrap_or(""));
        match v["type"].as_str().unwrap_or("") {
            "user" => self.user(v, at),
            "assistant" => self.assistant(v, at),
            "system" if v["subtype"] == "compact_boundary" => self.note("Conversation compacted", at),
            _ => {}
        }
        if matches!(v["type"].as_str(), Some("user" | "assistant"))
            && let Some(p) = v["parentUuid"].as_str()
        {
            self.parents.insert(p.to_owned());
        }
    }

    fn note(&mut self, text: impl Into<String>, at_ms: u64) {
        self.out.push(Entry::Note { text: text.into(), at_ms });
    }

    fn prompt(&mut self, v: &Value, text: String, at: u64) {
        if v["parentUuid"].as_str().is_some_and(|p| self.parents.contains(p)) {
            self.note("Rewound: the prompt below was sent again from an earlier point", at);
        }
        self.out.push(Entry::User { text, at_ms: at });
    }

    fn user(&mut self, v: &Value, at: u64) {
        if v["isMeta"] == true || v["isCompactSummary"] == true {
            return;
        }
        match &v["message"]["content"] {
            Value::String(s) => {
                if let Some(cmd) = command(s) {
                    // The adapter's own model switches, on every resume (S20).
                    if !(v["entrypoint"] == "sdk-ts" && cmd.starts_with("/model")) {
                        self.note(cmd, at);
                    }
                    return;
                }
                if is_command_output(s) {
                    return;
                }
                let text = strip_reminders(s);
                if !text.is_empty() {
                    self.prompt(v, text, at);
                }
            }
            Value::Array(blocks) => {
                let mut text = vec![];
                for b in blocks {
                    match b["type"].as_str() {
                        Some("text") => {
                            let t = b["text"].as_str().unwrap_or("");
                            if t.starts_with("[Request interrupted by user") {
                                self.note("Interrupted", at);
                            } else if !is_command_output(t) {
                                let t = strip_reminders(t);
                                if !t.is_empty() {
                                    text.push(t);
                                }
                            }
                        }
                        Some("image") => text.push("[image]".into()),
                        Some("tool_result") => self.result(b, at),
                        _ => {}
                    }
                }
                if !text.is_empty() {
                    self.prompt(v, text.join("\n"), at);
                }
            }
            _ => {}
        }
    }

    fn result(&mut self, b: &Value, at: u64) {
        let Some(&i) = b["tool_use_id"].as_str().and_then(|id| self.tools.get(id)) else { return };
        let Some(Entry::Tool(t)) = self.out.get_mut(i) else { return };
        let text = match &b["content"] {
            Value::String(s) => s.clone(),
            Value::Array(parts) => parts
                .iter()
                .filter_map(|p| match p["type"].as_str() {
                    Some("text") => p["text"].as_str().map(str::to_owned),
                    Some("image") => Some("[image]".into()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        };
        t.status = if b["is_error"] == true { "failed" } else { "completed" }.into();
        t.ended_ms = Some(at);
        if t.kind == "execute" {
            t.output = text;
        } else if t.text.is_empty() || b["is_error"] == true || t.is_question() {
            t.text = text;
        }
    }

    fn assistant(&mut self, v: &Value, at: u64) {
        let m = &v["message"];
        let id = m["id"].as_str().map(str::to_owned);
        if v["isApiErrorMessage"] == true {
            let text: Vec<&str> =
                m["content"].as_array().into_iter().flatten().filter_map(|b| b["text"].as_str()).collect();
            self.note(format!("API error: {}", text.join(" ")), at);
            return;
        }
        for b in m["content"].as_array().into_iter().flatten() {
            match b["type"].as_str() {
                Some("text") => {
                    let t = b["text"].as_str().unwrap_or("");
                    if t.is_empty() {
                        continue;
                    }
                    match self.out.last_mut() {
                        Some(Entry::Agent { text, id: last }) if *last == id && id.is_some() => {
                            text.push_str("\n\n");
                            text.push_str(t);
                        }
                        _ => self.out.push(Entry::Agent { text: t.to_owned(), id: id.clone() }),
                    }
                }
                // Often only a signature: nothing to show.
                Some("thinking") => {
                    let t = b["thinking"].as_str().unwrap_or("").trim();
                    if !t.is_empty() {
                        self.out.push(Entry::Thought { text: t.to_owned(), id: id.clone() });
                    }
                }
                Some("tool_use") => {
                    let tool = tool(b, at);
                    self.tools.insert(tool.id.clone(), self.out.len());
                    self.out.push(Entry::Tool(tool));
                }
                _ => {}
            }
        }
    }
}

/// A `tool_use` block as ACP's adapter would show it.
fn tool(b: &Value, at: u64) -> Tool {
    let name = b["name"].as_str().unwrap_or("tool").to_owned();
    let input = &b["input"];
    let s = |k: &str| input[k].as_str().map(str::to_owned);
    let path = s("file_path").or(s("notebook_path")).or(s("path"));
    let (kind, title) = match name.as_str() {
        "Bash" | "BashOutput" | "KillShell" | "Monitor" => {
            ("execute", s("command").or(s("description")).unwrap_or_else(|| name.clone()))
        }
        "Read" => ("read", path.clone().map(|p| format!("Read {p}")).unwrap_or_else(|| name.clone())),
        "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => {
            ("edit", path.clone().map(|p| format!("{name} {p}")).unwrap_or_else(|| name.clone()))
        }
        "Grep" | "Glob" | "LS" | "ToolSearch" => {
            ("search", s("pattern").or(s("query")).map(|p| format!("{name} {p}")).unwrap_or_else(|| name.clone()))
        }
        "WebFetch" => ("fetch", s("url").unwrap_or_else(|| name.clone())),
        "WebSearch" => ("fetch", s("query").map(|q| format!("Search: {q}")).unwrap_or_else(|| name.clone())),
        "Task" | "Agent" => ("think", s("description").unwrap_or_else(|| name.clone())),
        _ => ("other", s("description").map(|d| format!("{name}: {d}")).unwrap_or_else(|| name.clone())),
    };
    let text = match name.as_str() {
        "Edit" => format!("diff {}\n{}", path.clone().unwrap_or_default(), s("new_string").unwrap_or_default()),
        "Write" => format!("diff {}\n{}", path.clone().unwrap_or_default(), s("content").unwrap_or_default()),
        _ => String::new(),
    };
    Tool {
        id: b["id"].as_str().unwrap_or_default().to_owned(),
        name: Some(name.clone()),
        title,
        kind: kind.into(),
        status: "pending".into(),
        command: if kind == "execute" { s("command") } else { None },
        text,
        locations: path.into_iter().collect(),
        questions: input["questions"].as_array().filter(|q| !q.is_empty()).map(|q| Value::Array(q.clone())),
        started_ms: at,
        ..Default::default()
    }
}

/// A slash command the CLI recorded (`<command-name>/model</command-name>
/// … <command-args>haiku</command-args>`), as `/model haiku`.
fn command(s: &str) -> Option<String> {
    let t = s.trim_start();
    let t = t
        .strip_prefix("<local-command-caveat>")
        .map(|r| r.split_once("</local-command-caveat>").map_or(r, |x| x.1))
        .unwrap_or(t);
    let t = t.trim_start();
    if !t.starts_with("<command-name>") && !t.starts_with("<command-message>") {
        return None;
    }
    let tag = |name: &str| {
        let open = format!("<{name}>");
        let close = format!("</{name}>");
        let start = t.find(&open)? + open.len();
        let end = t[start..].find(&close)? + start;
        Some(t[start..end].trim().to_owned())
    };
    let name = tag("command-name").or_else(|| tag("command-message").map(|m| format!("/{m}")))?;
    let args = tag("command-args").filter(|a| !a.is_empty());
    Some(match args {
        Some(a) => format!("{name} {a}"),
        None => name,
    })
}

fn is_command_output(s: &str) -> bool {
    let t = s.trim_start();
    ["<local-command-stdout>", "<local-command-stderr>", "<local-command-caveat>", "<bash-stdout>", "<bash-stderr>"]
        .iter()
        .any(|p| t.starts_with(p))
}

/// Text without leading `<system-reminder>` blocks (the desktop app puts one
/// on a session's first prompt).
pub fn strip_reminders(s: &str) -> String {
    let mut t = s.trim_start();
    while let Some(rest) = t.strip_prefix("<system-reminder>") {
        match rest.find("</system-reminder>") {
            Some(end) => t = rest[end + "</system-reminder>".len()..].trim_start(),
            None => return String::new(),
        }
    }
    t.trim_end().to_owned()
}

/// An RFC 3339 UTC time (`2026-10-02T20:50:52.469Z`) in ms since the epoch;
/// 0 if it isn't one.
pub fn at_ms(s: &str) -> u64 {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[10] != b'T' {
        return 0;
    }
    let n = |r: std::ops::Range<usize>| s.get(r).and_then(|x| x.parse::<i64>().ok());
    let (Some(y), Some(mo), Some(d), Some(h), Some(mi), Some(sec)) =
        (n(0..4), n(5..7), n(8..10), n(11..13), n(14..16), n(17..19))
    else {
        return 0;
    };
    let ms = s.get(19..).and_then(|r| r.strip_prefix('.')).map_or(0, |r| {
        let digits: String = r.chars().take_while(char::is_ascii_digit).take(3).collect();
        format!("{digits:0<3}").parse::<i64>().unwrap_or(0)
    });
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if mo > 2 { mo - 3 } else { mo + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let t = ((days * 24 + h) * 60 + mi) * 60 + sec;
    u64::try_from(t * 1000 + ms).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        let p = format!("{}/tests/fixtures/conversations/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read(&p).unwrap_or_else(|e| panic!("{p}: {e}"))
    }

    fn tools(e: &[Entry]) -> Vec<&Tool> {
        e.iter()
            .filter_map(|e| match e {
                Entry::Tool(t) => Some(t),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn times() {
        assert_eq!(at_ms("1970-01-01T00:00:00Z"), 0);
        assert_eq!(at_ms("2026-10-02T20:50:52.469Z"), 1_790_974_252_469);
        assert_eq!(at_ms("2000-03-01T00:00:01.5Z"), 951_868_801_500);
        assert_eq!(at_ms("nonsense"), 0);
    }

    #[test]
    fn the_scratch_session() {
        // S20's own: a CLI session, turns through the adapter, then the
        // CLI's branch (Q5).
        let e = entries(&fixture("scratch/a683c96a-c2b1-4ed7-bdd4-51b7d759125b.jsonl"), true);
        let prompts: Vec<&str> = e
            .iter()
            .filter_map(|e| match e {
                Entry::User { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(prompts[0].starts_with("Remember the number 4817"), "{prompts:?}");
        assert!(prompts[1].starts_with("New secret"), "{prompts:?}");
        assert!(prompts.iter().any(|p| p.contains("vault code is 2468")));
        assert!(prompts.iter().any(|p| p.contains("lighthouse")));
        // The adapter's /model switches aren't shown.
        assert!(!e.iter().any(|e| matches!(e, Entry::Note { text, .. } if text.starts_with("/model"))), "{e:#?}");
        assert!(!prompts.iter().any(|p| p.contains("command-name")));
        let bash = tools(&e).into_iter().find(|t| t.kind == "execute").expect("a Bash call");
        assert_eq!(bash.command.as_deref(), Some("echo hello-s20 > note.txt && cat note.txt"));
        assert_eq!(bash.status, "completed");
        assert!(bash.output.contains("hello-s20"), "{bash:?}");
        assert!(e.iter().any(|e| matches!(e, Entry::Agent { text, .. } if text.contains("Odalys Brenner"))));
        // The CLI's branch, sent from an earlier point than the adapter's turns.
        assert!(e.iter().any(|e| matches!(e, Entry::Note { text, .. } if text.starts_with("Rewound"))), "{e:#?}");
    }

    #[test]
    fn tool_calls_and_their_results() {
        let e = entries(&fixture("shapes/bash.jsonl"), true);
        let t = tools(&e);
        assert_eq!(t.len(), 1);
        assert_eq!((t[0].kind.as_str(), t[0].status.as_str()), ("execute", "completed"));
        assert!(!t[0].output.is_empty());

        let e = entries(&fixture("shapes/edit.jsonl"), true);
        let t = tools(&e)[0];
        assert_eq!((t.kind.as_str(), t.name.as_deref()), ("edit", Some("Edit")));
        assert_eq!(t.locations, vec!["/work/project/file.rs"]);
        assert!(t.text.starts_with("diff /work/project/file.rs\n"));

        let e = entries(&fixture("shapes/ask-user-question.jsonl"), true);
        let t = tools(&e)[0];
        assert!(t.is_question());
        assert!(t.questions.as_ref().and_then(Value::as_array).is_some_and(|q| !q.is_empty()));
        assert_eq!(t.status, "completed");

        // Parallel calls: every call gets its own result.
        let e = entries(&fixture("shapes/parallel-tool-calls.jsonl"), true);
        let t = tools(&e);
        assert!(t.len() >= 2, "{t:?}");
        assert!(t.iter().all(|t| t.status == "completed"), "{t:?}");

        let e = entries(&fixture("shapes/subagent-call.jsonl"), true);
        let t = tools(&e)[0];
        assert_eq!(t.kind, "think");
        // Its result isn't in the fixture: cut off.
        assert_eq!(t.status, "failed");
        let live = entries(&fixture("shapes/subagent-call.jsonl"), false);
        assert_eq!(tools(&live)[0].status, "pending");
    }

    #[test]
    fn notes_and_skips() {
        let e = entries(&fixture("shapes/compaction.jsonl"), true);
        assert!(e.iter().any(|e| matches!(e, Entry::Note { text, .. } if text == "Conversation compacted")));
        let e = entries(&fixture("shapes/image.jsonl"), true);
        assert!(e.iter().any(|e| matches!(e, Entry::User { text, .. } if text.contains("[image]"))), "{e:#?}");
        let e = entries(&fixture("shapes/api-error.jsonl"), true);
        assert!(e.iter().all(|e| !matches!(e, Entry::User { .. })));
        let e = entries(&fixture("shapes/rewind.jsonl"), true);
        assert!(e.iter().any(|e| matches!(e, Entry::Note { text, .. } if text.starts_with("Rewound"))), "{e:#?}");
        // Subagent transcripts are sidechains: not the conversation.
        assert!(entries(&fixture("shapes/subagent-transcript.jsonl"), true).is_empty());
        // Unknown lines and garbage are skipped.
        assert!(entries(b"{\"type\":\"from-the-future\"}\nnot json\n\n", true).is_empty());
    }

    #[test]
    fn commands_and_reminders() {
        assert_eq!(
            command("<command-name>/model</command-name>\n<command-message>model</command-message>\n<command-args>haiku</command-args>").as_deref(),
            Some("/model haiku")
        );
        assert_eq!(
            command("<local-command-caveat>Caveat</local-command-caveat><command-name>/clear</command-name><command-args></command-args>").as_deref(),
            Some("/clear")
        );
        assert_eq!(command("hello"), None);
        assert_eq!(strip_reminders("<system-reminder>\nscratch\n</system-reminder>\n\nhello there"), "hello there");
        assert_eq!(strip_reminders("<system-reminder>only"), "");
        let e = entries(&fixture("shapes/desktop-transcript.jsonl"), true);
        assert!(e.iter().all(|e| !matches!(e, Entry::User { text, .. } if text.contains("system-reminder"))));
    }
}
