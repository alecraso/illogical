//! An editor of its own in the swarm (M28): VS Code, Cursor or nvim that
//! joined from outside arugula. It's an entry beside the panes (type
//! `editor`, kind `editor`, this host, its project) with no tab and no
//! PTY: it clusters, previews and peeks like a pane, following opens a
//! read-only view of its cursor, and it goes when the editor disconnects
//! or its workspace is turned off. Nothing of it is saved.

use std::{path::Path, sync::Arc};

use arugula_proto::{BlockType, WorkKind};
use futures_util::future::BoxFuture;
use serde_json::{Value, json};

use super::link::Link;
use crate::block::{Block, Summary, no_method};

pub struct Presence {
    link: Arc<Link>,
    /// Its workspace's git repository, found once.
    project: Option<arugula_proto::Project>,
}

impl Presence {
    pub fn make(link: Arc<Link>) -> Arc<dyn Block> {
        let project = link.hello.workspace.as_deref().and_then(crate::classify::project);
        Arc::new(Self { link, project })
    }

    fn folder(&self) -> Option<&str> {
        self.link.hello.workspace.as_deref()
    }
}

/// What an editor is called: "VS Code", "nvim".
pub fn app_name(app: &str) -> &str {
    match app {
        "vscode" => "VS Code",
        "cursor" => "Cursor",
        "code-server" => "VS Code",
        "nvim" => "nvim",
        other => other,
    }
}

impl Block for Presence {
    fn kind(&self) -> BlockType {
        BlockType::Editor
    }

    fn config(&self) -> Value {
        Value::Null
    }

    fn state(&self) -> Value {
        json!({
            "presence": true,
            "workspace": self.folder(),
            "file": self.link.file(),
            "editor": self.link.info(),
        })
    }

    fn text(&self) -> String {
        let p = self.link.peek();
        let rel = |f: &str| relative(self.folder(), f);
        match (&p.file, p.line) {
            (Some(f), Some(l)) => format!("{}:{l}\n{}\n", rel(f), p.lines.join("\n")),
            (Some(f), None) => format!("{}\n", rel(f)),
            _ => format!("{}\n", self.folder().unwrap_or("")),
        }
    }

    fn call(&self, method: &str, _args: Value) -> BoxFuture<'static, Result<Value, String>> {
        let r = match method {
            "continue" => self.link.resume().map(|()| json!({})),
            "state" => Ok(self.state()),
            m => Err(no_method(BlockType::Editor, m)),
        };
        Box::pin(async move { r })
    }

    fn close(&self) {}

    fn summary(&self) -> Summary {
        let folder = self.folder().map(str::to_owned);
        let file = self.link.file().map(|f| relative(folder.as_deref(), &f));
        let name = folder
            .as_deref()
            .and_then(|f| Path::new(f).file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "editor".into());
        let app = app_name(&self.link.hello.editor).to_owned();
        Summary {
            work: Some(WorkKind::Editor),
            cwd: folder,
            project: self.project.clone(),
            title: Some(match &file {
                Some(f) => format!("{} — {name} ({app})", f.rsplit('/').next().unwrap_or(f)),
                None => format!("{name} ({app})"),
            }),
            file,
            editor: Some(self.link.info()),
        }
    }

    fn link(&self) -> Option<Arc<Link>> {
        Some(self.link.clone())
    }

    fn detached(&self) -> bool {
        true
    }
}

/// `file` relative to `folder` when it's inside it (through links).
pub fn relative(folder: Option<&str>, file: &str) -> String {
    match folder {
        Some(f) => crate::paths::relative(f, file),
        None => file.to_owned(),
    }
}
