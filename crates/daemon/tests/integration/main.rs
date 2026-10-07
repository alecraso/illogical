//! The daemon's integration tests, as one binary: one per file linked 48
//! times over (125-200 MB each in a debug build, #466). Each file is a
//! module; `cargo test -p arugulad --test integration tmux::` runs one,
//! nextest's `-E 'binary(integration) & test(/^tmux::/)'` too.

mod agentd;
#[cfg(unix)]
mod replay;
#[cfg(unix)]
mod testnet;

mod agent_screens;
mod agents;
mod agents_real;
mod api;
mod apps;
mod attach;
mod attention;
mod blocks;
mod calls;
mod control_moves;
mod control_state;
mod conversations;
mod dialout;
mod editor_swarm;
mod editors;
mod forge;
mod forge_github;
mod forge_gitlab;
mod forge_issues;
mod forge_live;
mod forges_github_real;
mod forges_real;
mod fountain;
mod fs;
mod guest_ssh;
mod hand;
mod hosts;
mod ide;
mod invite;
mod local_auth;
mod machines;
mod mcp;
mod memory;
mod prompt;
mod questions;
mod reboot;
mod resident;
mod resume;
mod review;
mod share;
mod sites;
mod ssh;
mod summaries;
mod team_answers;
mod threads;
mod tmux;
mod upgrade;
mod vm_reboot;
mod windows;
mod workspace;
