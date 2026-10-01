//! What survives the daemon: the layout, and each pane's output and
//! terminal state.
//!
//! ```text
//! $XDG_STATE_HOME/illogical/          0700
//!   layout.json                       sessions, tabs, splits, pane details
//!   panes/<id>/
//!     seg-<offset>.log                raw output; the name is the stream
//!                                     offset of its first byte
//!     index                           "<offset> resize <cols> <rows>",
//!                                     "<offset> restore <unix ms>"
//!     checkpoint                      "offset <n>\n" + engine checkpoint
//! ```
//!
//! The log is the truth. A checkpoint is a cache of the terminal at some
//! log offset, so a restore replays only the log after it; one that can't be
//! used is ignored and the log tail is replayed instead.
//!
//! Scrollback holds secrets (pasted tokens), so everything is private to the
//! user and bounded by retention.

use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use illogical_core::Mux;
use illogical_proto::{PaneId, Policy};
use serde::{Deserialize, Serialize};

pub const LAYOUT_VERSION: u32 = 1;
const SEGMENT_BYTES: u64 = 4 * 1024 * 1024;
pub const RETAIN_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Saved {
    pub version: u32,
    pub saved_at_ms: u64,
    pub mux: Mux,
    pub panes: BTreeMap<PaneId, PaneMeta>,
}

/// What a restore needs to know about a pane beyond its place in the layout.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PaneMeta {
    pub policy: Policy,
    pub cwd: Option<String>,
    pub command: Option<String>,
}

#[derive(Debug, Clone)]
pub struct StateDir {
    root: PathBuf,
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

fn private_dir(path: &Path) -> io::Result<()> {
    fs::DirBuilder::new().recursive(true).mode(0o700).create(path)
}

fn private_file() -> OpenOptions {
    let mut o = OpenOptions::new();
    o.mode(0o600);
    o
}

/// Write a file so that it is either entirely old or entirely new, even
/// across a crash or power loss.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = private_file().write(true).create(true).truncate(true).open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    if let Some(dir) = path.parent() {
        File::open(dir)?.sync_all()?;
    }
    Ok(())
}

impl StateDir {
    pub fn open(root: PathBuf) -> io::Result<Self> {
        private_dir(&root)?;
        private_dir(&root.join("panes"))?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn pane_dir(&self, pane: PaneId) -> PathBuf {
        self.root.join("panes").join(pane.to_string())
    }

    pub fn load_layout(&self) -> io::Result<Option<Saved>> {
        let bytes = match fs::read(self.root.join("layout.json")) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let saved: Saved = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        if saved.version != LAYOUT_VERSION {
            return Err(io::Error::other(format!("layout.json version {} (want {LAYOUT_VERSION})", saved.version)));
        }
        Ok(Some(saved))
    }

    pub fn save_layout(&self, saved: &Saved) -> io::Result<()> {
        write_atomic(&self.root.join("layout.json"), &serde_json::to_vec_pretty(saved).map_err(io::Error::other)?)
    }

    /// Pane directories with no pane in the layout (closed while the daemon
    /// was down, or left by a crash).
    pub fn remove_strays(&self, keep: &[PaneId]) {
        let Ok(entries) = fs::read_dir(self.root.join("panes")) else {
            return;
        };
        for e in entries.flatten() {
            let id = e.file_name().to_str().and_then(|n| n.parse::<PaneId>().ok());
            if id.is_some_and(|id| !keep.contains(&id)) {
                let _ = fs::remove_dir_all(e.path());
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Resize { cols: u16, rows: u16 },
    Restore { at_ms: u64 },
}

/// One pane's output on disk: append-only segments, an index of events by
/// offset, and the latest checkpoint.
pub struct PaneLog {
    dir: PathBuf,
    /// Start offsets of the segments, oldest first.
    segments: Vec<u64>,
    current: Option<File>,
    end: u64,
    retain: u64,
}

impl PaneLog {
    pub fn open(dir: PathBuf) -> io::Result<Self> {
        Self::open_with(dir, RETAIN_BYTES)
    }

    pub fn open_with(dir: PathBuf, retain: u64) -> io::Result<Self> {
        private_dir(&dir)?;
        let mut segments: Vec<u64> = fs::read_dir(&dir)?
            .flatten()
            .filter_map(|e| e.file_name().to_str()?.strip_prefix("seg-")?.strip_suffix(".log")?.parse().ok())
            .collect();
        segments.sort_unstable();
        let end = match segments.last() {
            Some(start) => start + fs::metadata(dir.join(seg_name(*start)))?.len(),
            None => 0,
        };
        Ok(Self { dir, segments, current: None, end, retain })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Stream offset just past the last byte written.
    pub fn end(&self) -> u64 {
        self.end
    }

    /// Oldest offset still on disk.
    pub fn start(&self) -> u64 {
        self.segments.first().copied().unwrap_or(self.end)
    }

    pub fn append(&mut self, data: &[u8]) -> io::Result<()> {
        let mut data = data;
        while !data.is_empty() {
            let seg_start = match self.segments.last() {
                Some(s) if self.end - s < SEGMENT_BYTES => *s,
                _ => {
                    self.segments.push(self.end);
                    self.current = None;
                    self.end
                }
            };
            if self.current.is_none() {
                let path = self.dir.join(seg_name(seg_start));
                self.current = Some(private_file().create(true).append(true).open(&path)?);
            }
            let room = (SEGMENT_BYTES - (self.end - seg_start)) as usize;
            let (now, rest) = data.split_at(room.min(data.len()));
            self.current.as_mut().unwrap().write_all(now)?;
            self.end += now.len() as u64;
            data = rest;
        }
        self.enforce_retention();
        Ok(())
    }

    fn enforce_retention(&mut self) {
        while self.segments.len() > 1 && self.end - self.segments[0] > self.retain {
            let _ = fs::remove_file(self.dir.join(seg_name(self.segments[0])));
            self.segments.remove(0);
        }
    }

    pub fn sync(&mut self) -> io::Result<()> {
        if let Some(f) = &self.current {
            f.sync_data()?;
        }
        Ok(())
    }

    /// The bytes from `from` (clamped to what is still on disk) to the end.
    pub fn read_from(&self, from: u64) -> io::Result<(u64, Vec<u8>)> {
        let from = from.clamp(self.start(), self.end);
        let mut out = Vec::with_capacity((self.end - from) as usize);
        for (i, start) in self.segments.iter().enumerate() {
            let next = self.segments.get(i + 1).copied().unwrap_or(self.end);
            if next <= from {
                continue;
            }
            let mut f = File::open(self.dir.join(seg_name(*start)))?;
            f.seek(SeekFrom::Start(from.saturating_sub(*start)))?;
            f.read_to_end(&mut out)?;
        }
        Ok((from, out))
    }

    pub fn record(&mut self, offset: u64, event: Event) -> io::Result<()> {
        let line = match event {
            Event::Resize { cols, rows } => format!("{offset} resize {cols} {rows}\n"),
            Event::Restore { at_ms } => format!("{offset} restore {at_ms}\n"),
        };
        let path = self.dir.join("index");
        private_file().create(true).append(true).open(&path)?.write_all(line.as_bytes())
    }

    pub fn events(&self) -> Vec<(u64, Event)> {
        let Ok(text) = fs::read_to_string(self.dir.join("index")) else {
            return vec![];
        };
        text.lines()
            .filter_map(|l| {
                let mut w = l.split_whitespace();
                let offset = w.next()?.parse().ok()?;
                let event = match w.next()? {
                    "resize" => Event::Resize { cols: w.next()?.parse().ok()?, rows: w.next()?.parse().ok()? },
                    "restore" => Event::Restore { at_ms: w.next()?.parse().ok()? },
                    _ => return None,
                };
                Some((offset, event))
            })
            .collect()
    }

    pub fn save_checkpoint(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        self.sync()?;
        let mut out = format!("offset {offset}\n").into_bytes();
        out.extend_from_slice(bytes);
        write_atomic(&self.dir.join("checkpoint"), &out)
    }

    pub fn load_checkpoint(&self) -> Option<(u64, Vec<u8>)> {
        let bytes = fs::read(self.dir.join("checkpoint")).ok()?;
        let nl = bytes.iter().position(|b| *b == b'\n')?;
        let offset = std::str::from_utf8(&bytes[..nl]).ok()?.strip_prefix("offset ")?.parse().ok()?;
        Some((offset, bytes[nl + 1..].to_vec()))
    }

    /// Forget all history; the stream carries on from the same offset.
    pub fn purge(&mut self) -> io::Result<()> {
        self.current = None;
        for s in self.segments.drain(..) {
            let _ = fs::remove_file(self.dir.join(seg_name(s)));
        }
        let _ = fs::remove_file(self.dir.join("index"));
        let _ = fs::remove_file(self.dir.join("checkpoint"));
        Ok(())
    }

    pub fn remove(self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn seg_name(start: u64) -> String {
    format!("seg-{start:020}.log")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("illogical-store-{name}-{}-{}", std::process::id(), now_ms()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn log_appends_rotates_reads_and_reopens() {
        let dir = tmp("log");
        let mut log = PaneLog::open(dir.clone()).unwrap();
        let chunk = vec![b'a'; 3 * 1024 * 1024];
        log.append(&chunk).unwrap();
        log.append(b"hello").unwrap();
        log.append(&chunk).unwrap();
        assert_eq!(log.end(), 6 * 1024 * 1024 + 5);
        assert_eq!(log.segments, vec![0, SEGMENT_BYTES]);
        let (from, bytes) = log.read_from(3 * 1024 * 1024).unwrap();
        assert_eq!(from, 3 * 1024 * 1024);
        assert_eq!(&bytes[..5], b"hello");
        assert_eq!(bytes.len() as u64, log.end() - from);
        drop(log);
        let log = PaneLog::open(dir.clone()).unwrap();
        assert_eq!(log.end(), 6 * 1024 * 1024 + 5);
        let mode = fs::metadata(dir.join(seg_name(0))).unwrap().permissions();
        assert_eq!(std::os::unix::fs::PermissionsExt::mode(&mode) & 0o777, 0o600);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn retention_drops_oldest_segments() {
        let dir = tmp("retain");
        let mut log = PaneLog::open_with(dir.clone(), SEGMENT_BYTES * 2).unwrap();
        for _ in 0..5 {
            log.append(&vec![b'x'; SEGMENT_BYTES as usize]).unwrap();
        }
        assert_eq!(log.segments.len(), 2);
        assert_eq!(log.start(), 3 * SEGMENT_BYTES);
        assert_eq!(log.read_from(0).unwrap().0, 3 * SEGMENT_BYTES, "clamped to what is kept");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn index_and_checkpoint_round_trip() {
        let dir = tmp("index");
        let mut log = PaneLog::open(dir.clone()).unwrap();
        log.record(0, Event::Resize { cols: 80, rows: 24 }).unwrap();
        log.record(42, Event::Restore { at_ms: 7 }).unwrap();
        assert_eq!(log.events(), vec![(0, Event::Resize { cols: 80, rows: 24 }), (42, Event::Restore { at_ms: 7 })]);
        log.save_checkpoint(42, b"state\nbytes").unwrap();
        assert_eq!(log.load_checkpoint(), Some((42, b"state\nbytes".to_vec())));
        log.append(b"abc").unwrap();
        log.purge().unwrap();
        assert_eq!(log.load_checkpoint(), None);
        assert!(log.events().is_empty());
        log.append(b"def").unwrap();
        assert_eq!(log.read_from(0).unwrap(), (3, b"def".to_vec()), "offsets carry on after a purge");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn layout_saves_atomically_and_privately() {
        let dir = tmp("layout");
        let state = StateDir::open(dir.clone()).unwrap();
        assert_eq!(state.load_layout().unwrap(), None);
        let mut mux = Mux::new();
        mux.apply(illogical_core::Intent::NewSession { name: None, from_pane: None }).unwrap();
        let saved = Saved {
            version: LAYOUT_VERSION,
            saved_at_ms: 1,
            mux,
            panes: [(1, PaneMeta { policy: Policy::Rerun { confirm: true }, cwd: Some("/tmp".into()), command: None })]
                .into(),
        };
        state.save_layout(&saved).unwrap();
        assert_eq!(state.load_layout().unwrap(), Some(saved));
        let mode = fs::metadata(&dir).unwrap().permissions();
        assert_eq!(std::os::unix::fs::PermissionsExt::mode(&mode) & 0o777, 0o700);
        fs::remove_dir_all(dir).unwrap();
    }
}
