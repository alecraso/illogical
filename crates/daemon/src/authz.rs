//! The API for someone who isn't the owner (M12). The owner can call
//! anything. Anyone else reaches only calls about one pane or block, in a
//! session shared with them: reading it as a viewer, driving it (typing,
//! keys, answering, approving an agent) as an editor. Everything that
//! reaches the machine itself (files, new panes, machines, history across
//! panes, hosts, sharing) stays the owner's.
//!
//! The WebSocket is checked in the mux, per message; this is the HTTP half.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Request, State},
    http::{Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use illogical_core::Role;
use illogical_proto::PaneId;
use serde_json::json;

use crate::{acl::Principal, mux::Api, server::App};

/// What a call needs from someone who isn't the owner.
#[derive(Debug, PartialEq, Eq)]
enum Policy {
    Anyone,
    /// This role on the pane's (or block's) session.
    On(PaneId, Role),
    /// The handler checks (it knows where the new thing goes).
    Handler,
    Owner,
}

fn policy(method: &Method, path: &str) -> Policy {
    let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    let pane = |s: &str| s.parse::<PaneId>().ok();
    let get = method == Method::GET;
    match parts.as_slice() {
        ["api", "host"] if get => Policy::Anyone,
        // M63: for huddles, which anyone with a session may join.
        ["api", "turn"] if get => Policy::Anyone,
        ["api", "panes", id, "capture" | "process" | "detection" | "tail" | "wait" | "export.cast"] if get => {
            pane(id).map_or(Policy::Owner, |p| Policy::On(p, Role::Viewer))
        }
        ["api", "blocks", id] if get => pane(id).map_or(Policy::Owner, |p| Policy::On(p, Role::Viewer)),
        // M28: the edit a diff card shows.
        ["api", "panes", id, "diff"] if get => pane(id).map_or(Policy::Owner, |p| Policy::On(p, Role::Viewer)),
        // M28: the handlers check each editor, and the pane mentioned to.
        ["api", "editors"] if get => Policy::Handler,
        ["api", "ide", "mention"] if !get => Policy::Handler,
        [
            "api",
            "panes",
            id,
            "send" | "prompt" | "keys" | "mouse" | "attention" | "close" | "ask" | "cd" | "permit" | "hook" | "inbox"
            | "followup" | "upload" | "paste",
        ] if !get => pane(id).map_or(Policy::Owner, |p| Policy::On(p, Role::Editor)),
        ["api", "panes", id, "ask", "withdraw"] if !get => {
            pane(id).map_or(Policy::Owner, |p| Policy::On(p, Role::Editor))
        }
        // M35: a way into a studio box is the owner's (it signs in as them).
        ["api", "blocks", _, "call", "enter"] => Policy::Owner,
        // M36: a forge block's login, and fetching its code into the
        // owner's clone and opening panes there, are the owner's. Writes
        // (comment, review, merge) are the owner's and editors'.
        // M37: an agent on an issue works in the owner's clone, as them.
        // M40: a webhook on the repository (*Live updates*) is the owner's.
        ["api", "blocks", _, "call", "login" | "diff" | "checkout" | "agent" | "live"] => Policy::Owner,
        // M43: which Fountain login a catalog reads with, and which of the
        // owner's checkouts it opens specs from, are the owner's. So are
        // running an agent from it (a `fountain acp` on this host, with the
        // owner's login: a guest's agents go on VMs) and Spec (file and
        // browser blocks on the owner's host). M44: so is *Run here*, a
        // Claude Code on the owner's host with their secrets.
        ["api", "blocks", _, "call", "profile" | "specs" | "run" | "run_fountain" | "spec" | "run_here"] => {
            Policy::Owner
        }
        // M45b: the runner view and what it opens: an agent block on a
        // runner conversation (the owner's login), a diff whose git runs
        // as `fountain` through the owner's sudoers rule, a shell as
        // `fountain`, and which view (the runner's reads the unit).
        ["api", "blocks", _, "call", "view" | "follow" | "changes" | "shell"] => Policy::Owner,
        ["api", "blocks", id, "call", _] if !get => pane(id).map_or(Policy::Owner, |p| Policy::On(p, Role::Editor)),
        // M61: the mux checks each thread (a pane's, or a session's).
        ["api", "threads", ..] => Policy::Handler,
        // M24: the handler shows each person what they may read, and checks
        // each pane acted on.
        ["api", "attention"] if get => Policy::Handler,
        // M29: anyone here may be notified about what they may answer.
        ["api", "push", "key"] if get => Policy::Anyone,
        ["api", "push", "subscribe" | "test"] if !get => Policy::Handler,
        ["api", "notify"] => Policy::Handler,
        ["api", "attention", "act"] if !get => Policy::Handler,
        // An editor's agent (M14): the handler puts it on a VM of theirs.
        ["api", "blocks"] if !get => Policy::Handler,
        _ => Policy::Owner,
    }
}

/// What drives a pane, so needs the owner's trust on their machine (M14).
/// A follow-up (M29) is an instruction to an agent running there; an upload
/// (M70) writes a file where the pane runs, and a paste is typing.
fn drives(path: &str) -> bool {
    ["/send", "/keys", "/mouse", "/followup", "/upload", "/paste"].iter().any(|s| path.ends_with(s))
}

fn refuse(status: StatusCode, why: &str) -> Response {
    (status, Json(json!({ "error": why }))).into_response()
}

pub async fn check(State(app): State<Arc<App>>, req: Request, next: Next) -> Response {
    let who = req.extensions().get::<Principal>().cloned().unwrap_or(Principal::Owner);
    if who.is_owner() {
        return next.run(req).await;
    }
    match policy(req.method(), req.uri().path()) {
        Policy::Anyone | Policy::Handler => next.run(req).await,
        Policy::Owner => refuse(StatusCode::FORBIDDEN, "only the owner can do that"),
        Policy::On(pane, need) => match app.mux.api(|r| Api::RoleOn(who, pane, r)).await.flatten() {
            Some((r, floor)) if r >= need => {
                if let Some(f) = floor
                    && let Err(why) = from_now(req.uri().path(), req.uri().query().unwrap_or(""), f)
                {
                    return refuse(StatusCode::FORBIDDEN, why);
                }
                // Typing into a pane on the owner's machine needs their trust
                // (M14).
                let path = req.uri().path();
                if drives(path) {
                    let who = req.extensions().get::<Principal>().cloned().unwrap_or(Principal::Owner);
                    if let Some(Err(why)) = app.mux.api(|r| Api::MayDrive(who, pane, r)).await {
                        return refuse(StatusCode::FORBIDDEN, &why);
                    }
                }
                next.run(req).await
            }
            Some(_) => refuse(StatusCode::FORBIDDEN, "you're watching this session; you can't change it"),
            None => refuse(StatusCode::NOT_FOUND, "no such pane"),
        },
    }
}

/// A "from now" share (M13): nothing before `floor` by any route. The
/// screen, live output, and tails from at or after it.
fn from_now(path: &str, query: &str, floor: u64) -> Result<(), &'static str> {
    let param = |k: &str| url_param(query, k);
    if path.ends_with("/export.cast") {
        return Err("shared from now on: no history to export");
    }
    if path.ends_with("/capture") && param("scope").is_some_and(|s| s != "screen") {
        return Err("shared from now on: the screen only");
    }
    if path.ends_with("/tail") && !param("from").and_then(|f| f.parse::<u64>().ok()).is_some_and(|f| f >= floor) {
        return Err("shared from now on: tail with from at or after where the share began");
    }
    Ok(())
}

fn url_param(query: &str, key: &str) -> Option<String> {
    query.split('&').filter_map(|kv| kv.split_once('=')).find(|(k, _)| *k == key).map(|(_, v)| v.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policies() {
        let g = Method::GET;
        let p = Method::POST;
        assert_eq!(policy(&g, "/api/host"), Policy::Anyone);
        assert_eq!(policy(&g, "/api/panes/3/capture"), Policy::On(3, Role::Viewer));
        assert_eq!(policy(&p, "/api/panes/3/send"), Policy::On(3, Role::Editor));
        for path in ["/api/panes/3/upload", "/api/panes/3/paste"] {
            assert_eq!(policy(&p, path), Policy::On(3, Role::Editor), "{path}");
            assert!(drives(path), "{path}");
        }
        assert!(!drives("/api/panes/3/capture"));
        assert_eq!(policy(&p, "/api/blocks/7/call/approve"), Policy::On(7, Role::Editor));
        assert_eq!(policy(&p, "/api/panes/3/capture"), Policy::Owner);
        assert_eq!(policy(&g, "/api/panes"), Policy::Owner);
        assert_eq!(policy(&p, "/api/run"), Policy::Owner);
        assert_eq!(policy(&g, "/api/fs/read"), Policy::Owner);
        assert_eq!(policy(&g, "/api/search"), Policy::Owner);
        assert_eq!(policy(&p, "/api/acl"), Policy::Owner);
        assert_eq!(policy(&g, "/api/panes/x/capture"), Policy::Owner);
        assert_eq!(policy(&g, "/api/panes/3/diff"), Policy::On(3, Role::Viewer));
        assert_eq!(policy(&g, "/api/ide"), Policy::Owner);
        assert_eq!(policy(&Method::PUT, "/api/ide"), Policy::Owner);
        assert_eq!(policy(&g, "/api/rules"), Policy::Owner);
        assert_eq!(policy(&Method::DELETE, "/api/rules/0"), Policy::Owner);
        assert_eq!(policy(&g, "/api/editors/vsix"), Policy::Owner);
        assert_eq!(policy(&g, "/api/hosts/self/shell-env"), Policy::Owner);
        assert_eq!(policy(&p, "/api/hosts/self/shell-env/refresh"), Policy::Owner);
        assert_eq!(policy(&p, "/api/blocks/7/call/enter"), Policy::Owner);
        assert_eq!(policy(&p, "/api/blocks/7/call/checkout"), Policy::Owner);
        assert_eq!(policy(&p, "/api/blocks/7/call/live"), Policy::Owner);
        assert_eq!(policy(&p, "/api/blocks/7/call/agent"), Policy::Owner);
        assert_eq!(policy(&p, "/api/blocks/7/call/run_here"), Policy::Owner);
        assert_eq!(policy(&p, "/api/blocks/7/call/comment"), Policy::On(7, Role::Editor));
        assert_eq!(policy(&p, "/api/blocks/7/call/answer"), Policy::On(7, Role::Editor));
        assert_eq!(policy(&p, "/api/studio"), Policy::Owner);
        assert_eq!(policy(&g, "/api/studio/apps"), Policy::Owner);
    }

    #[test]
    fn from_now_shares() {
        assert!(from_now("/api/panes/3/capture", "", 100).is_ok());
        assert!(from_now("/api/panes/3/capture", "scope=screen&format=ansi", 100).is_ok());
        assert!(from_now("/api/panes/3/capture", "scope=scrollback", 100).is_err());
        assert!(from_now("/api/panes/3/tail", "", 100).is_err());
        assert!(from_now("/api/panes/3/tail", "from=99", 100).is_err());
        assert!(from_now("/api/panes/3/tail", "from=100&follow=1", 100).is_ok());
        assert!(from_now("/api/panes/3/export.cast", "", 100).is_err());
        assert!(from_now("/api/panes/3/process", "", 100).is_ok());
    }
}
