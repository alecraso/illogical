//! `arugula upload`: files onto a pane's host, and their paths pasted into it.

use super::Ctx;
use crate::http::{self, request};
use crate::util::Pane;
use anyhow::Context;
use serde_json::json;

/// What one upload request carries (the daemon takes up to 4 MB).
const UPLOAD_CHUNK: usize = 4 << 20;

/// `arugula upload`: each file to the pane's host in chunks, then their
/// paths pasted together.
fn upload(
    sock: &http::Target,
    pane: u32,
    files: &[std::path::PathBuf],
    force: bool,
    no_paste: bool,
) -> anyhow::Result<i32> {
    let mut paths = Vec::new();
    for f in files {
        let bytes = std::fs::read(f).with_context(|| format!("can't read {}", f.display()))?;
        let ext: String = f
            .extension()
            .and_then(|e| e.to_str())
            .filter(|e| e.len() <= 8 && e.bytes().all(|b| b.is_ascii_alphanumeric()))
            .unwrap_or("")
            .to_ascii_lowercase();
        // A name of our own: std's hasher is seeded at random.
        let id = {
            use std::hash::{BuildHasher, Hasher};
            let mut h = std::hash::RandomState::new().build_hasher();
            h.write(f.as_os_str().as_encoded_bytes());
            format!("{:016x}", h.finish())
        };
        let mut at = 0;
        let path = loop {
            let end = (at + UPLOAD_CHUNK).min(bytes.len());
            let last = end == bytes.len();
            let q = format!("id={id}&ext={ext}&offset={at}{}", if last { "&last=true" } else { "" });
            let path = format!("/api/panes/{pane}/upload?{q}");
            let headers = [("Content-Type", "application/octet-stream")];
            let r = http::send(sock, "POST", &path, &headers, &bytes[at..end])?
                .json()
                .with_context(|| format!("can't upload {}", f.display()))?;
            if last {
                break r["path"].as_str().unwrap_or_default().to_owned();
            }
            at = end;
        };
        paths.push(path);
    }
    if no_paste {
        for p in &paths {
            println!("{p}");
        }
        return Ok(0);
    }
    let r = request(sock, "POST", &format!("/api/panes/{pane}/paste"), Some(&json!({"paths": paths, "force": force})))?
        .json()?;
    if r["pasted"] == true {
        return Ok(0);
    }
    let front = r["front"].as_str().unwrap_or("something");
    eprintln!("not pasted: `{front}` is in front of %{pane}, not a shell or an agent (--force pastes anyway)");
    for p in &paths {
        println!("{p}");
    }
    Ok(2)
}

#[derive(clap::Args)]
pub struct Args {
    pane: Pane,
    #[arg(required = true)]
    files: Vec<std::path::PathBuf>,
    /// Paste even if what's in front isn't a shell or an agent.
    #[arg(long)]
    force: bool,
    /// Only upload them, and print where they went.
    #[arg(long, conflicts_with = "force")]
    no_paste: bool,
}

pub fn run(args: Args, ctx: Ctx) -> anyhow::Result<i32> {
    let Ctx { sock, .. } = ctx;
    let Args { pane, files, force, no_paste } = args;
    upload(&sock, pane.0, &files, force, no_paste)
}
