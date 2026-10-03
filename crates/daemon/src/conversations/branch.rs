//! Which branch of a transcript a resume follows (#79), so an opened
//! conversation can mark what Continue won't remember.
//!
//! The block shows the transcript in file order, every branch included
//! (S20). Claude Code resumes from one leaf and walks `parentUuid` back from
//! it; lines off that walk are a rewind's abandoned turns, another writer's
//! turns (S20 Q5), or a prompt cut off by an interrupt. This follows what
//! Claude Code 2.1.288 does when it loads a session:
//!
//! - **The leaf** is the newest `last-prompt.leafUuid`, unless the file's
//!   last line goes on from it (a turn not yet closed by a `last-prompt`);
//!   with no `last-prompt` it's the last line. A compaction after it drops
//!   it. A `last-prompt` with `leafUuid: null` and `explicit` cleared the
//!   conversation.
//! - **The walk** goes from the leaf's nearest user or assistant line up
//!   `parentUuid`. A parent missing from the file continues at the nearest
//!   line up to 5 s older. A compaction that kept a preserved segment
//!   re-links it under the summary first (Claude Code does the same).
//! - **Compactions are bridged** through `logicalParentUuid`: what came
//!   before is in the summary the model got, so it counts as remembered
//!   (Claude Code itself stops at the boundary).
//! - **Side lines are not:** an `away_summary` whose parent is older than
//!   the last exchange cuts that exchange off. Checked against a resumed
//!   copy of a real session: the model didn't know the prompt the walk
//!   skips, and did know the one before it.
//! - Parallel tool calls branch off the walk; the other lines of a response
//!   on it (same `message.id`) and the results of its tool calls are added
//!   back.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use crate::agent::transcript::Entry;

/// How far back a missing parent may be found by time (Claude Code's).
const MISSING_PARENT_MS: u64 = 5_000;

pub const NOTE: &str = "Not in what it remembers: Continue goes on from another branch";

#[derive(Default, Clone)]
pub struct Chain {
    lines: HashMap<String, Line>,
    order: Vec<String>,
    /// The newest `last-prompt.leafUuid`.
    leaf: Option<String>,
    /// That `last-prompt` was explicit and nothing came after it.
    explicit: bool,
    /// An explicit `last-prompt` with no leaf: the conversation was cleared.
    cleared: bool,
    /// The last line (not progress).
    last: Option<String>,
}

#[derive(Default, Clone)]
struct Line {
    parent: Option<String>,
    /// `user` or `assistant`.
    message: bool,
    progress: bool,
    at_ms: u64,
    /// An assistant line's `message.id`.
    response: Option<String>,
    /// An assistant line's tool calls.
    calls: Vec<String>,
    /// A user line made only of tool results: the calls they answer.
    results: Vec<String>,
    compaction: Option<Compaction>,
}

#[derive(Clone)]
struct Compaction {
    logical: Option<String>,
    /// The kept segment, head first, and the summary it goes under.
    preserved: Option<(Vec<String>, String)>,
    /// `preservedSegment`: head, tail and anchor, to walk.
    segment: Option<(String, String, String)>,
}

impl Chain {
    /// One line of the transcript, in file order.
    pub fn line(&mut self, v: &Value) {
        if v["type"] == "last-prompt" {
            match v["leafUuid"].as_str() {
                Some(l) if !l.is_empty() => {
                    self.leaf = Some(l.to_owned());
                    self.explicit = v["explicit"] == true;
                    self.cleared = false;
                }
                _ if v["leafUuid"].is_null() && v["explicit"] == true => {
                    self.leaf = None;
                    self.explicit = false;
                    self.cleared = true;
                }
                _ => {}
            }
            return;
        }
        let Some(uuid) = v["uuid"].as_str() else { return };
        if v["isSidechain"] == true {
            return;
        }
        let kind = v["type"].as_str().unwrap_or("");
        let content = &v["message"]["content"];
        let blocks = content.as_array().map(Vec::as_slice).unwrap_or_default();
        let ids = |t: &str, key: &str| -> Vec<String> {
            blocks.iter().filter(|b| b["type"] == t).filter_map(|b| b[key].as_str().map(str::to_owned)).collect()
        };
        let results = if kind == "user" && !blocks.is_empty() && blocks.iter().all(|b| b["type"] == "tool_result") {
            ids("tool_result", "tool_use_id")
        } else {
            vec![]
        };
        let compaction = (kind == "system" && v["subtype"] == "compact_boundary").then(|| {
            let m = &v["compactMetadata"];
            let s = |x: &Value| x.as_str().map(str::to_owned);
            let pm = &m["preservedMessages"];
            let seg = &m["preservedSegment"];
            Compaction {
                logical: s(&v["logicalParentUuid"]),
                preserved: pm["uuids"]
                    .as_array()
                    .filter(|u| !u.is_empty())
                    .and_then(|u| Some((u.iter().filter_map(s).collect(), s(&pm["anchorUuid"])?))),
                segment: (|| Some((s(&seg["headUuid"])?, s(&seg["tailUuid"])?, s(&seg["anchorUuid"])?)))(),
            }
        });
        let line = Line {
            parent: v["parentUuid"].as_str().map(str::to_owned),
            message: matches!(kind, "user" | "assistant"),
            progress: kind == "progress",
            at_ms: super::convert::at_ms(v["timestamp"].as_str().unwrap_or("")),
            response: if kind == "assistant" { v["message"]["id"].as_str().map(str::to_owned) } else { None },
            calls: if kind == "assistant" { ids("tool_use", "id") } else { vec![] },
            results,
            compaction,
        };
        if !line.progress {
            self.last = Some(uuid.to_owned());
            self.explicit = false;
            self.cleared = false;
            if line.compaction.is_some() {
                self.leaf = None;
            }
        }
        if self.lines.insert(uuid.to_owned(), line).is_none() {
            self.order.push(uuid.to_owned());
        }
    }

    /// The lines a resume would follow; `None` if there's nothing to say
    /// (no conversation lines yet).
    pub fn remembered(&self) -> Option<HashSet<&str>> {
        if self.lines.is_empty() {
            return None;
        }
        if self.cleared {
            return Some(HashSet::new());
        }
        let mut parent: HashMap<&str, Option<&str>> =
            self.lines.iter().map(|(u, l)| (u.as_str(), l.parent.as_deref())).collect();
        // A compaction's preserved segment goes under its summary, and what
        // hung off the summary goes on from the segment's end. From the
        // boundary, what came before is above the segment's head.
        let mut before: HashMap<&str, Option<&str>> = HashMap::new();
        for u in &self.order {
            let Some(c) = &self.lines[u].compaction else { continue };
            let Some((kept, anchor)) = self.preserved(c) else { continue };
            if !self.lines.contains_key(anchor) || kept.iter().any(|k| !self.lines.contains_key(*k)) {
                continue;
            }
            before.insert(u, self.lines[kept[0]].parent.as_deref());
            let (head, tail) = (kept[0], kept[kept.len() - 1]);
            for (k, p) in parent.iter_mut() {
                if *p == Some(anchor) && *k != head {
                    *p = Some(tail);
                }
            }
            let mut prev = anchor;
            for k in kept {
                parent.insert(k, Some(prev));
                prev = k;
            }
        }

        let last = self.last.as_deref();
        let mut start = self.leaf.as_deref().filter(|l| self.lines.contains_key(*l));
        if let Some(l) = start
            && !self.explicit
            && descends(&parent, last, l)
        {
            start = last;
        }
        let mut x = start.or(last);
        let mut seen = HashSet::new();
        while let Some(u) = x {
            if !seen.insert(u) || self.lines.get(u).is_none_or(|l| l.message) {
                break;
            }
            x = parent_of(&parent, u);
        }
        let leaf = x.filter(|u| self.lines.get(*u).is_some_and(|l| l.message))?;

        let mut keep: HashSet<&str> = HashSet::new();
        let mut x = Some(leaf);
        while let Some(u) = x {
            if !keep.insert(u) {
                break;
            }
            let line = &self.lines[u];
            let mut p = parent_of(&parent, u);
            if p.is_none()
                && let Some(c) = &line.compaction
            {
                p = c.logical.as_deref();
                if p.is_some_and(|p| keep.contains(p)) {
                    p = before.get(u).copied().flatten();
                }
            }
            let Some(pu) = p else { break };
            x = if self.lines.contains_key(pu) && !keep.contains(pu) { Some(pu) } else { self.near(line.at_ms, &keep) };
        }

        let responses: HashSet<&str> = keep.iter().filter_map(|u| self.lines[*u].response.as_deref()).collect();
        let mut calls: HashSet<&str> = HashSet::new();
        for u in &self.order {
            let l = &self.lines[u];
            if keep.contains(u.as_str()) || l.response.as_deref().is_some_and(|r| responses.contains(r)) {
                keep.insert(u);
                calls.extend(l.calls.iter().map(String::as_str));
            }
        }
        for u in &self.order {
            let l = &self.lines[u];
            if !l.results.is_empty() && l.results.iter().all(|r| calls.contains(r.as_str())) {
                keep.insert(u);
            }
        }
        Some(keep)
    }

    fn preserved<'a>(&'a self, c: &'a Compaction) -> Option<(Vec<&'a str>, &'a str)> {
        if let Some((kept, anchor)) = &c.preserved {
            return Some((kept.iter().map(String::as_str).collect(), anchor));
        }
        let (head, tail, anchor) = c.segment.as_ref()?;
        let mut kept = vec![];
        let mut x = Some(tail.as_str());
        while let Some(u) = x {
            if kept.contains(&u) {
                return None;
            }
            kept.push(u);
            if u == head {
                kept.reverse();
                return Some((kept, anchor));
            }
            x = self.lines.get(u)?.parent.as_deref();
        }
        None
    }

    /// A missing parent: the nearest line up to 5 s older, not on the walk.
    fn near(&self, at_ms: u64, keep: &HashSet<&str>) -> Option<&str> {
        if at_ms == 0 {
            return None;
        }
        self.lines
            .iter()
            .filter(|(u, l)| !l.progress && l.at_ms != 0 && !keep.contains(u.as_str()))
            .filter(|(_, l)| l.at_ms <= at_ms && at_ms - l.at_ms <= MISSING_PARENT_MS)
            .min_by_key(|(u, l)| (at_ms - l.at_ms, self.order.iter().position(|o| o == *u)))
            .map(|(u, _)| u.as_str())
    }
}

/// `entries` with what isn't on the remembered branch marked, and a note
/// before each run of it. `from[i]` is the line entry `i` came from.
pub fn mark(entries: Vec<Entry>, from: &[String], remembered: Option<&HashSet<&str>>) -> Vec<Entry> {
    let Some(keep) = remembered else { return entries };
    let mut out = Vec::with_capacity(entries.len());
    let mut was = false;
    for (i, mut e) in entries.into_iter().enumerate() {
        let gone = from.get(i).is_some_and(|u| !u.is_empty() && !keep.contains(u.as_str()));
        if gone {
            if !was {
                out.push(Entry::Note { text: NOTE.into(), at_ms: at_ms(&e), forgotten: false });
            }
            e.forget();
        }
        was = gone;
        out.push(e);
    }
    out
}

/// Whether `x` is `from` or goes on from it.
fn descends<'a>(parent: &HashMap<&'a str, Option<&'a str>>, mut x: Option<&'a str>, from: &str) -> bool {
    let mut seen = HashSet::new();
    while let Some(u) = x {
        if u == from {
            return true;
        }
        if !seen.insert(u) {
            return false;
        }
        x = parent_of(parent, u);
    }
    false
}

fn parent_of<'a>(parent: &HashMap<&'a str, Option<&'a str>>, u: &str) -> Option<&'a str> {
    parent.get(u).copied().flatten()
}

fn at_ms(e: &Entry) -> u64 {
    match e {
        Entry::User { at_ms, .. } | Entry::Note { at_ms, .. } => *at_ms,
        Entry::Tool(t) => t.started_ms,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::conversations::convert::entries;

    fn fixture(name: &str) -> Vec<u8> {
        let p = format!("{}/tests/fixtures/conversations/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read(&p).unwrap_or_else(|e| panic!("{p}: {e}"))
    }

    /// Each prompt, and whether it's remembered.
    fn prompts(e: &[Entry]) -> Vec<(String, bool)> {
        e.iter()
            .filter_map(|e| match e {
                Entry::User { text, forgotten, .. } => Some((text.clone(), !forgotten)),
                _ => None,
            })
            .collect()
    }

    fn remembered(p: &[(String, bool)], has: &str) -> bool {
        p.iter().find(|(t, _)| t.contains(has)).unwrap_or_else(|| panic!("no prompt {has:?} in {p:#?}")).1
    }

    /// The note comes right before each run of what isn't remembered.
    fn noted(e: &[Entry]) {
        for (i, x) in e.iter().enumerate() {
            let starts = x.forgotten() && (i == 0 || !e[i - 1].forgotten());
            if starts {
                assert!(
                    matches!(&e[i - 1], Entry::Note { text, .. } if text == NOTE),
                    "{:#?}",
                    &e[i.saturating_sub(2)..=i]
                );
            }
        }
    }

    #[test]
    fn a_rewind() {
        // Claude Code's own rewind (`--resume-session-at` back to the first
        // reply). A resumed copy answered "APPLE, blue".
        let e = entries(&fixture("branches/rewind.jsonl"), true);
        let p = prompts(&e);
        assert!(remembered(&p, "APPLE"));
        assert!(!remembered(&p, "red"));
        assert!(remembered(&p, "blue"));
        noted(&e);
        // The abandoned prompt's reply goes with it.
        let red = e.iter().position(|e| matches!(e, Entry::User { text, .. } if text.contains("red"))).unwrap();
        assert!(
            e[red + 1..]
                .iter()
                .take_while(|e| !matches!(e, Entry::User { .. } | Entry::Note { .. }))
                .all(Entry::forgotten)
        );
        assert_eq!(e.iter().filter(|e| matches!(e, Entry::Note { text, .. } if text == NOTE)).count(), 1);
    }

    #[test]
    fn two_writers() {
        // S20 Q5, once the CLI had exited: its `last-prompt` is the newest,
        // so the adapter's turns are off the branch. A resumed copy answered
        // "UNKNOWN, teal".
        let e = entries(&fixture("scratch/a683c96a-c2b1-4ed7-bdd4-51b7d759125b.jsonl"), true);
        let p = prompts(&e);
        assert!(remembered(&p, "Remember the number 4817"));
        assert!(remembered(&p, "New secret"));
        assert!(!remembered(&p, "vault code is 2468"));
        assert!(remembered(&p, "lighthouse"));
        assert!(!remembered(&p, "One line: the vault code"));
        noted(&e);
        assert!(e.iter().any(|e| matches!(e, Entry::Agent { text, forgotten: true, .. } if text.contains("2468"))));

        // While the CLI was still open: the adapter's `last-prompt` is the
        // newest, and the CLI's turn doesn't go on from it. A resumed copy
        // answered "2468, UNKNOWN".
        let e = entries(&fixture("branches/two-writers-open.jsonl"), false);
        let p = prompts(&e);
        assert!(remembered(&p, "vault code is 2468"));
        assert!(!remembered(&p, "lighthouse"));
        noted(&e);
    }

    #[test]
    fn one_branch_marks_nothing() {
        for f in
            ["shapes/bash.jsonl", "shapes/compaction.jsonl", "shapes/parallel-tool-calls.jsonl", "shapes/edit.jsonl"]
        {
            let e = entries(&fixture(f), true);
            assert!(!e.is_empty(), "{f}");
            assert!(e.iter().all(|e| !e.forgotten()), "{f}: {e:#?}");
        }
    }

    fn chain(lines: &[Value]) -> Chain {
        let mut c = Chain::default();
        for l in lines {
            c.line(l);
        }
        c
    }

    fn msg(uuid: &str, parent: Option<&str>, kind: &str, at: &str) -> Value {
        json!({ "type": kind, "uuid": uuid, "parentUuid": parent, "timestamp": format!("2026-10-02T20:00:{at}Z"),
                "message": { "role": kind, "content": uuid } })
    }

    fn sys(uuid: &str, parent: Option<&str>, subtype: &str, at: &str) -> Value {
        json!({ "type": "system", "subtype": subtype, "uuid": uuid, "parentUuid": parent,
                "timestamp": format!("2026-10-02T20:00:{at}Z") })
    }

    fn set(c: &Chain) -> Vec<&str> {
        let mut v: Vec<&str> = c.remembered().unwrap().into_iter().collect();
        v.sort();
        v
    }

    #[test]
    fn a_compaction_is_bridged() {
        // A compaction that kept a segment (c, d and t), with t written
        // after the boundary under the summary f: Claude Code re-links it.
        let mut boundary = sys("e", None, "compact_boundary", "05");
        boundary["logicalParentUuid"] = json!("t");
        boundary["compactMetadata"] = json!({ "preservedMessages": { "anchorUuid": "f", "uuids": ["c", "d", "t"] } });
        let c = chain(&[
            msg("a", None, "user", "00"),
            msg("b", Some("a"), "assistant", "01"),
            msg("c", Some("b"), "user", "02"),
            msg("d", Some("c"), "assistant", "03"),
            boundary.clone(),
            msg("f", Some("e"), "user", "05"),
            msg("t", Some("f"), "user", "03"),
            msg("g", Some("t"), "user", "06"),
            msg("h", Some("g"), "assistant", "07"),
        ]);
        assert_eq!(set(&c), ["a", "b", "c", "d", "e", "f", "g", "h", "t"]);

        // The same with `preservedSegment`, walked from its tail.
        boundary["compactMetadata"] =
            json!({ "preservedSegment": { "headUuid": "c", "tailUuid": "t", "anchorUuid": "f" } });
        let c = chain(&[
            msg("a", None, "user", "00"),
            msg("b", Some("a"), "assistant", "01"),
            msg("c", Some("b"), "user", "02"),
            msg("d", Some("c"), "assistant", "03"),
            boundary,
            msg("f", Some("e"), "user", "05"),
            msg("t", Some("d"), "user", "03"),
            msg("g", Some("f"), "user", "06"),
        ]);
        assert_eq!(set(&c), ["a", "b", "c", "d", "e", "f", "g", "t"]);
    }

    #[test]
    fn side_lines_and_missing_parents() {
        // An away summary written under an older line cuts off the exchange
        // after that line, as it does for Claude Code (checked on a resumed
        // copy of a real session).
        let c = chain(&[
            msg("a", None, "user", "00"),
            msg("b", Some("a"), "assistant", "01"),
            sys("c", Some("b"), "turn_duration", "01"),
            msg("d", Some("c"), "user", "02"),
            msg("e", Some("d"), "assistant", "03"),
            sys("f", Some("c"), "away_summary", "04"),
            msg("g", Some("f"), "user", "05"),
            msg("h", Some("g"), "assistant", "06"),
        ]);
        assert_eq!(set(&c), ["a", "b", "c", "f", "g", "h"]);

        // A parent that isn't in the file: the nearest line before, within
        // 5 s.
        let c = chain(&[
            msg("a", None, "user", "00"),
            msg("b", Some("a"), "assistant", "01"),
            msg("c", Some("gone"), "user", "03"),
            msg("d", Some("c"), "assistant", "04"),
        ]);
        assert_eq!(set(&c), ["a", "b", "c", "d"]);
        let c = chain(&[
            msg("a", None, "user", "00"),
            msg("b", Some("a"), "assistant", "01"),
            msg("c", Some("gone"), "user", "09"),
        ]);
        assert_eq!(set(&c), ["c"]);
    }

    #[test]
    fn the_leaf() {
        let lp = |leaf: Value, explicit: bool| json!({ "type": "last-prompt", "leafUuid": leaf, "explicit": explicit });
        let rewound = [
            msg("a", None, "user", "00"),
            msg("b", Some("a"), "assistant", "01"),
            msg("c", Some("b"), "user", "02"),
            msg("d", Some("c"), "assistant", "03"),
            msg("e", Some("b"), "user", "04"),
            msg("f", Some("e"), "assistant", "05"),
        ];
        // No last-prompt: the last line.
        assert_eq!(set(&chain(&rewound)), ["a", "b", "e", "f"]);
        // The newest last-prompt, though another branch came after it.
        let mut lines = rewound.to_vec();
        lines.insert(4, lp(json!("d"), false));
        assert_eq!(set(&chain(&lines)), ["a", "b", "c", "d"]);
        // A turn that goes on from it, not closed yet.
        let mut lines = rewound[..4].to_vec();
        lines.push(lp(json!("b"), false));
        assert_eq!(set(&chain(&lines)), ["a", "b", "c", "d"]);
        // Unless that last-prompt was explicit and nothing followed it.
        lines.truncate(2);
        lines.push(msg("c", Some("b"), "user", "02"));
        lines.push(lp(json!("b"), true));
        assert_eq!(set(&chain(&lines)), ["a", "b"]);
        // Cleared.
        lines.push(lp(Value::Null, true));
        assert!(chain(&lines).remembered().unwrap().is_empty());
        assert!(Chain::default().remembered().is_none());
    }

    #[test]
    fn marks_survive_the_block_log() {
        let e = entries(&fixture("branches/rewind.jsonl"), true);
        let back: Vec<Entry> = serde_json::from_slice(&serde_json::to_vec(&e).unwrap()).unwrap();
        assert_eq!(back, e);
        // Entries from before #79 read as remembered.
        let old: Entry = serde_json::from_value(json!({ "type": "user", "text": "hi", "at_ms": 1 })).unwrap();
        assert!(!old.forgotten());
    }
}
