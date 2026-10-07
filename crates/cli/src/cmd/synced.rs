//! `arugula synced`: the history other hosts synced here.

use super::Ctx;
use crate::http::{enc, request};
use crate::util::{print_json, time};

#[derive(clap::Subcommand)]
pub enum SyncedCmd {
    /// Forget a host's synced history.
    Rm { name: String },
    /// Re-encrypt all synced history under a new key, and drop the old one.
    RotateKey,
}

pub fn run(cmd: Option<SyncedCmd>, ctx: Ctx) -> anyhow::Result<i32> {
    let Ctx { sock, json_out, .. } = ctx;
    let v = match cmd {
        None => request(&sock, "GET", "/api/synced", None)?.json()?,
        Some(SyncedCmd::Rm { name }) => {
            request(&sock, "DELETE", &format!("/api/synced/{}", enc(&name)), None)?.json()?
        }
        Some(SyncedCmd::RotateKey) => request(&sock, "POST", "/api/synced/rotate-key", None)?.json()?,
    };
    if json_out || !v.is_array() {
        print_json(&v);
        return Ok(0);
    }
    for h in v.as_array().into_iter().flatten() {
        let panes = h["panes"].as_object().cloned().unwrap_or_default();
        let bytes: u64 = panes.values().filter_map(|p| p["bytes"].as_u64()).sum();
        let last = panes.values().filter_map(|p| p["last_push_ms"].as_u64()).max().unwrap_or(0);
        println!(
            "{:<20} {} panes, {} KB, last pushed {}",
            h["name"].as_str().unwrap_or("?"),
            panes.len(),
            bytes / 1024,
            time(last)
        );
    }
    Ok(0)
}
