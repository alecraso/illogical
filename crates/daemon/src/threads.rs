//! Threads on panes and sessions (M61): who said what, kept by the daemon
//! that owns the pane, in `<state>/threads/`.
//!
//! - `<target>.jsonl`: one [`ThreadMsg`] per line, appended (`pane-7.jsonl`,
//!   `session-2.jsonl`). They're outside the pane's own directory, so
//!   *Forget history* and closing the pane leave the conversation.
//! - `reads.json`: how far each person has read each thread.
//!
//! Who may read or post is the mux's call; this only keeps them.

use std::collections::{BTreeMap, HashMap};
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use illogical_proto::{ThreadMsg, ThreadTarget};
use tracing::warn;

/// The longest message kept, in bytes (longer ones are cut).
pub const MAX_TEXT: usize = 8 * 1024;
/// The longest quote kept, in bytes.
pub const MAX_QUOTE: usize = 16 * 1024;

pub struct Threads {
    dir: PathBuf,
    msgs: HashMap<ThreadTarget, Vec<ThreadMsg>>,
    /// Principal id → thread → the last message id they've read.
    reads: BTreeMap<String, BTreeMap<String, u64>>,
    reads_dirty: bool,
}

impl Threads {
    /// Every thread under `<root>/threads`; a line that doesn't parse is
    /// skipped (a crash mid-append leaves at most one).
    pub fn open(root: &Path) -> Self {
        let dir = root.join("threads");
        if let Err(e) = crate::store::private_dir(&dir) {
            warn!(?e, dir = %dir.display(), "can't make the threads directory");
        }
        let mut msgs = HashMap::new();
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            let Some(target) = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(".jsonl"))
                .and_then(ThreadTarget::parse)
            else {
                continue;
            };
            let Ok(f) = std::fs::File::open(&path) else { continue };
            let list: Vec<ThreadMsg> = io::BufReader::new(f)
                .lines()
                .map_while(Result::ok)
                .filter_map(|l| serde_json::from_str(&l).ok())
                .collect();
            if !list.is_empty() {
                msgs.insert(target, list);
            }
        }
        let reads = std::fs::read(dir.join("reads.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Self { dir, msgs, reads, reads_dirty: false }
    }

    pub fn get(&self, target: ThreadTarget) -> &[ThreadMsg] {
        self.msgs.get(&target).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Every thread that has messages.
    pub fn targets(&self) -> impl Iterator<Item = ThreadTarget> + '_ {
        self.msgs.keys().copied()
    }

    /// Add a message, numbered and timed here, and write it down.
    pub fn post(&mut self, target: ThreadTarget, mut msg: ThreadMsg) -> io::Result<ThreadMsg> {
        let list = self.msgs.entry(target).or_default();
        msg.id = list.last().map_or(1, |m| m.id + 1);
        msg.text = cut(msg.text, MAX_TEXT);
        if let Some(q) = msg.quote.as_mut() {
            q.text = cut(std::mem::take(&mut q.text), MAX_QUOTE);
        }
        let mut line = serde_json::to_vec(&msg).map_err(io::Error::other)?;
        line.push(b'\n');
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join(format!("{}.jsonl", target.key())))?;
        f.write_all(&line)?;
        f.sync_data()?;
        list.push(msg.clone());
        Ok(msg)
    }

    /// The last message `who` has read in `target` (0: none).
    pub fn read_upto(&self, who: &str, target: ThreadTarget) -> u64 {
        self.reads.get(who).and_then(|r| r.get(&target.key())).copied().unwrap_or(0)
    }

    /// `who` has read up to `upto`; it never goes backwards.
    pub fn mark_read(&mut self, who: &str, target: ThreadTarget, upto: u64) -> bool {
        let last = self.get(target).last().map_or(0, |m| m.id);
        let upto = upto.min(last);
        let r = self.reads.entry(who.to_owned()).or_default().entry(target.key()).or_default();
        if upto <= *r {
            return false;
        }
        *r = upto;
        self.reads_dirty = true;
        true
    }

    /// Write `reads.json` if anything changed.
    pub fn save(&mut self) {
        if !self.reads_dirty {
            return;
        }
        match serde_json::to_vec(&self.reads) {
            Ok(b) => match crate::store::write_atomic(&self.dir.join("reads.json"), &b) {
                Ok(()) => self.reads_dirty = false,
                Err(e) => warn!(?e, "can't save thread reads"),
            },
            Err(e) => warn!(?e, "can't encode thread reads"),
        }
    }
}

/// `s`, at most `max` bytes, cut on a character boundary.
fn cut(mut s: String, max: usize) -> String {
    if s.len() > max {
        let mut at = max;
        while !s.is_char_boundary(at) {
            at -= 1;
        }
        s.truncate(at);
        s.push('…');
    }
    s
}

/// The `@names` in a message, lowercased, without the `@`.
pub fn mentions(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (i, _) in text.match_indices('@') {
        // Not inside a word (an email address).
        if text[..i].chars().next_back().is_some_and(|c| c.is_alphanumeric()) {
            continue;
        }
        let name: String =
            text[i + 1..].chars().take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.')).collect();
        let name = name.trim_end_matches('.').to_lowercase();
        if !name.is_empty() && !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

/// Whether `token` (from [`mentions`]) names this person: their name with
/// the spaces taken out, its first word, the part of an email address
/// before the `@`, or the login in their id.
pub fn names(token: &str, id: &str, name: &str) -> bool {
    let name = name.to_lowercase();
    let squashed: String = name.chars().filter(|c| !c.is_whitespace()).collect();
    let first = name.split_whitespace().next().unwrap_or("");
    let local = first.split('@').next().unwrap_or("");
    let login = id.rsplit(':').next().unwrap_or(id).split('@').next().unwrap_or("").to_lowercase();
    [squashed.as_str(), first, local, login.as_str()].iter().any(|n| !n.is_empty() && token == *n)
}

/// Whether a message calls the pane's agent.
pub fn calls_agent(tokens: &[String]) -> bool {
    tokens.iter().any(|t| t == "agent" || t == "claude")
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tmp(PathBuf);
    impl Tmp {
        fn new(name: &str) -> Self {
            let d = std::env::temp_dir().join(format!(
                "illogical-threads-{name}-{}-{}",
                std::process::id(),
                crate::store::now_ms()
            ));
            std::fs::create_dir_all(&d).unwrap();
            Tmp(d)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn msg(text: &str) -> ThreadMsg {
        ThreadMsg {
            id: 0,
            at: 1,
            who: "owner".into(),
            name: "Jake".into(),
            text: text.into(),
            quote: None,
            mentions: vec![],
            to_agent: false,
            agent: false,
        }
    }

    #[test]
    fn messages_and_reads_survive_reopening() {
        let dir = Tmp::new("reopen");
        let mut t = Threads::open(dir.path());
        let a = ThreadTarget::Pane(7);
        assert_eq!(t.post(a, msg("one")).unwrap().id, 1);
        assert_eq!(t.post(a, msg("two")).unwrap().id, 2);
        t.post(ThreadTarget::Session(1), msg("hi")).unwrap();
        assert!(t.mark_read("owner", a, 1));
        assert!(!t.mark_read("owner", a, 1), "reads never go back or repeat");
        assert!(t.mark_read("owner", a, 99), "capped at the last message");
        t.save();
        let t = Threads::open(dir.path());
        assert_eq!(t.get(a).iter().map(|m| m.text.as_str()).collect::<Vec<_>>(), ["one", "two"]);
        assert_eq!(t.get(ThreadTarget::Session(1)).len(), 1);
        assert_eq!(t.read_upto("owner", a), 2);
    }

    #[test]
    fn a_torn_last_line_is_skipped() {
        let dir = Tmp::new("torn");
        let mut t = Threads::open(dir.path());
        t.post(ThreadTarget::Pane(1), msg("ok")).unwrap();
        let f = dir.path().join("threads/pane-1.jsonl");
        let mut b = std::fs::read(&f).unwrap();
        b.extend_from_slice(b"{\"id\":2,\"at\"");
        std::fs::write(&f, b).unwrap();
        let mut t = Threads::open(dir.path());
        assert_eq!(t.get(ThreadTarget::Pane(1)).len(), 1);
        // The next message still numbers on from the last good one.
        assert_eq!(t.post(ThreadTarget::Pane(1), msg("next")).unwrap().id, 2);
    }

    #[test]
    fn long_text_is_cut_on_a_character() {
        let dir = Tmp::new("long");
        let mut t = Threads::open(dir.path());
        let m = t.post(ThreadTarget::Pane(1), msg(&"é".repeat(MAX_TEXT))).unwrap();
        assert!(m.text.len() <= MAX_TEXT + '…'.len_utf8());
        assert!(m.text.ends_with('…'));
    }

    #[test]
    fn mentions_and_names() {
        assert_eq!(mentions("@agent why? cc @Sam.Lee, mail jake@example.com"), ["agent", "sam.lee"]);
        assert!(names("sam", "tailnet:sam@example.com", "Sam Lee"));
        assert!(names("samlee", "account:abc", "Sam Lee"));
        assert!(names("octocat", "tailnet:octocat@github", "The Octocat"));
        assert!(!names("sa", "account:abc", "Sam Lee"));
        assert!(names("me", "owner", "me@example.com"));
        assert!(names("owner", "owner", "Jake"));
        assert!(calls_agent(&mentions("hey @Claude look")));
    }
}
