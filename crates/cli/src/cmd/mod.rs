//! The subcommands: one file for each command or family, holding its arguments and what it does.
//! `main.rs` has the command tree and dispatches here.

pub mod access;
pub mod agent;
pub mod app;
pub mod attention;
pub mod call;
pub mod capture;
pub mod claude;
pub mod close;
pub mod describe;
pub mod diff;
pub mod edit;
pub mod editors;
pub mod events;
pub mod export;
pub mod fountain;
pub mod history;
pub mod ide;
pub mod install;
pub mod invite;
pub mod issue;
pub mod join;
pub mod keys;
pub mod log;
pub mod login;
pub mod ls;
pub mod machines;
pub mod mcp;
pub mod mouse;
pub mod open;
pub mod pr;
pub mod process;
pub mod rerun;
pub mod rules;
pub mod run;
pub mod search;
pub mod send;
pub mod share;
pub mod shell_env;
pub mod studio;
pub mod synced;
pub mod tail;
pub mod upload;
pub mod view;
pub mod wait;
pub mod web;
pub mod workspace;

use std::path::PathBuf;

use crate::http::{Target, enc};

/// What a command that talks to a daemon has: the daemon it reaches, whether to print JSON, and where
/// the local daemon and a host that's gone are, for the few commands that need them.
pub struct Ctx {
    pub sock: Target,
    pub json_out: bool,
    /// `run --home` and `close` use the local daemon too.
    pub local_sock: PathBuf,
    pub host_name: Option<String>,
    /// A host that's gone (deleted, unreachable) whose synced history is read instead.
    pub gone: Option<String>,
}

/// `host=` for reading synced history: the one asked for, else the host that's gone.
pub fn synced_q(flag: Option<String>, gone: &Option<String>) -> Option<String> {
    flag.or(gone.clone()).map(|h| format!("host={}", enc(if h == "all" { "*" } else { &h })))
}
