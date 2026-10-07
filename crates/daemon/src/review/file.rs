//! File blocks (M11): one file on the block's host, read-only, at a line,
//! followed live while drawn.
//!
//! Config `{path, line?}`. It reads through M7's `fs` ([`Target`]), so the
//! same places are refused as for `/api/fs`, and a machine's file is read
//! through its provider (no symlinks followed there). At most
//! [`READ_MAX`] bytes are shown (cut at a line); a binary file says so.
//! While some client draws it, it looks at the file's size and time every
//! second (every 3s on a machine) and reads it again when they change,
//! keeping the marked line on the same text when lines above it change.
//!
//! Methods (editor role): `goto {line}`, `refresh`, and (the owner only)
//! `open {path, line?}` to show another file on the same host.

use std::{
    sync::{Arc, Mutex, Weak},
    time::Duration,
};

use arugula_proto::{
    BlockType,
    fs::{FsKind, READ_MAX},
};
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::Live;
use crate::{
    block::{Block, BlockCtx, Summary, no_method},
    fs::Target,
    store::now_ms,
};

const POLL: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Default, Deserialize)]
struct Config {
    path: String,
    #[serde(default)]
    line: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize)]
struct State {
    /// As given; `real` once read (symlinks resolved, `~` expanded).
    path: String,
    real: Option<String>,
    name: String,
    /// The marked line (from 1), if any.
    line: Option<u32>,
    /// Bumped when someone moves the mark (`goto`, `open`), so clients
    /// scroll to it; following edits doesn't.
    jump: u64,
    size: u64,
    mtime_ms: u64,
    text: String,
    /// Bumped when the text changes.
    rev: u64,
    /// More than [`READ_MAX`]: only the start is shown.
    truncated: bool,
    binary: bool,
    loading: bool,
    error: Option<String>,
    /// Following the file now (someone draws it).
    watching: bool,
    updated_ms: u64,
}

pub struct FileView {
    ctx: BlockCtx,
    me: Weak<FileView>,
    files: Result<Target, String>,
    state: Mutex<State>,
    /// The size and time last read, to tell a change.
    seen: Mutex<Option<(u64, u64)>>,
    live: Live,
    reading: tokio::sync::Mutex<()>,
}

impl FileView {
    pub fn create(ctx: BlockCtx, config: Value) -> Result<Arc<dyn Block>, String> {
        let config: Config = serde_json::from_value(config).map_err(|e| format!("file config: {e}"))?;
        if config.path.is_empty() {
            return Err("a file block needs a path".into());
        }
        let files = ctx.files();
        if let Ok(Target::Local(scope)) = &files
            && !ctx.restoring
        {
            scope.resolve(&config.path).map_err(|e| e.to_string())?;
        }
        let state = State {
            name: name_of(&config.path),
            path: config.path.clone(),
            line: config.line.filter(|l| *l > 0),
            jump: 1,
            loading: !ctx.restoring,
            ..Default::default()
        };
        super::log(&ctx, &json!({ "e": "view", "path": config.path, "line": config.line }));
        let restoring = ctx.restoring;
        let f = Arc::new_cyclic(|me| Self {
            ctx,
            me: me.clone(),
            files,
            state: Mutex::new(state),
            seen: Mutex::new(None),
            live: Live::default(),
            reading: tokio::sync::Mutex::new(()),
        });
        if !restoring {
            let me = f.clone();
            f.ctx.rt.spawn(async move { me.load(true).await });
        }
        Ok(f)
    }

    /// Look at the file; read it if it changed (or `force`).
    async fn load(&self, force: bool) {
        let _one = self.reading.lock().await;
        if self.live.closed() {
            return;
        }
        let files = match &self.files {
            Ok(f) => f.clone(),
            Err(e) => return self.failed(e.clone()),
        };
        let path = self.state.lock().unwrap().path.clone();
        let entry = match files.stat(&path).await {
            Ok(e) => e,
            Err(e) => return self.failed(e.to_string()),
        };
        if entry.kind == FsKind::Directory {
            return self.failed(format!("{}: a directory", entry.path));
        }
        let key = (entry.size, entry.mtime_ms);
        if !force && *self.seen.lock().unwrap() == Some(key) && self.state.lock().unwrap().error.is_none() {
            return;
        }
        let (bytes, size) = match files.read(&path, 0, READ_MAX).await {
            Ok(r) => r,
            Err(e) => return self.failed(e.to_string()),
        };
        *self.seen.lock().unwrap() = Some(key);
        let binary = bytes.iter().take(8000).any(|b| *b == 0);
        let truncated = size > bytes.len() as u64;
        let mut text = if binary { String::new() } else { String::from_utf8_lossy(&bytes).into_owned() };
        if truncated && let Some(cut) = text.rfind('\n') {
            text.truncate(cut + 1);
        }
        {
            let mut st = self.state.lock().unwrap();
            let changed = st.text != text || st.binary != binary;
            if changed {
                st.line = st.line.map(|l| follow(&st.text, &text, l));
                st.text = text;
                st.rev += 1;
            }
            if !changed && st.error.is_none() && !st.loading && st.size == size {
                return;
            }
            st.real = Some(entry.path.clone());
            st.name = name_of(&entry.path);
            (st.size, st.mtime_ms, st.binary, st.truncated) = (size, entry.mtime_ms, binary, truncated);
            st.error = None;
            st.loading = false;
            st.updated_ms = now_ms();
        }
        self.ctx.changed();
    }

    fn failed(&self, why: String) {
        {
            let mut st = self.state.lock().unwrap();
            if st.error.as_deref() == Some(why.as_str()) && !st.loading {
                return;
            }
            st.error = Some(why);
            st.loading = false;
            st.updated_ms = now_ms();
        }
        *self.seen.lock().unwrap() = None;
        self.ctx.changed();
    }

    fn goto(&self, line: Option<u32>) {
        {
            let mut st = self.state.lock().unwrap();
            st.line = line.filter(|l| *l > 0);
            st.jump += 1;
        }
        self.ctx.changed();
    }

    fn watch(&self, round: u64) {
        let Some(me) = self.me.upgrade() else { return };
        let local = matches!(self.files, Ok(Target::Local(_)));
        let every = super::every(local, POLL);
        self.ctx.rt.spawn(async move {
            while me.live.on(round) {
                me.load(false).await;
                tokio::time::sleep(every).await;
            }
        });
    }
}

impl Block for FileView {
    fn kind(&self) -> BlockType {
        BlockType::File
    }

    fn config(&self) -> Value {
        let st = self.state.lock().unwrap();
        json!({ "path": st.path, "line": st.line })
    }

    fn state(&self) -> Value {
        serde_json::to_value(&*self.state.lock().unwrap()).unwrap_or_default()
    }

    fn text(&self) -> String {
        let st = self.state.lock().unwrap();
        match (&st.error, st.binary) {
            (Some(e), _) => format!("{}: {e}\n", st.path),
            (None, true) => format!("{}: a binary file ({} bytes)\n", st.path, st.size),
            (None, false) => st.text.clone(),
        }
    }

    fn call(&self, method: &str, args: Value) -> BoxFuture<'static, Result<Value, String>> {
        self.call_by(method, args, None)
    }

    fn call_by(&self, method: &str, args: Value, by: Option<&str>) -> BoxFuture<'static, Result<Value, String>> {
        let me = self.me.upgrade();
        let line = || args["line"].as_u64().map(|l| l.min(u32::MAX as u64) as u32);
        match method {
            "goto" => {
                self.goto(line());
                let at = self.state.lock().unwrap().line;
                Box::pin(async move { Ok(json!({ "line": at })) })
            }
            "open" => {
                // Someone else could read anything of the owner's here.
                let refused = by.map(|who| format!("only the owner can point it at another file ({who} can't)"));
                let path = args["path"].as_str().filter(|p| !p.is_empty()).map(str::to_owned);
                let line = line();
                Box::pin(async move {
                    if let Some(e) = refused {
                        return Err(e);
                    }
                    let me = me.ok_or("closed")?;
                    let path = path.ok_or("open needs {\"path\": …}")?;
                    if let Ok(Target::Local(scope)) = &me.files {
                        scope.resolve(&path).map_err(|e| e.to_string())?;
                    }
                    super::log(&me.ctx, &json!({ "e": "view", "path": path, "line": line }));
                    {
                        let mut st = me.state.lock().unwrap();
                        st.name = name_of(&path);
                        (st.path, st.real, st.text, st.loading) = (path, None, String::new(), true);
                        st.rev += 1;
                    }
                    me.goto(line);
                    me.load(true).await;
                    let st = me.state.lock().unwrap();
                    match &st.error {
                        Some(e) => Err(e.clone()),
                        None => Ok(json!({ "path": st.real, "line": st.line, "size": st.size })),
                    }
                })
            }
            "refresh" => Box::pin(async move {
                let me = me.ok_or("closed")?;
                me.load(true).await;
                let st = me.state.lock().unwrap();
                match &st.error {
                    Some(e) => Err(e.clone()),
                    None => Ok(json!({ "size": st.size })),
                }
            }),
            "state" => {
                let s = self.state();
                Box::pin(async move { Ok(s) })
            }
            m => {
                let e = no_method(BlockType::File, m);
                Box::pin(async move { Err(e) })
            }
        }
    }

    fn drawn(&self, on: bool) {
        if let Some(round) = self.live.set(on) {
            self.watch(round);
        }
        let mut st = self.state.lock().unwrap();
        if st.watching != self.live.drawn() {
            st.watching = self.live.drawn();
            drop(st);
            self.ctx.changed();
        }
    }

    fn close(&self) {
        self.live.close();
    }

    fn summary(&self) -> Summary {
        let st = self.state.lock().unwrap();
        let path = st.real.clone().unwrap_or_else(|| st.path.clone());
        let dir = std::path::Path::new(&path).parent().map(|p| p.display().to_string());
        let local = matches!(self.files, Ok(Target::Local(_)));
        Summary {
            project: dir.as_deref().and_then(|d| super::project(d, local)),
            cwd: dir,
            title: Some(match st.line {
                Some(l) => format!("{}:{l}", st.name),
                None => st.name.clone(),
            }),
            file: Some(path),
            ..Default::default()
        }
    }
}

fn name_of(path: &str) -> String {
    path.rsplit('/').find(|s| !s.is_empty()).unwrap_or(path).to_owned()
}

/// Where line `line` (from 1) of `old` is in `new`: the same if it's
/// before what changed, moved by as many lines as were added or removed if
/// after it; inside the change, the same text nearest where it was, else
/// the change's start.
pub fn follow(old: &str, new: &str, line: u32) -> u32 {
    let (a, b): (Vec<&str>, Vec<&str>) = (old.lines().collect(), new.lines().collect());
    let at = line as usize - 1;
    if a.is_empty() || at >= a.len() {
        return line.min(b.len().max(1) as u32);
    }
    let head = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let tail = a.iter().rev().zip(b.iter().rev()).take_while(|(x, y)| x == y).count().min(a.len().min(b.len()) - head);
    if at < head {
        return line;
    }
    if at >= a.len() - tail {
        return (at + b.len() - a.len() + 1) as u32;
    }
    // In the change: the same text, nearest its old place.
    let want = a[at];
    let (lo, hi) = (head, b.len() - tail);
    let best = (lo..hi).filter(|i| b[*i] == want).min_by_key(|i| i.abs_diff(at));
    (best.unwrap_or(lo.min(b.len().saturating_sub(1))) + 1) as u32
}

#[cfg(test)]
mod tests {
    use super::follow;

    #[test]
    fn the_mark_follows_its_line() {
        let old = "a\nb\nc\nd\ne\n";
        // Lines added above: it moves down.
        assert_eq!(follow(old, "x\ny\na\nb\nc\nd\ne\n", 4), 6);
        // Removed above: up.
        assert_eq!(follow(old, "c\nd\ne\n", 4), 2);
        // Changed below: stays.
        assert_eq!(follow(old, "a\nb\nc\nd\nE\nF\n", 3), 3);
        // Its own line moved within the change: found by its text.
        assert_eq!(follow(old, "a\nnew\nd\nc\ne\n", 3), 4);
        // Its line gone: the change's start.
        assert_eq!(follow(old, "a\nX\ne\n", 3), 2);
        // Past the end: clamped.
        assert_eq!(follow(old, "a\n", 5), 1);
        assert_eq!(follow("", "a\nb\n", 1), 1);
    }
}
