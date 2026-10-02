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
    Owner,
}

fn policy(method: &Method, path: &str) -> Policy {
    let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    let pane = |s: &str| s.parse::<PaneId>().ok();
    let get = method == Method::GET;
    match parts.as_slice() {
        ["api", "host"] if get => Policy::Anyone,
        ["api", "panes", id, "capture" | "process" | "tail" | "wait" | "export.cast"] if get => {
            pane(id).map_or(Policy::Owner, |p| Policy::On(p, Role::Viewer))
        }
        ["api", "blocks", id] if get => pane(id).map_or(Policy::Owner, |p| Policy::On(p, Role::Viewer)),
        ["api", "panes", id, "send" | "keys" | "mouse" | "attention" | "close" | "ask" | "cd"] if !get => {
            pane(id).map_or(Policy::Owner, |p| Policy::On(p, Role::Editor))
        }
        ["api", "panes", id, "ask", "withdraw"] if !get => {
            pane(id).map_or(Policy::Owner, |p| Policy::On(p, Role::Editor))
        }
        ["api", "blocks", id, "call", _] if !get => pane(id).map_or(Policy::Owner, |p| Policy::On(p, Role::Editor)),
        _ => Policy::Owner,
    }
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
        Policy::Anyone => next.run(req).await,
        Policy::Owner => refuse(StatusCode::FORBIDDEN, "only the owner can do that"),
        Policy::On(pane, need) => match app.mux.api(|r| Api::RoleOn(who, pane, r)).await.flatten() {
            Some(r) if r >= need => next.run(req).await,
            Some(_) => refuse(StatusCode::FORBIDDEN, "you're watching this session; you can't change it"),
            None => refuse(StatusCode::NOT_FOUND, "no such pane"),
        },
    }
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
        assert_eq!(policy(&p, "/api/blocks/7/call/approve"), Policy::On(7, Role::Editor));
        assert_eq!(policy(&p, "/api/panes/3/capture"), Policy::Owner);
        assert_eq!(policy(&g, "/api/panes"), Policy::Owner);
        assert_eq!(policy(&p, "/api/run"), Policy::Owner);
        assert_eq!(policy(&g, "/api/fs/read"), Policy::Owner);
        assert_eq!(policy(&g, "/api/search"), Policy::Owner);
        assert_eq!(policy(&p, "/api/acl"), Policy::Owner);
        assert_eq!(policy(&g, "/api/panes/x/capture"), Policy::Owner);
    }
}
