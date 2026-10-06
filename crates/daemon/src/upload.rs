//! M70: a file from a client onto the pane's host, and its path pasted into
//! the pane, so a screenshot reaches a `claude` running there.
//!
//! `POST /api/panes/<id>/upload?id=<hex>&ext=<ext>&offset=N[&last=true]`
//! with a chunk of the file as the body: the first chunk (offset 0) makes
//! the file, each next one goes on its end, and the last answers its path.
//! Chunks keep one upload from holding a relay's channel, and give the
//! client its progress.
//!
//! `POST /api/panes/<id>/paste {paths, force}` then pastes the paths, the
//! way a client pastes text: bracketed when the program asked. Only into a
//! shell or an agent: a path means nothing to whatever runs behind `ssh`,
//! so for anything else it answers what's in front instead, and the client
//! offers *Copy* and *Paste anyway* (`force`).
//!
//! Files go in `illogical-uploads/<pane>/` under `$XDG_RUNTIME_DIR`, else
//! `$TMPDIR`, else `/tmp/illogical-<uid>`: folders `0700` and refused
//! unless they're ours, files `0600`, made new (never through a link), with
//! names we choose. A pane's go when it closes; anything older than a day
//! goes in the sweep, which also runs as the daemon starts.
//!
//! A VM pane's file is staged here, then written on its machine, in
//! `~/.cache/illogical/uploads/<pane>/` there.
//!
//! M71: an agent block takes uploads too, always on this host: its `send
//! {text, files}` takes them as its prompt's files (images go to the agent
//! as images), and `paste` into it sends them so.

use std::{
    fs::{self, DirBuilder, OpenOptions},
    io::{self, Seek, SeekFrom, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path as FsPath, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};

use axum::{
    Json,
    body::Bytes,
    extract::{Path, Query, State},
    http::StatusCode,
};
use illogical_proto::PaneId;
use serde::Deserialize;
use tracing::warn;

use crate::{
    api::{ApiError, Res, pane},
    mux::{Api, Cmd},
    server::App,
};

/// The most one request carries; bigger files come in chunks.
pub const CHUNK_MAX: usize = 4 << 20;
/// The most one file may be.
const FILE_MAX: u64 = 20 << 20;
/// The most all the uploads on this host may come to.
const QUOTA: u64 = 200 << 20;
/// How long an upload is kept.
const KEEP: Duration = Duration::from_secs(24 * 3600);
/// How often the sweep runs.
const SWEEP_EVERY: Duration = Duration::from_secs(3600);

fn err(code: StatusCode, msg: impl Into<String>) -> ApiError {
    ApiError(code, msg.into())
}

/// `dir`, made `0700`, or there already as a directory (not a link) of
/// ours that no one else may read.
fn private_dir(dir: &FsPath) -> io::Result<()> {
    match DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    let m = fs::symlink_metadata(dir)?;
    if !m.is_dir() || m.uid() != nix::unistd::geteuid().as_raw() || m.mode() & 0o077 != 0 {
        return Err(io::Error::other(format!("{} isn't a private directory of ours", dir.display())));
    }
    Ok(())
}

/// Where every pane's uploads go: `illogical-uploads` in the user's runtime
/// or temp directory (one of the system's, not checked), else in a private
/// `/tmp/illogical-<uid>`.
fn root() -> io::Result<PathBuf> {
    let base = match std::env::var_os("XDG_RUNTIME_DIR").or_else(|| std::env::var_os("TMPDIR")) {
        Some(d) => PathBuf::from(d),
        None => {
            let d = PathBuf::from(format!("/tmp/illogical-{}", nix::unistd::geteuid().as_raw()));
            private_dir(&d)?;
            d
        }
    };
    let root = base.join("illogical-uploads");
    private_dir(&root)?;
    Ok(root)
}

/// A pane's folder, made if it isn't there.
fn folder(pane: PaneId) -> io::Result<PathBuf> {
    let dir = root()?.join(pane.to_string());
    private_dir(&dir)?;
    Ok(dir)
}

/// What the uploads under `root` come to.
fn used(root: &FsPath) -> u64 {
    let Ok(panes) = fs::read_dir(root) else { return 0 };
    panes
        .flatten()
        .filter_map(|p| fs::read_dir(p.path()).ok())
        .flat_map(|files| files.flatten())
        .filter_map(|f| f.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}

/// Remove a closed pane's uploads.
pub fn forget(pane: PaneId) {
    std::thread::spawn(move || {
        let Ok(root) = root() else { return };
        let dir = root.join(pane.to_string());
        if let Err(e) = fs::remove_dir_all(&dir)
            && e.kind() != io::ErrorKind::NotFound
        {
            warn!(pane, error = %e, "can't remove the pane's uploads");
        }
    });
}

/// Remove uploads older than `keep`, and folders left empty.
fn sweep(root: &FsPath, keep: Duration) {
    let Ok(panes) = fs::read_dir(root) else { return };
    let now = SystemTime::now();
    for p in panes.flatten() {
        let Ok(files) = fs::read_dir(p.path()) else { continue };
        for f in files.flatten() {
            let old =
                f.metadata().and_then(|m| m.modified()).is_ok_and(|t| now.duration_since(t).unwrap_or_default() > keep);
            if old {
                let _ = fs::remove_file(f.path());
            }
        }
        // Only if it's empty.
        let _ = fs::remove_dir(p.path());
    }
}

/// The sweep, now (the daemon is starting) and every hour.
pub async fn keep_sweeping() {
    let mut every = tokio::time::interval(SWEEP_EVERY);
    loop {
        every.tick().await;
        let _ = tokio::task::spawn_blocking(|| {
            if let Ok(root) = root() {
                sweep(&root, KEEP);
            }
        })
        .await;
    }
}

#[derive(Deserialize)]
pub struct UploadQuery {
    /// The client's name for this upload, which names the file.
    id: String,
    /// The file's extension, which `claude` reads to know it's an image.
    #[serde(default)]
    ext: String,
    #[serde(default)]
    offset: u64,
    #[serde(default)]
    last: bool,
}

/// Write one chunk at `offset` of `path`: the first makes the file, the
/// rest must follow on from what's there.
fn write_chunk(path: &FsPath, offset: u64, body: &[u8]) -> io::Result<()> {
    let mut o = OpenOptions::new();
    o.write(true).mode(0o600).custom_flags(nix::libc::O_NOFOLLOW);
    if offset == 0 {
        o.create_new(true);
    }
    let mut f = o.open(path)?;
    if f.metadata()?.len() != offset {
        return Err(io::Error::other("that chunk doesn't follow on from what's been written"));
    }
    f.seek(SeekFrom::Start(offset))?;
    f.write_all(body)
}

/// An agent block (M71), which takes uploads as its prompts' files.
async fn agent(app: &App, id: PaneId) -> Option<Arc<dyn crate::block::Block>> {
    app.mux.api(|r| Api::Block(id, r)).await.flatten().filter(|b| b.kind() == illogical_proto::BlockType::Agent)
}

/// One of `pane`'s uploads, by the path the route answered: a file (not a
/// link) right in its folder here.
pub fn take(pane: PaneId, path: &str) -> io::Result<PathBuf> {
    let name = FsPath::new(path).file_name().ok_or_else(|| io::Error::other("not an upload"))?;
    let ours = folder(pane)?.join(name);
    if ours != FsPath::new(path) || !fs::symlink_metadata(&ours)?.is_file() {
        return Err(io::Error::other("not one of this block's uploads"));
    }
    Ok(ours)
}

pub async fn upload(
    State(app): State<Arc<App>>,
    Path(id): Path<PaneId>,
    Query(q): Query<UploadQuery>,
    body: Bytes,
) -> Res<Json<serde_json::Value>> {
    store(&app, id, q, body).await.map(Json)
}

/// A whole file from an agent through MCP (M71's `attach`): its path, as
/// the route would answer it.
pub async fn store_whole(app: &App, id: PaneId, ext: &str, body: Vec<u8>) -> Res<String> {
    use std::hash::{BuildHasher, RandomState};
    let tag = format!("{:016x}", RandomState::new().hash_one(std::time::SystemTime::now()));
    let q = UploadQuery { id: tag, ext: ext.to_owned(), offset: 0, last: true };
    let out = store(app, id, q, body.into()).await?;
    Ok(out["path"].as_str().unwrap_or_default().to_owned())
}

async fn store(app: &App, id: PaneId, q: UploadQuery, body: Bytes) -> Res<serde_json::Value> {
    // An agent block's stay here; a VM pane's go on to its machine.
    let machine = match agent(app, id).await {
        Some(_) => None,
        None => {
            pane(app, id).await?;
            app.mux.api(|r| Api::MachineOf(id, r)).await.flatten()
        }
    };
    if q.id.is_empty() || q.id.len() > 32 || !q.id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(err(StatusCode::BAD_REQUEST, "id: up to 32 hex digits"));
    }
    let ext = q.ext.to_ascii_lowercase();
    if ext.len() > 8 || !ext.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(err(StatusCode::BAD_REQUEST, "ext: up to 8 letters and digits"));
    }
    if q.offset + body.len() as u64 > FILE_MAX {
        return Err(err(StatusCode::PAYLOAD_TOO_LARGE, "a file can be 20 MB at most"));
    }
    let name = if ext.is_empty() { q.id.clone() } else { format!("{}.{ext}", q.id) };
    let (offset, last) = (q.offset, q.last);
    let path = tokio::task::spawn_blocking(move || -> Result<PathBuf, ApiError> {
        let dir = folder(id).map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        if used(dir.parent().unwrap_or(&dir)) + body.len() as u64 > QUOTA {
            return Err(err(
                StatusCode::INSUFFICIENT_STORAGE,
                "uploads on this host are at their 200 MB: close panes with old ones, or wait a day",
            ));
        }
        let path = dir.join(name);
        write_chunk(&path, offset, &body).map_err(|e| err(StatusCode::CONFLICT, e.to_string()))?;
        Ok(path)
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;
    // A VM pane's file goes on to its machine once it's all here.
    let path = match machine {
        Some(m) if last => to_machine(app, id, &m.sprite, &path).await?,
        _ => path.to_string_lossy().into_owned(),
    };
    Ok(serde_json::json!({ "path": path, "done": last }))
}

/// On a machine: `~/.cache/illogical/uploads/<pane>`, `0700`, made by
/// `run` as its user. Clears what's older than a day there first.
const MACHINE_FOLDER: &str = r#"set -e
d="$HOME/.cache/illogical/uploads"
find "$d" -type f -mmin +1440 -delete 2>/dev/null || true
find "$d" -mindepth 1 -type d -empty -delete 2>/dev/null || true
mkdir -p "$d/$1"
chmod 700 "$d" "$d/$1"
printf %s "$d/$1""#;

/// Send a whole staged file to the pane's machine, then drop it here: its
/// path there. The provider writes as root (S32), so the file is `0644`
/// in the user's `0700` folder: `claude` there reads it, no one else can
/// reach it.
async fn to_machine(app: &App, pane: PaneId, sprite: &str, staged: &FsPath) -> Res<String> {
    let gone = |why: String| err(StatusCode::BAD_GATEWAY, why);
    let provider =
        app.mux.provider.clone().ok_or_else(|| err(StatusCode::SERVICE_UNAVAILABLE, "VM panes aren't set up"))?;
    let data = tokio::fs::read(staged).await.map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let _ = tokio::fs::remove_file(staged).await;
    let tag = pane.to_string();
    let argv = ["sh", "-c", MACHINE_FOLDER, "illogical-upload", &tag];
    let (out, code) =
        provider.run(sprite, &argv).await.map_err(|e| gone(format!("the machine isn't answering: {e}")))?;
    let dir = String::from_utf8_lossy(&out).trim().to_owned();
    if code != Some(0) || !dir.starts_with('/') {
        return Err(gone("can't make the uploads folder on the machine".into()));
    }
    let name = staged.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let path = format!("{dir}/{name}");
    provider
        .write_file(sprite, &path, data, 0o644)
        .await
        .map_err(|e| gone(format!("can't write it on the machine: {e}")))?;
    Ok(path)
}

/// Remove a closed pane's uploads on the machine it ran on, if that
/// machine stays.
pub fn forget_on(provider: Arc<dyn crate::provider::Provider>, sprite: String, pane: PaneId) {
    tokio::spawn(async move {
        let script = r#"rm -rf "$HOME/.cache/illogical/uploads/$1""#;
        let tag = pane.to_string();
        let _ = provider.run(&sprite, &["sh", "-c", script, "illogical-upload", &tag]).await;
    });
}

#[derive(Deserialize)]
pub struct PasteRequest {
    paths: Vec<String>,
    /// Paste whatever's in front (*Paste anyway*).
    #[serde(default)]
    force: bool,
}

/// Paths as one paste: space-separated, quoted where they need it, with
/// no control characters.
fn joined(paths: &[String]) -> String {
    paths
        .iter()
        .map(|p| {
            let p: String = p.chars().filter(|c| !c.is_control()).collect();
            if p.chars().any(|c| c.is_whitespace() || "'\"\\$`".contains(c)) {
                format!("'{}'", p.replace('\'', r"'\''"))
            } else {
                p
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

const SHELLS: &[&str] =
    &["sh", "bash", "zsh", "fish", "dash", "ksh", "mksh", "tcsh", "csh", "nu", "elvish", "xonsh", "pwsh"];

/// Whether a path on this host means something to what's in front: a shell
/// or an agent. Not `ssh`, a container's shell, an editor, a password
/// prompt (`sudo`), or anything else.
fn takes_paths(command: &str) -> bool {
    let mut words = command.split_whitespace().map(|w| w.trim_matches(['\'', '"']));
    let first = words.next().unwrap_or("");
    let name = first.rsplit('/').next().unwrap_or(first).trim_start_matches('-');
    // A shell at its prompt, not running a script (`sh build.sh`).
    let interactive = SHELLS.contains(&name) && words.all(|w| w.starts_with('-'));
    interactive || crate::classify::agent(command).is_some()
}

pub async fn paste(
    State(app): State<Arc<App>>,
    Path(id): Path<PaneId>,
    Json(req): Json<PasteRequest>,
) -> Res<Json<serde_json::Value>> {
    paste_into(&app, id, req.paths, req.force, None).await.map(Json)
}

/// Paste `paths` into a pane; into an agent block (M71), send them as a
/// prompt (`by`: whose).
pub async fn paste_into(
    app: &App,
    id: PaneId,
    paths: Vec<String>,
    force: bool,
    by: Option<&str>,
) -> Res<serde_json::Value> {
    if paths.is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "nothing to paste"));
    }
    if let Some(b) = agent(app, id).await {
        b.call_by("send", serde_json::json!({ "files": paths }), by)
            .await
            .map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
        return Ok(serde_json::json!({ "pasted": true, "sent": true }));
    }
    let p = pane(app, id).await?;
    let req = PasteRequest { paths, force };
    let text = joined(&req.paths);
    // What's in front: its foreground job, else its own program. A VM's
    // processes aren't ours to read; its pane is a shell or what's run
    // there.
    let on_machine = matches!(app.mux.api(|r| Api::MachineOf(id, r)).await, Some(Some(_)));
    if !req.force && !on_machine {
        let front = tokio::task::spawn_blocking({
            let p = p.clone();
            move || p.command().or_else(|| p.own_command())
        })
        .await
        .ok()
        .flatten();
        if let Some(front) = front.filter(|c| !takes_paths(c)) {
            return Ok(serde_json::json!({ "pasted": false, "front": front, "text": text }));
        }
    }
    let data = tokio::task::spawn_blocking({
        let p = p.clone();
        move || p.encode_paste(text)
    })
    .await
    .ok()
    .flatten()
    .ok_or_else(|| err(StatusCode::CONFLICT, "the pane isn't answering"))?;
    p.mark_input();
    app.mux.send(Cmd::Input { client: None, pane: id, data });
    Ok(serde_json::json!({ "pasted": true }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ilg-upload-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_folder_someone_could_read_or_a_link_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let t = temp("perm");
        let open = t.join("open");
        fs::create_dir(&open).unwrap();
        fs::set_permissions(&open, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(private_dir(&open).is_err());
        let link = t.join("link");
        std::os::unix::fs::symlink(t.join("elsewhere"), &link).unwrap();
        fs::create_dir(t.join("elsewhere")).unwrap();
        assert!(private_dir(&link).is_err());
        let fresh = t.join("fresh");
        private_dir(&fresh).unwrap();
        assert_eq!(fs::metadata(&fresh).unwrap().mode() & 0o777, 0o700);
        fs::remove_dir_all(t).unwrap();
    }

    #[test]
    fn chunks_follow_on_and_a_file_is_never_reused() {
        let t = temp("chunks");
        let f = t.join("a.png");
        write_chunk(&f, 0, b"abc").unwrap();
        assert!(write_chunk(&f, 0, b"x").is_err(), "offset 0 makes a new file");
        assert!(write_chunk(&f, 5, b"x").is_err(), "a gap");
        write_chunk(&f, 3, b"def").unwrap();
        assert_eq!(fs::read(&f).unwrap(), b"abcdef");
        assert_eq!(fs::metadata(&f).unwrap().mode() & 0o777, 0o600);
        // Never through a link.
        let link = t.join("b.png");
        std::os::unix::fs::symlink(&f, &link).unwrap();
        assert!(write_chunk(&link, 6, b"g").is_err());
        assert!(write_chunk(&link, 0, b"g").is_err());
        fs::remove_dir_all(t).unwrap();
    }

    #[test]
    fn the_quota_counts_every_pane_and_the_sweep_takes_old_files() {
        let t = temp("sweep");
        for (pane, size) in [("1", 10), ("2", 5)] {
            fs::create_dir(t.join(pane)).unwrap();
            fs::write(t.join(pane).join("x.png"), vec![0; size]).unwrap();
        }
        assert_eq!(used(&t), 15);
        sweep(&t, KEEP);
        assert_eq!(used(&t), 15, "new files stay");
        fs::create_dir(t.join("3")).unwrap();
        sweep(&t, Duration::ZERO);
        assert_eq!(used(&t), 0);
        assert_eq!(fs::read_dir(&t).unwrap().count(), 0, "empty folders go");
        fs::remove_dir_all(t).unwrap();
    }

    #[test]
    fn paths_paste_into_a_shell_or_an_agent_only() {
        for c in [
            "-zsh",
            "/bin/bash -l",
            "bash --norc --noprofile",
            "fish",
            "claude",
            "node /usr/lib/node_modules/.bin/claude",
            "/bin/sh /tmp/bin/claude",
            "codex resume",
        ] {
            assert!(takes_paths(c), "{c}");
        }
        for c in [
            "ssh geek",
            "docker exec -it web sh",
            "sudo apt upgrade",
            "vim notes.md",
            "kubectl exec -it p -- bash",
            "/bin/sh ./build.sh",
            "bash -c 'read x'",
        ] {
            assert!(!takes_paths(c), "{c}");
        }
    }

    #[test]
    fn paths_are_joined_and_quoted_where_they_need_it() {
        let paths = ["/run/u/a.png".to_string(), "/tmp/my shot.png".into(), "/tmp/it's\x1b[201~.png".into()];
        assert_eq!(joined(&paths), r"/run/u/a.png '/tmp/my shot.png' '/tmp/it'\''s[201~.png'");
    }
}
