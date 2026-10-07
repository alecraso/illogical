//! `arugula editors`: editors in the swarm, and Arugula's extension for them.

use super::Ctx;
use crate::http::{self, request};
use crate::util::print_json;
use anyhow::{Context, bail};
use std::path::PathBuf;

#[derive(clap::Subcommand)]
pub enum EditorsCmd {
    /// Write Arugula's VS Code extension (a VSIX) to a file.
    Vsix {
        /// Where [default: arugula-editor-VERSION.vsix here].
        #[arg(short, long)]
        out: Option<PathBuf>,
    },
    /// Install Arugula's extension in VS Code or Cursor.
    ///
    /// With their CLI (`code --install-extension`).
    Install {
        /// The editor's command [default: `code`, else `cursor`].
        #[arg(long)]
        with: Option<String>,
    },
}

pub fn run(cmd: Option<EditorsCmd>, ctx: Ctx) -> anyhow::Result<i32> {
    let Ctx { sock, json_out, .. } = ctx;
    match cmd {
        None => {
            let v = request(&sock, "GET", "/api/editors", None)?.json()?;
            if json_out {
                print_json(&v);
                return Ok(0);
            }
            for e in v.as_array().into_iter().flatten() {
                let s = |k: &str| e[k].as_str().unwrap_or("");
                let app = e["editor"]["app"].as_str().unwrap_or("?");
                let remote = e["editor"]["remote"].as_str().map(|r| format!(" ({r})")).unwrap_or_default();
                let n = e["editor"]["followers"].as_u64().unwrap_or(0);
                let following = if n > 0 { format!("  {n} following") } else { String::new() };
                let why = e["reason"]["headline"].as_str().map(|h| format!("  [{h}]")).unwrap_or_default();
                println!(
                    "%{:<4} {:<12} {:<40} {}{following}{why}",
                    e["pane"],
                    format!("{app}{remote}"),
                    s("folder"),
                    s("file")
                );
            }
        }
        Some(EditorsCmd::Vsix { out }) => {
            let (name, bytes) = vsix(&sock)?;
            let out = out.unwrap_or_else(|| PathBuf::from(name));
            std::fs::write(&out, bytes).with_context(|| format!("writing {}", out.display()))?;
            println!("{}", out.display());
        }
        Some(EditorsCmd::Install { with }) => {
            let (name, bytes) = vsix(&sock)?;
            let path = std::env::temp_dir().join(name);
            std::fs::write(&path, bytes)?;
            let which = |c: &str| {
                std::process::Command::new(c)
                    .arg("--version")
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .is_ok_and(|s| s.success())
            };
            let editor = match with {
                Some(w) => w,
                None => ["code", "cursor", "code-server"]
                    .into_iter()
                    .find(|c| which(c))
                    .context(
                        "no `code` or `cursor` here: pass --with, or install the VSIX (`arugula editors vsix`) by hand",
                    )?
                    .to_owned(),
            };
            let st = std::process::Command::new(&editor).arg("--install-extension").arg(&path).status()?;
            let _ = std::fs::remove_file(&path);
            if !st.success() {
                bail!("{editor} --install-extension failed");
            }
            // The extension under its name before the rename, which would
            // join the swarm a second time (#505). Not there is fine.
            let _ = std::process::Command::new(&editor)
                .args(["--uninstall-extension", "illogical.illogical-editor"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            println!("Installed. In the editor: \"arugula: Show this workspace in the swarm\".");
        }
    }
    Ok(0)
}

/// Arugula's VS Code extension, from the daemon (M28).
fn vsix(sock: &http::Target) -> anyhow::Result<(String, Vec<u8>)> {
    let res = request(sock, "GET", "/api/editors/vsix", None)?;
    let name = res
        .header("content-disposition")
        .and_then(|d| d.split("filename=").nth(1))
        .map(|f| f.trim_matches('"').to_owned())
        .unwrap_or_else(|| "arugula-editor.vsix".into());
    Ok((name, res.bytes()?))
}
