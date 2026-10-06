//! `illogical access`: who else can reach which sessions.

use super::Ctx;
use crate::http::request;
use crate::util::{print_json, time};

#[derive(clap::Subcommand)]
pub enum AccessCmd {
    /// Let someone reach a session, at once.
    ///
    /// As a viewer (watch), an editor (drive its panes, make and close tabs
    /// and splits) or an owner.
    Grant { session: String, who: String, role: String },
    /// Take it away; they're cut off at once.
    Revoke { session: String, who: String },
    /// Every grant and revoke, oldest first.
    Log,
}

pub fn run(cmd: Option<AccessCmd>, ctx: Ctx) -> anyhow::Result<i32> {
    let Ctx { sock, json_out, .. } = ctx;
    let principal = |who: &str| if who.contains(':') { who.to_owned() } else { format!("tailnet:{who}") };
    // A session by name or id ($N), from the panes list.
    let session_id = |want: &str| -> anyhow::Result<u64> {
        let v = request(&sock, "GET", "/api/panes", None)?.json()?;
        let id = want.strip_prefix('$').and_then(|n| n.parse().ok());
        v.as_array()
            .into_iter()
            .flatten()
            .find(|p| Some(p["session"].as_u64().unwrap_or(0)) == id || p["session_name"].as_str() == Some(want))
            .and_then(|p| p["session"].as_u64())
            .ok_or_else(|| anyhow::anyhow!("no session {want}"))
    };
    let log = matches!(cmd, Some(AccessCmd::Log));
    let v = match cmd {
        None | Some(AccessCmd::Log) => request(&sock, "GET", "/api/acl", None)?.json()?,
        Some(AccessCmd::Grant { session, who, role }) => {
            if !matches!(role.as_str(), "viewer" | "editor" | "owner") {
                anyhow::bail!("a role is viewer, editor or owner");
            }
            let body =
                serde_json::json!({ "session": session_id(&session)?, "principal": principal(&who), "role": role });
            request(&sock, "POST", "/api/acl", Some(&body))?.json()?
        }
        Some(AccessCmd::Revoke { session, who }) => {
            let body =
                serde_json::json!({ "session": session_id(&session)?, "principal": principal(&who), "role": null });
            request(&sock, "POST", "/api/acl", Some(&body))?.json()?
        }
    };
    if json_out {
        print_json(&v);
        return Ok(0);
    }
    if log {
        for e in v["audit"].as_array().into_iter().flatten() {
            println!(
                "{}  {:<6} ${:<3} {:<30} {}",
                time(e["at"].as_u64().unwrap_or(0)),
                e["action"].as_str().unwrap_or(""),
                e["session"],
                e["principal"].as_str().unwrap_or(""),
                e["role"].as_str().unwrap_or("")
            );
        }
    } else {
        let grants = v["grants"].as_array().cloned().unwrap_or_default();
        if grants.is_empty() {
            println!("nothing is shared");
        }
        for g in grants {
            println!(
                "${:<4} {:<7} {}",
                g["session"],
                g["role"].as_str().unwrap_or(""),
                g["principal"].as_str().unwrap_or("")
            );
        }
    }
    Ok(0)
}
