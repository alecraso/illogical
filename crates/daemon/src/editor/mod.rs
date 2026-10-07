//! Editor blocks (M27): VS Code, as code-server, in a folder on the block's
//! machine (this host, or the VM it was opened from), drawn like a browser
//! block.
//!
//! - **One server per machine** (`server.rs`), shared by every editor block
//!   there, started when a block needs it and stopped by itself when idle.
//! - **Each block has its own site** (`sites.rs`), so its own origin, and
//!   that site is the only way in: the server has no auth of its own and
//!   listens on a 0600 Unix socket, so the daemon's access checks are its
//!   auth. Like browser blocks on ports, they're the owner's.
//! - **Each block has a workspace** (`<state>/editor/w/<id>/`) that names
//!   the block (`arugula.block`), so Arugula's extension in that window
//!   knows which block it is and reports the active file, the cursor and
//!   the lines around it (`report`). That's the swarm's preview, and what
//!   comes back after a restart.
//! - **Opening a file** is part of the page's address (VS Code's
//!   `payload=[["openFile", …]]`), so a block made for a file, or brought
//!   back after a restart, asks the server to open it as it loads.
//!
//! In a VM there's no daemon socket for the extension to reach, so the
//! block shows the file it was opened on and doesn't follow the cursor.

pub mod link;
pub mod presence;
pub mod server;
pub mod vsix;

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
};

use arugula_proto::{Attention, BlockType, Project, WorkKind};
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::info;

use self::server::{On, Server, Status};
use crate::{
    block::{Block, BlockCtx, Summary, no_method},
    ports::{Service, Target},
    sites::{self, Report, Site},
};

/// At most this many lines of context come with a report.
const MAX_LINES: usize = 12;
/// First in the block's pages: storage in memory where the browser refuses
/// a third-party frame its own (#69).
const STORAGE_JS: &str = include_str!("storage.js");

#[derive(Debug, Clone, Default, Deserialize)]
struct Config {
    /// What to open: a folder, or a file (its project is the folder).
    #[serde(default)]
    path: Option<String>,
    /// Where it was: the folder, the file and line it showed.
    #[serde(default)]
    folder: Option<String>,
    #[serde(default)]
    file: Option<String>,
    #[serde(default)]
    line: Option<u32>,
    /// Its git repository's top, found on its machine.
    #[serde(default)]
    project: Option<String>,
    /// The random part of its site's name (dev scheme).
    #[serde(default)]
    key: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
struct State {
    /// The folder it's open on, and its name.
    folder: String,
    name: String,
    /// The active file: relative to the folder when it's inside it.
    file: Option<String>,
    /// ...and where, with the lines around the cursor (from `top`).
    line: Option<u32>,
    col: Option<u32>,
    top: Option<u32>,
    lines: Vec<String>,
    /// Files with unsaved changes.
    dirty: u32,
    /// What the frame loads (stays put while the block lives, so the
    /// frame isn't reloaded as the file changes).
    src: String,
    /// Bumped when the frame should load again (the server wasn't there
    /// when it tried).
    reloads: u64,
    /// Its server, as `{"is": "running"}` and so on.
    server: Option<Status>,
    /// The VM it's on (none for this host).
    machine: Option<String>,
    error: Option<String>,
}

pub struct Editor {
    ctx: BlockCtx,
    me: Weak<Editor>,
    state: Mutex<State>,
    /// The active file's whole path (config keeps it for a restart).
    path: Mutex<Option<String>>,
    /// The git repository it's in, on its machine.
    project: Mutex<Option<Project>>,
    key: String,
    server: Arc<Server>,
    site: Arc<Site>,
    /// The workspace file that names this block (this host only).
    workspace: Option<PathBuf>,
    /// A load found no server: reload the frame once it's up.
    missed: AtomicBool,
    closed: AtomicBool,
    /// Its window's Arugula extension, connected (M28).
    link: Mutex<Option<Arc<link::Link>>>,
}

impl Editor {
    pub fn create(ctx: BlockCtx, config: Value) -> Result<Arc<dyn Block>, String> {
        let config: Config = serde_json::from_value(config).map_err(|e| format!("editor config: {e}"))?;
        let settings = server::settings().ok_or("editor blocks are off here")?;
        let sites = sites::get().ok_or("editor blocks need block sites: start arugulad with --block-listen")?;
        let on = match (&ctx.sprite, &ctx.provider) {
            (None, _) => On::Here,
            (Some(sprite), Some(provider)) => On::Vm { provider: provider.clone(), sprite: sprite.clone() },
            (Some(_), None) => return Err("this block's machine can't be reached: VM panes aren't set up".into()),
        };
        let here = matches!(on, On::Here);
        // Where: as it was, or from a path (found now on this host; in a VM
        // once the VM answers).
        let place = match (&config.folder, &config.path) {
            (Some(folder), _) => Some((folder.clone(), config.file.clone(), config.project.clone())),
            (None, Some(path)) if here => Some(resolve_here(path)?),
            (None, Some(_)) => None,
            (None, None) => return Err("an editor block needs a folder or a file".into()),
        };
        let key = config
            .key
            .filter(|k| k.len() >= 16 && k.chars().all(|c| c.is_ascii_alphanumeric()))
            .unwrap_or_else(crate::browser::new_key);
        let server = server::get(&settings, on, ctx.env.clone());
        let workspace = here.then(|| settings.dir.join("w").join(ctx.id.to_string()));
        let ed = Arc::new_cyclic(|me: &Weak<Editor>| {
            let report = me.clone();
            let site = sites.open(ctx.id, &key, move |r| {
                if let Some(e) = report.upgrade() {
                    e.reported(r);
                }
            });
            // The script before the target: a page served in between would
            // come without it (#69).
            site.set_head_script(STORAGE_JS);
            site.set_target(Target::Service(server.clone() as Arc<dyn Service>));
            Editor {
                state: Mutex::new(State { machine: ctx.sprite.clone(), ..State::default() }),
                path: Mutex::new(None),
                project: Mutex::new(None),
                ctx,
                me: me.clone(),
                key,
                server,
                site,
                workspace,
                missed: AtomicBool::new(false),
                closed: AtomicBool::new(false),
                link: Mutex::new(None),
            }
        });
        match place {
            Some((folder, file, project)) => {
                if let Err(e) = ed.locate(folder, file, config.line, project) {
                    ed.close();
                    return Err(e);
                }
            }
            None => {
                let (e, path) = (ed.clone(), config.path.clone().unwrap_or_default());
                ed.ctx.rt.spawn(async move {
                    let found = resolve_vm(&e.ctx, &path).await;
                    if let Err(why) = found.and_then(|(folder, file, project)| e.locate(folder, file, None, project)) {
                        e.failed(why);
                    }
                });
            }
        }
        ed.watch_server();
        // A new block wants its server warm now; a restored one starts it
        // when someone looks (or finds the one the last daemon left).
        let s = ed.server.clone();
        if ed.ctx.restoring {
            ed.ctx.rt.spawn(async move { s.check().await });
        } else {
            ed.ctx.rt.spawn(async move {
                let _ = s.ensure().await;
            });
        }
        Ok(ed)
    }

    /// Open on `folder`, showing `file` (a whole path) at `line`. Its
    /// project: the git repository `project` names (in a VM), or the one
    /// it's in here.
    fn locate(
        &self,
        folder: String,
        file: Option<String>,
        line: Option<u32>,
        project: Option<String>,
    ) -> Result<(), String> {
        let name = Path::new(&folder).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or(folder.clone());
        // The page: this block's workspace on this host (it names the
        // block), the folder in a VM; and the file, opened as it loads.
        let mut query = match &self.workspace {
            Some(dir) => {
                let ws = dir.join(format!("{}.code-workspace", safe(&name)));
                std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
                let text = json!({ "folders": [{ "path": folder }], "settings": { "arugula.block": self.ctx.id } });
                crate::store::write_atomic(&ws, text.to_string().as_bytes()).map_err(|e| e.to_string())?;
                format!("/?workspace={}", enc(&ws.display().to_string()))
            }
            None => format!("/?folder={}", enc(&folder)),
        };
        if let Some(f) = &file {
            query.push_str(&format!("&payload={}", enc(&open_payload(&self.site.origin, f, line))));
        }
        let src = self.site.url(&query);
        info!(block = self.ctx.id, folder, file = file.as_deref().unwrap_or(""), "editor");
        {
            let mut st = self.state.lock().unwrap();
            st.file = file.as_deref().map(|f| relative(&folder, f));
            st.line = line.filter(|_| file.is_some());
            st.folder = folder;
            st.name = name;
            st.src = src;
        }
        *self.path.lock().unwrap() = file;
        let folder = self.state.lock().unwrap().folder.clone();
        *self.project.lock().unwrap() = match (&self.workspace, project) {
            (Some(_), _) => crate::classify::project(&folder),
            (None, Some(root)) => Some(Project {
                name: Path::new(&root).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                root,
            }),
            (None, None) => None,
        };
        self.ctx.changed();
        Ok(())
    }

    fn failed(&self, why: String) {
        self.state.lock().unwrap().error = Some(why.clone());
        self.ctx.attention(Attention::NeedsInput, why);
        self.ctx.changed();
    }

    /// Follow the server's status: its attention, and a reload of the
    /// frame if it loaded before the server was there.
    fn watch_server(&self) {
        let mut rx = self.server.subscribe();
        let me = self.me.clone();
        self.ctx.rt.spawn(async move {
            loop {
                let status = rx.borrow_and_update().clone();
                let Some(e) = me.upgrade() else { return };
                if e.closed.load(Ordering::Relaxed) {
                    return;
                }
                e.server_is(status);
                drop(e);
                if rx.changed().await.is_err() {
                    return;
                }
            }
        });
    }

    fn server_is(&self, status: Status) {
        let error = self.state.lock().unwrap().error.is_some();
        match &status {
            Status::Fetching { .. } => self.ctx.attention(Attention::Working, "downloading code-server"),
            Status::Starting => self.ctx.attention(Attention::Working, "starting code-server"),
            Status::Running | Status::Stopped if !error => self.ctx.attention(Attention::Idle, "ready"),
            Status::Failed { error } => self.ctx.attention(Attention::NeedsInput, format!("VS Code: {error}")),
            _ => {}
        }
        {
            let mut st = self.state.lock().unwrap();
            if status == Status::Running && self.missed.swap(false, Ordering::Relaxed) {
                st.reloads += 1;
            }
            st.server = Some(status);
        }
        self.ctx.changed();
    }

    /// What the proxy saw.
    fn reported(&self, r: Report) {
        if let Report::Unreachable(_) = r {
            self.missed.store(true, Ordering::Relaxed);
        }
    }

    /// Its window says where its cursor is (M28's `peek`; M27's `report`
    /// said the same).
    fn peeked(&self, p: &link::Peek) {
        {
            let mut st = self.state.lock().unwrap();
            st.file = p.file.as_deref().map(|f| relative(&st.folder, f));
            st.line = p.line;
            st.col = p.col;
            st.top = p.top;
            st.lines = p.lines.clone();
            st.dirty = p.dirty;
        }
        *self.path.lock().unwrap() = p.file.clone();
        self.ctx.changed();
    }

    /// The extension's report (M27): the active file and the cursor.
    fn report(&self, args: &Value) -> Result<Value, String> {
        let file = args["file"].as_str().map(str::to_owned);
        let n = |k: &str| args[k].as_u64().map(|v| v.min(u32::MAX as u64) as u32);
        let lines: Vec<String> = args["lines"]
            .as_array()
            .map(|a| {
                a.iter().take(MAX_LINES).filter_map(|l| l.as_str()).map(|l| l.chars().take(240).collect()).collect()
            })
            .unwrap_or_default();
        {
            let mut st = self.state.lock().unwrap();
            st.file = file.as_deref().map(|f| relative(&st.folder, f));
            st.line = n("line").filter(|_| file.is_some());
            st.col = n("col").filter(|_| file.is_some());
            st.top = n("top").filter(|_| file.is_some());
            st.lines = if file.is_some() { lines } else { vec![] };
            st.dirty = n("dirty").unwrap_or(0);
        }
        *self.path.lock().unwrap() = file;
        self.ctx.changed();
        Ok(json!({}))
    }
}

impl Block for Editor {
    fn kind(&self) -> BlockType {
        BlockType::Editor
    }

    fn config(&self) -> Value {
        let st = self.state.lock().unwrap();
        json!({
            "folder": st.folder,
            "file": *self.path.lock().unwrap(),
            "line": st.line,
            "project": self.project.lock().unwrap().as_ref().map(|p| p.root.clone()),
            "key": self.key,
        })
    }

    fn state(&self) -> Value {
        serde_json::to_value(&*self.state.lock().unwrap()).unwrap_or_default()
    }

    fn text(&self) -> String {
        let st = self.state.lock().unwrap();
        match (&st.file, st.line) {
            (Some(f), Some(l)) => format!("{f}:{l}\n{}\n", st.lines.join("\n")),
            (Some(f), None) => format!("{f}\n"),
            _ => format!("{}\n", st.folder),
        }
    }

    fn summary(&self) -> Summary {
        let st = self.state.lock().unwrap();
        Summary {
            work: Some(WorkKind::Editor),
            cwd: (!st.folder.is_empty()).then(|| st.folder.clone()),
            project: self.project.lock().unwrap().clone(),
            file: st.file.clone(),
            title: Some(match &st.file {
                Some(f) => format!("{} — {}", f.rsplit('/').next().unwrap_or(f), st.name),
                None => st.name.clone(),
            }),
            editor: self.link.lock().unwrap().as_ref().map(|l| l.info()),
        }
    }

    fn link(&self) -> Option<Arc<link::Link>> {
        self.link.lock().unwrap().clone()
    }

    fn attach(&self, l: Arc<link::Link>) -> bool {
        if self.closed.load(Ordering::Relaxed) {
            return false;
        }
        let me = self.me.clone();
        l.on_peek(move |p| {
            if let Some(e) = me.upgrade() {
                e.peeked(p);
            }
        });
        l.bind(self.ctx.id, self.ctx.sink());
        // A reload of the window connects again before the old one has gone.
        *self.link.lock().unwrap() = Some(l);
        self.ctx.changed();
        true
    }

    fn detach(&self, l: &Arc<link::Link>) {
        let mut cur = self.link.lock().unwrap();
        if cur.as_ref().is_some_and(|c| Arc::ptr_eq(c, l)) {
            *cur = None;
            drop(cur);
            self.ctx.changed();
        }
    }

    fn call(&self, method: &str, args: Value) -> BoxFuture<'static, Result<Value, String>> {
        let result = match method {
            "report" => self.report(&args),
            "continue" => match self.link.lock().unwrap().clone() {
                Some(l) => l.resume().map(|()| json!({})),
                None => Err("its window isn't connected".into()),
            },
            // Try again after a failed start (offline, say).
            "start" => {
                let s = self.server.clone();
                self.ctx.rt.spawn(async move {
                    let _ = s.ensure().await;
                });
                Ok(json!({}))
            }
            "state" => Ok(self.state()),
            m => Err(no_method(BlockType::Editor, m)),
        };
        Box::pin(async move { result })
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Relaxed);
        if let Some(sites) = sites::get() {
            sites.close(self.ctx.id);
        }
        if let Some(dir) = &self.workspace {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// A path on this host: a folder opens as itself; a file opens in its
/// project (its git repository, else its directory).
fn resolve_here(path: &str) -> Result<(String, Option<String>, Option<String>), String> {
    let p = Path::new(path);
    if !p.is_absolute() {
        return Err(format!("{path}: not a whole path"));
    }
    let p = p.canonicalize().map_err(|e| format!("{path}: {e}"))?;
    if p.is_dir() {
        return Ok((p.display().to_string(), None, None));
    }
    let dir = p.parent().unwrap_or(Path::new("/")).display().to_string();
    let folder = crate::classify::project(&dir).map(|pr| pr.root).unwrap_or(dir);
    Ok((folder, Some(p.display().to_string()), None))
}

/// The same, on the block's VM (relative paths start at its home), with
/// the git repository it's in, if any.
async fn resolve_vm(ctx: &BlockCtx, path: &str) -> Result<(String, Option<String>, Option<String>), String> {
    let (Some(provider), Some(sprite)) = (&ctx.provider, &ctx.sprite) else {
        return Err("this block's machine can't be reached".into());
    };
    const FIND: &str = r#"case $1 in "~") set -- "$HOME" ;; "~/"*) set -- "$HOME/${1#??}" ;; esac
cd ~ && p=$(realpath -- "$1") || exit 2
if [ -d "$p" ]; then d=$p f=-; elif [ -e "$p" ]; then d=$(dirname -- "$p") f=$p; else exit 2; fi
g=$d
while [ "$g" != / ] && [ ! -e "$g/.git" ]; do g=$(dirname -- "$g"); done
[ "$g" != / ] || g=-
[ "$f" = - ] || [ "$g" = - ] || d=$g
printf '%s\n%s\n%s\n' "$d" "$f" "$g""#;
    let (out, code) =
        provider.run(sprite, &["sh", "-c", FIND, "sh", path]).await.map_err(|e| format!("{path}: {e}"))?;
    let out = String::from_utf8_lossy(&out);
    let lines: Vec<&str> = out.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let some = |s: &str| (s != "-").then(|| s.to_owned());
    match (code, lines.as_slice()) {
        (Some(0), [folder, file, git]) => Ok(((*folder).to_owned(), some(file), some(git))),
        _ => Err(format!("{path}: no such file or directory on {sprite}")),
    }
}

/// VS Code's way to open a file as the page loads: `openFile` with the
/// file on this window's remote (its page's host), at a line.
fn open_payload(origin: &str, file: &str, line: Option<u32>) -> String {
    let authority = origin.split_once("://").map_or(origin, |(_, a)| a);
    let mut uri = url::Url::parse(&format!("vscode-remote://{authority}")).expect("authority");
    uri.set_path(file);
    let mut target = uri.to_string();
    let mut payload = vec![json!(["openFile", ""])];
    if let Some(l) = line {
        target.push_str(&format!(":{l}"));
        payload.push(json!(["gotoLineMode", "true"]));
    }
    payload[0][1] = target.into();
    Value::Array(payload).to_string()
}

/// `file` relative to `folder` when it's inside it (through links).
fn relative(folder: &str, file: &str) -> String {
    crate::paths::relative(folder, file)
}

/// A name for a file: no slashes or odd characters.
fn safe(name: &str) -> String {
    let s: String =
        name.chars().map(|c| if c.is_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '-' }).collect();
    if s.is_empty() || s.starts_with('.') { format!("w{s}") } else { s }
}

/// Percent-encoding for a query value.
fn enc(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>().replace('+', "%20")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_open_on_the_windows_remote() {
        let p = open_payload("http://b-4-k.localhost:7701", "/src/a b/main.rs", Some(42));
        let v: Value = serde_json::from_str(&p).unwrap();
        assert_eq!(v[0][0], "openFile");
        assert_eq!(v[0][1], "vscode-remote://b-4-k.localhost:7701/src/a%20b/main.rs:42");
        assert_eq!(v[1], json!(["gotoLineMode", "true"]));
        let p = open_payload("https://b-4.arugula.example.com", "/x.rs", None);
        assert_eq!(p, r#"[["openFile","vscode-remote://b-4.arugula.example.com/x.rs"]]"#);
    }

    #[test]
    fn paths_and_names() {
        assert_eq!(relative("/r/proj", "/r/proj/src/a.rs"), "src/a.rs");
        assert_eq!(relative("/r/proj/", "/r/proj/a.rs"), "a.rs");
        assert_eq!(relative("/r/proj", "/r/project/a.rs"), "/r/project/a.rs");
        assert_eq!(relative("/r/proj", "/etc/hosts"), "/etc/hosts");
        assert_eq!(safe("arugula"), "arugula");
        assert_eq!(safe("my proj/x"), "my-proj-x");
        assert_eq!(safe(".hidden"), "w.hidden");
        assert_eq!(enc("/a b/c?d&e"), "%2Fa%20b%2Fc%3Fd%26e");
    }

    #[test]
    fn a_file_opens_in_its_project() {
        let root = std::env::temp_dir().join(format!("ilg-ed-{}", std::process::id()));
        std::fs::create_dir_all(root.join("proj/.git")).unwrap();
        std::fs::create_dir_all(root.join("proj/src")).unwrap();
        std::fs::create_dir_all(root.join("loose")).unwrap();
        std::fs::write(root.join("proj/src/a.rs"), "").unwrap();
        std::fs::write(root.join("loose/n.txt"), "").unwrap();
        let root = root.canonicalize().unwrap();
        let s = |p: PathBuf| p.display().to_string();
        assert_eq!(
            resolve_here(&s(root.join("proj/src/a.rs"))).unwrap(),
            (s(root.join("proj")), Some(s(root.join("proj/src/a.rs"))), None)
        );
        assert_eq!(resolve_here(&s(root.join("proj/src"))).unwrap(), (s(root.join("proj/src")), None, None));
        assert_eq!(
            resolve_here(&s(root.join("loose/n.txt"))).unwrap(),
            (s(root.join("loose")), Some(s(root.join("loose/n.txt"))), None)
        );
        assert!(resolve_here(&s(root.join("nope"))).is_err());
        assert!(resolve_here("relative/path").is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
