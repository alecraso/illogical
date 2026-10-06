//! `illogical mcp`: an MCP server on stdio, and tokens for MCP clients that reach `/mcp` over HTTP.

use super::Ctx;
use crate::http::{enc, request};
use crate::{
    mcp,
    util::{print_json, time},
};
use anyhow::bail;
use serde_json::json;

#[derive(clap::Subcommand)]
pub enum McpCmd {
    /// Make a token for an MCP client that uses `/mcp` over HTTP.
    ///
    /// Sent as `Authorization: Bearer TOKEN`, printed once; `--list` shows
    /// them, `--revoke NAME` cuts one off.
    Token {
        /// What to call it (the client, or the machine it's on). A token
        /// with the same name is replaced.
        #[arg(long, default_value = "client")]
        name: String,
        /// `full` (every tool) or `read` (the read-only tools only).
        #[arg(long, default_value = "full")]
        scope: String,
        #[arg(long, conflicts_with = "revoke")]
        list: bool,
        #[arg(long, value_name = "NAME")]
        revoke: Option<String>,
    },
}

#[derive(clap::Args)]
pub struct Args {
    /// A token to send (an agent block's, or a client token for a
    /// daemon this machine has no identity on).
    #[arg(long, env = "ILLOGICAL_MCP_TOKEN", hide_env_values = true)]
    token: Option<String>,
    #[command(subcommand)]
    cmd: Option<McpCmd>,
}

pub fn run(args: Args, ctx: Ctx) -> anyhow::Result<i32> {
    let Ctx { sock, json_out, .. } = ctx;
    match args {
        Args { token, cmd: None } => return mcp::run(sock, token),
        Args { cmd: Some(McpCmd::Token { list: true, .. }), .. } => {
            let v = request(&sock, "GET", "/api/mcp/tokens", None)?.json()?;
            if json_out {
                print_json(&v);
                return Ok(0);
            }
            let list = v.as_array().cloned().unwrap_or_default();
            if list.is_empty() {
                println!("no MCP tokens");
            }
            for t in list {
                let used = t["used_ms"].as_u64().map(time).unwrap_or_else(|| "never".into());
                println!(
                    "{:<20} {:<5} made {:>8}, last used {used}",
                    t["name"].as_str().unwrap_or(""),
                    t["scope"].as_str().unwrap_or(""),
                    time(t["created_ms"].as_u64().unwrap_or(0))
                );
            }
        }
        Args { cmd: Some(McpCmd::Token { revoke: Some(name), .. }), .. } => {
            request(&sock, "DELETE", &format!("/api/mcp/tokens/{}", enc(&name)), None)?.json()?;
            eprintln!("revoked {name}: a client using it is cut off at its next call");
        }
        Args { cmd: Some(McpCmd::Token { name, scope, .. }), .. } => {
            if !matches!(scope.as_str(), "full" | "read") {
                bail!("--scope: full or read");
            }
            let v =
                request(&sock, "POST", "/api/mcp/tokens", Some(&json!({ "name": name, "scope": scope })))?.json()?;
            if json_out {
                print_json(&v);
                return Ok(0);
            }
            println!("{}", v["token"].as_str().unwrap_or_default());
            eprintln!(
                "That's shown once. A client sends it to /mcp as `Authorization: Bearer <token>`; \
                 `illogical mcp token --revoke {name}` cuts it off."
            );
        }
    }
    Ok(0)
}
