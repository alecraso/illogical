//! Reading history back: commands across panes (open and recently closed),
//! full-text search of their output, and asciicast export. Everything here
//! reads the pane directories the store writes; nothing needs the panes to
//! be running.

use std::path::Path;

use illogical_proto::{
    Event as ApiEvent, EventKind, PaneId,
    api::{DriverEntry, HistoryEntry, HistoryKind, SearchHit},
};
use regex::Regex;

use crate::{
    osc::strip,
    store::{Event, PaneLog, StateDir, read_events},
};

#[derive(Debug, Default)]
pub struct Filter {
    pub pane: Option<PaneId>,
    pub failed: bool,
    /// Only this kind; none is all of them.
    pub kind: Option<HistoryKind>,
    pub since_ms: Option<u64>,
    pub cwd: Option<String>,
    pub matching: Option<Regex>,
}

/// Who typed in a pane, by handoff (M13), oldest first.
pub fn drivers(dir: &Path) -> Vec<DriverEntry> {
    read_events(dir)
        .into_iter()
        .filter_map(|(offset, e)| match e {
            Event::Driver { at_ms, who } => Some(DriverEntry { at_ms, offset, who }),
            _ => None,
        })
        .collect()
}

/// Commands (from the shell integration's marks), oldest first.
pub fn commands(dir: &Path, pane: PaneId, open: bool) -> Vec<HistoryEntry> {
    commands_in(read_events(dir), pane, open)
}

/// The same, from a pane's events (a synced copy's, say).
pub fn commands_in(events: Vec<(u64, Event)>, pane: PaneId, open: bool) -> Vec<HistoryEntry> {
    let mut out: Vec<HistoryEntry> = Vec::new();
    let mut cwd: Option<String> = None;
    for (offset, e) in events {
        match e {
            Event::Cwd { path } => cwd = Some(path),
            Event::Command { at_ms, text, cwd: c, by, kind } => out.push(HistoryEntry {
                pane,
                open,
                text,
                cwd: c.or(cwd.clone()),
                exit: None,
                started_ms: at_ms,
                ended_ms: None,
                start: offset,
                end: None,
                host: None,
                by,
                kind,
            }),
            Event::End { at_ms, exit } => {
                // The latest one still running: a note recorded inside a
                // command (M29's approvals) is its own finished entry.
                if let Some(last) = out.iter_mut().rev().find(|l| l.end.is_none()) {
                    last.end = Some(offset);
                    last.ended_ms = Some(at_ms);
                    last.exit = exit;
                }
            }
            _ => {}
        }
    }
    out
}

pub fn history(store: &StateDir, f: &Filter, limit: usize) -> Vec<HistoryEntry> {
    let all = store
        .pane_dirs()
        .into_iter()
        .filter(|(id, _, _)| f.pane.is_none_or(|p| p == *id))
        .flat_map(|(id, open, dir)| commands(&dir, id, open));
    filtered(all, f, limit)
}

/// Entries that pass the filter, the last `limit` by start time.
pub fn filtered(all: impl Iterator<Item = HistoryEntry>, f: &Filter, limit: usize) -> Vec<HistoryEntry> {
    let dirs = f.cwd.as_deref().map(crate::paths::forms);
    let mut all: Vec<HistoryEntry> = all
        // Only a command that ran has an exit code that means failure.
        .filter(|c| !f.failed || (c.kind.is_command() && c.exit.is_some_and(|e| e != 0)))
        .filter(|c| f.kind.is_none_or(|k| k == c.kind))
        .filter(|c| f.since_ms.is_none_or(|s| c.started_ms >= s))
        .filter(|c| {
            dirs.as_ref()
                .is_none_or(|ds| c.cwd.as_deref().is_some_and(|x| ds.iter().any(|d| crate::paths::is_under(x, d))))
        })
        .filter(|c| f.matching.as_ref().is_none_or(|re| c.text.as_deref().is_some_and(|t| re.is_match(t))))
        .collect();
    all.sort_by_key(|c| c.started_ms);
    let skip = all.len().saturating_sub(limit);
    all.split_off(skip)
}

/// Lines of output (escape sequences stripped) matching `re`, newest panes
/// first, with the command each came from.
pub fn search(store: &StateDir, re: &Regex, since_ms: Option<u64>, limit: usize) -> Vec<SearchHit> {
    let mut hits = Vec::new();
    for (pane, open, dir) in store.pane_dirs() {
        let events = read_events(&dir);
        // An agent's log is its JSON-RPC stream: search what it said and
        // ran instead (offsets are line numbers in `capture --text`).
        if let Some(text) = crate::agent::transcript_of(&dir) {
            if since_ms
                .is_some_and(|s| !events.iter().any(|(_, e)| matches!(e, Event::Command { at_ms, .. } if *at_ms >= s)))
            {
                continue;
            }
            for (n, line) in text.lines().enumerate() {
                if re.is_match(line) {
                    hits.push(SearchHit {
                        pane,
                        open,
                        offset: n as u64,
                        line: line.to_owned(),
                        command: None,
                        host: None,
                        thread: None,
                    });
                    if hits.len() >= limit {
                        return hits;
                    }
                }
            }
            continue;
        }
        let read = |from| PaneLog::open(dir.clone()).and_then(|l| l.read_from(from)).ok();
        if search_log(pane, open, events, read, re, since_ms, limit, &mut hits) {
            return hits;
        }
    }
    // What people said in threads (M61), open panes' or closed ones'.
    let open: std::collections::HashSet<PaneId> =
        store.pane_dirs().into_iter().filter(|(_, open, _)| *open).map(|(p, _, _)| p).collect();
    let threads = crate::threads::Threads::open(store.root());
    let mut targets: Vec<_> = threads.targets().collect();
    targets.sort();
    for t in targets {
        let pane = match t {
            illogical_proto::ThreadTarget::Pane(p) => p,
            illogical_proto::ThreadTarget::Session(_) => 0,
        };
        for m in threads.get(t) {
            if since_ms.is_some_and(|s| m.at < s) {
                continue;
            }
            let line = format!("{}: {}", m.name, m.text);
            let quoted = m.quote.as_ref().is_some_and(|q| re.is_match(&q.text));
            if re.is_match(&line) || quoted {
                hits.push(SearchHit {
                    pane,
                    open: open.contains(&pane),
                    offset: m.id,
                    line,
                    command: None,
                    host: None,
                    thread: Some(t.key()),
                });
                if hits.len() >= limit {
                    return hits;
                }
            }
        }
    }
    hits
}

/// Search one terminal's output (`read` gives its log from an offset),
/// adding to `hits`; whether `limit` was reached.
#[allow(clippy::too_many_arguments)]
pub fn search_log(
    pane: PaneId,
    open: bool,
    events: Vec<(u64, Event)>,
    read: impl FnOnce(u64) -> Option<(u64, Vec<u8>)>,
    re: &Regex,
    since_ms: Option<u64>,
    limit: usize,
    hits: &mut Vec<SearchHit>,
) -> bool {
    // Skip output older than `since`: start at the first time mark at or
    // after it, and skip panes with nothing that recent.
    let from = match since_ms {
        None => 0,
        Some(s) => match events.iter().find_map(|(o, e)| match e {
            Event::Time { at_ms } | Event::Command { at_ms, .. } if *at_ms >= s => Some(*o),
            _ => None,
        }) {
            Some(o) => o,
            None => return false,
        },
    };
    let cmds = commands_in(events, pane, open);
    let Some((start, bytes)) = read(from) else { return false };
    let mut at = start;
    for raw in bytes.split(|b| *b == b'\n') {
        let line = strip(raw);
        let line = line.trim_end_matches('\n');
        if re.is_match(line) {
            // The first output line begins before the command's start mark
            // (the mark's own bytes open that line), so compare with where
            // the line ends.
            let line_end = at + raw.len() as u64;
            let command = cmds
                .iter()
                .rev()
                .find(|c| c.start <= line_end && c.end.is_none_or(|e| at < e))
                .and_then(|c| c.text.clone());
            hits.push(SearchHit { pane, open, offset: at, line: line.to_owned(), command, host: None, thread: None });
            if hits.len() >= limit {
                return true;
            }
        }
        at += raw.len() as u64 + 1;
    }
    false
}

/// What a pane's index says happened, as API events (for `events` without
/// `--follow`).
pub fn stored_events(dir: &Path, pane: PaneId, since_ms: u64) -> Vec<ApiEvent> {
    let mut out = Vec::new();
    let mut last_text: Option<String> = None;
    for (_, e) in read_events(dir) {
        let (at_ms, kind) = match e {
            Event::Prompt { at_ms } => (at_ms, EventKind::Prompt),
            Event::Command { at_ms, text, .. } => {
                last_text = text.clone();
                (at_ms, EventKind::CommandStart { text })
            }
            Event::End { at_ms, exit } => (at_ms, EventKind::CommandEnd { text: last_text.take(), exit }),
            Event::Notify { at_ms, title, body } => (at_ms, EventKind::Notify { title, body }),
            Event::Bell { at_ms } => (at_ms, EventKind::Bell),
            _ => continue,
        };
        if at_ms >= since_ms {
            out.push(ApiEvent { at_ms, pane: Some(pane), kind });
        }
    }
    out
}

/// The pane's output as asciicast v3 (`asciinema play`). Timing comes from
/// the index's time marks, so it's accurate to about a second.
pub fn export_cast(dir: &Path, title: &str) -> std::io::Result<String> {
    let log = PaneLog::open(dir.to_owned())?;
    let (start, bytes) = log.read_from(0)?;
    let events = read_events(dir);
    let (cols, rows) = events
        .iter()
        .find_map(|(_, e)| match e {
            Event::Resize { cols, rows } => Some((*cols, *rows)),
            _ => None,
        })
        .unwrap_or((80, 24));
    let first_ms = events
        .iter()
        .find_map(|(_, e)| match e {
            Event::Time { at_ms } | Event::Restore { at_ms } | Event::Command { at_ms, .. } => Some(*at_ms),
            _ => None,
        })
        .unwrap_or(0);
    let header = serde_json::json!({
        "version": 3,
        "term": { "cols": cols, "rows": rows },
        "timestamp": first_ms / 1000,
        "title": title,
    });
    let mut out = format!("{header}\n");
    let mut last_ms = first_ms;
    let mut at = start;
    let mut emit = |out: &mut String, at_ms: u64, code: &str, data: String| {
        let interval = at_ms.saturating_sub(last_ms) as f64 / 1000.0;
        last_ms = last_ms.max(at_ms);
        out.push_str(&serde_json::json!([interval, code, data]).to_string());
        out.push('\n');
    };
    let mut now = first_ms;
    for (offset, e) in events.iter().filter(|(o, _)| *o >= start) {
        if *offset > at {
            let chunk = &bytes[(at - start) as usize..((*offset - start) as usize).min(bytes.len())];
            emit(&mut out, now, "o", String::from_utf8_lossy(chunk).into_owned());
            at = *offset;
        }
        match e {
            Event::Time { at_ms } | Event::Restore { at_ms } | Event::Prompt { at_ms } | Event::Bell { at_ms } => {
                now = now.max(*at_ms)
            }
            Event::Command { at_ms, text, .. } => {
                now = now.max(*at_ms);
                emit(&mut out, now, "m", text.clone().unwrap_or_default());
            }
            Event::End { at_ms, .. } => now = now.max(*at_ms),
            Event::Resize { cols, rows } => emit(&mut out, now, "r", format!("{cols}x{rows}")),
            _ => {}
        }
    }
    if (at - start) < bytes.len() as u64 {
        emit(&mut out, now, "o", String::from_utf8_lossy(&bytes[(at - start) as usize..]).into_owned());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{PaneLog, now_ms};

    #[test]
    fn commands_search_and_cast_from_a_pane_dir() {
        let root = std::env::temp_dir().join(format!("illogical-history-{}-{}", std::process::id(), now_ms()));
        let store = StateDir::open(root.clone()).unwrap();
        let mut log = PaneLog::open(store.pane_dir(3)).unwrap();
        log.record(0, Event::Resize { cols: 100, rows: 30 }).unwrap();
        log.record(0, Event::Time { at_ms: 1_000 }).unwrap();
        log.append(b"$ make test\r\n").unwrap();
        log.record(
            13,
            Event::Command {
                at_ms: 1_000,
                text: Some("make test".into()),
                cwd: Some("/src".into()),
                by: None,
                kind: HistoryKind::Command,
            },
        )
        .unwrap();
        log.append(b"\x1b[31mFAILED\x1b[0m: 2 tests\r\n").unwrap();
        log.record(log.end(), Event::End { at_ms: 4_000, exit: Some(2) }).unwrap();
        log.append(b"$ ").unwrap();

        let h = history(&store, &Filter { failed: true, ..Default::default() }, 10);
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].text.as_deref(), Some("make test"));
        assert_eq!((h[0].exit, h[0].cwd.as_deref(), h[0].start), (Some(2), Some("/src"), 13));

        let hits = search(&store, &Regex::new("FAILED").unwrap(), None, 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line, "FAILED: 2 tests");
        assert_eq!(hits[0].command.as_deref(), Some("make test"));

        let cast = export_cast(&store.pane_dir(3), "t").unwrap();
        let lines: Vec<&str> = cast.lines().collect();
        assert!(lines[0].contains("\"version\":3") && lines[0].contains("\"cols\":100"));
        assert!(cast.contains(r#""m","make test""#));
        assert!(cast.contains("FAILED"));

        // Retired (closed) panes still answer.
        log.retire(3);
        let h = history(&store, &Filter::default(), 10);
        assert_eq!((h.len(), h[0].open), (1, false));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn cwd_filter_is_the_directory_or_below_through_links() {
        let root = std::env::temp_dir().join(format!("illogical-cwd-{}-{}", std::process::id(), now_ms()));
        let real = root.join("real");
        std::fs::create_dir_all(real.join("repo/src")).unwrap();
        std::fs::create_dir_all(real.join("repo2")).unwrap();
        std::os::unix::fs::symlink(&real, root.join("link")).unwrap();
        // The shell reports resolved paths, as macOS does for /var.
        let real = std::fs::canonicalize(&real).unwrap().display().to_string();
        let entry = |cwd: &str| HistoryEntry {
            pane: 1,
            open: true,
            text: Some(cwd.into()),
            cwd: Some(cwd.into()),
            exit: Some(0),
            started_ms: 0,
            ended_ms: None,
            start: 0,
            end: None,
            host: None,
            by: None,
            kind: HistoryKind::Command,
        };
        let cwds = [format!("{real}/repo"), format!("{real}/repo/src"), format!("{real}/repo2")];
        let under = |dir: String| {
            let f = Filter { cwd: Some(dir), ..Default::default() };
            filtered(cwds.iter().map(|c| entry(c)), &f, 10).into_iter().filter_map(|h| h.text).collect::<Vec<_>>()
        };
        let link = root.join("link/repo").display().to_string();
        assert_eq!(under(format!("{real}/repo")), cwds[..2]);
        assert_eq!(under(format!("{real}/repo/")), cwds[..2]);
        assert_eq!(under(link), cwds[..2], "a path through a link matches the resolved one");
        assert_eq!(under(format!("{real}/rep")), Vec::<String>::new());
        assert_eq!(under("/".into()).len(), 3);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn answers_are_not_commands_and_a_kind_filters() {
        let root = std::env::temp_dir().join(format!("illogical-kinds-{}-{}", std::process::id(), now_ms()));
        let store = StateDir::open(root.clone()).unwrap();
        let mut log = PaneLog::open(store.pane_dir(4)).unwrap();
        let mut entry = |at_ms, text: &str, by: Option<&str>, kind, exit| {
            let at = log.end();
            let cmd = Event::Command { at_ms, text: Some(text.into()), cwd: None, by: by.map(Into::into), kind };
            log.record(at, cmd).unwrap();
            log.record(at, Event::End { at_ms: at_ms + 1, exit }).unwrap();
        };
        entry(1_000, "make", None, HistoryKind::Command, Some(2));
        // What a person did: a record with an exit code of 1 or 0, still not a command.
        entry(2_000, "allowed: Bash: make", Some("sam"), HistoryKind::Answer, Some(1));
        entry(3_000, "Read /x", None, HistoryKind::Agent, None);
        entry(4_000, "ls", None, HistoryKind::Command, Some(0));

        let texts = |f: &Filter| history(&store, f, 10).into_iter().map(|h| h.text.unwrap()).collect::<Vec<_>>();
        assert_eq!(texts(&Filter::default()), ["make", "allowed: Bash: make", "Read /x", "ls"], "all, by default");
        assert_eq!(texts(&Filter { failed: true, ..Default::default() }), ["make"]);
        let answer = Filter { kind: Some(HistoryKind::Answer), ..Default::default() };
        let h = history(&store, &answer, 10);
        assert_eq!((h.len(), h[0].by.as_deref(), h[0].kind), (1, Some("sam"), HistoryKind::Answer));
        let command = Filter { kind: Some(HistoryKind::Command), ..Default::default() };
        assert_eq!(texts(&command), ["make", "ls"]);
        // Asking for failed answers finds none: they don't fail.
        assert!(texts(&Filter { failed: true, kind: Some(HistoryKind::Answer), ..Default::default() }).is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_record_without_a_kind_is_a_command() {
        let old = r#"{"e":"command","at_ms":5,"text":"ls","by":"sam"}"#;
        let e: Event = serde_json::from_str(old).expect("an older index record reads");
        assert!(matches!(e, Event::Command { kind: HistoryKind::Command, .. }), "{e:?}");
        // Commands are written as they always were; others say what they are.
        let cmd = Event::Command { at_ms: 5, text: None, cwd: None, by: None, kind: HistoryKind::Command };
        assert!(!serde_json::to_string(&cmd).unwrap().contains("kind"));
        let ans = Event::Command { at_ms: 5, text: None, cwd: None, by: None, kind: HistoryKind::Answer };
        assert!(serde_json::to_string(&ans).unwrap().contains(r#""kind":"answer""#));
        assert_eq!(HistoryKind::parse("answer"), Some(HistoryKind::Answer));
        assert_eq!(HistoryKind::parse("nope"), None);
    }
}
