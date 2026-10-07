//! Hosted sandboxes (M20): a VM made by control, for someone with only a
//! browser.
//!
//! Control creates a sprite, puts the static daemon in it, and runs it as
//! a service that joins the requester's account with a one-time ticket.
//! The device that asked approves the join by itself (no extra click), so
//! the sandbox is a daemon like any other: its terminals are end to end,
//! and control holds no key to them. Control reaches it through the
//! provider's proxy, on demand: nothing holds it awake, so it sleeps when
//! idle and wakes when someone connects. When its last session closes (or
//! its owner deletes it), the sprite is deleted.
//!
//! Until there's billing, only allowlisted accounts may make them, a few
//! each.

use std::{path::PathBuf, sync::Arc};

use arugula_e2e::now_ms;
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::{
    ApiError, App,
    auth::{DaemonAuth, Session, hash, token},
    err,
    sprites::Sprites,
};

type R = Result<Json<Value>, ApiError>;

/// The daemon's port inside a sandbox.
pub const PORT: u16 = 7681;
const SERVICE: &str = "arugulad";
const PREFIX: &str = "ilc-";

pub struct Hosted {
    pub sprites: Sprites,
    /// The static daemon to put in them.
    pub binary: PathBuf,
    /// Accounts that may make them (until billing).
    pub allow: Vec<String>,
    pub quota: usize,
}

fn hosted(app: &App) -> Result<&Hosted, ApiError> {
    app.hosted.as_ref().ok_or_else(|| err(StatusCode::NOT_FOUND, "hosted sandboxes aren't set up on this control"))
}

pub async fn create(State(app): State<Arc<App>>, s: Session, body: Option<Json<Value>>) -> R {
    let h = hosted(&app)?;
    // With billing, a paid plan pays for them; without, an allowlist.
    let allowed = h.allow.iter().any(|a| a == &s.account || a == "*");
    if !allowed {
        if app.stripe.is_none() {
            return Err(err(StatusCode::FORBIDDEN, "hosted sandboxes aren't open yet"));
        }
        if !crate::billing::paid(&app, &s.account)? {
            return Err(err(StatusCode::PAYMENT_REQUIRED, "hosted VMs are on a paid plan: upgrade first"));
        }
    }
    let live = app.db.sandboxes(&s.account)?.into_iter().filter(|x| x.deleted.is_none()).count();
    if live >= h.quota {
        return Err(err(
            StatusCode::TOO_MANY_REQUESTS,
            &format!("you have {live} hosted VMs, the most for now: delete one first"),
        ));
    }
    let device = body.as_ref().and_then(|b| b["device"].as_str()).unwrap_or_default().to_owned();
    let id = format!("{PREFIX}{}", hex::encode(arugula_e2e::random::<6>()));
    let ticket = token();
    app.db.add_sandbox(&id, &s.account, &device, &hash(&ticket), now_ms())?;
    info!(sandbox = id, account = s.account, "creating a hosted sandbox");
    let (a, sid) = (app.clone(), id.clone());
    tokio::spawn(async move {
        if let Err(e) = provision(&a, &sid, &ticket).await {
            warn!(sandbox = sid, error = %e, "hosted sandbox failed to start");
            let _ = a.db.set_sandbox_state(&sid, &format!("failed: {e}"));
        }
    });
    Ok(Json(json!({ "id": id })))
}

async fn provision(app: &App, id: &str, _ticket: &str) -> anyhow::Result<()> {
    let h = app.hosted.as_ref().expect("hosted");
    h.sprites.create(id).await?;
    app.db.set_sandbox_state(id, "installing")?;
    let bin = tokio::fs::read(&h.binary).await?;
    h.sprites.write_file(id, ".local/bin/arugulad", bin, 0o755).await?;
    // The daemon makes its key there and asks to join; control fetches the
    // request through the provider, the browser that asked approves it, and
    // control writes the enrollment back. Then it runs for good: on
    // loopback, reached through the provider's proxy, never dialing in.
    let script = format!(
        "B=$HOME/.local/bin/arugulad; S=$HOME/.local/state/arugula; mkdir -p $S; \
         [ -f $S/control.json ] || $B join-request --name {id} --out $S/join-request.json --state-dir $S; \
         while [ ! -f $S/control.json ]; do sleep 1; done; \
         exec $B --listen 127.0.0.1:{PORT} --state-dir $S --no-relay --sandbox-of-control --no-manager-env"
    );
    h.sprites.put_service(id, SERVICE, "/bin/sh", &["-c".to_owned(), script]).await?;
    app.db.set_sandbox_state(id, "joining")?;
    let mut req = None;
    for _ in 0..120 {
        if let Some(b) = h.sprites.read_file(id, ".local/state/arugula/join-request.json").await?
            && let Ok(c) = serde_json::from_slice::<arugula_e2e::Cert>(&b)
        {
            req = Some(c);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    let cert = req.ok_or_else(|| anyhow::anyhow!("the daemon didn't ask to join"))?;
    cert.check_request()?;
    let code = arugula_e2e::cert::join_code(&cert);
    // Control read the request from inside the box it made: its own key's.
    app.db.add_join(&code, &cert, &hash(&token()), &[], None, Some(id), "", true, now_ms())?;
    app.db.set_sandbox_state(id, "approving")?;
    Ok(())
}

/// The requester approved its join: hand the daemon its enrollment.
pub async fn enrolled(app: &App, sandbox: &str, cert: &arugula_e2e::Cert) -> anyhow::Result<()> {
    let h = app.hosted.as_ref().ok_or_else(|| anyhow::anyhow!("no hosted sandboxes"))?;
    let root = app.db.account(&cert.account)?.and_then(|a| a.root).ok_or_else(|| anyhow::anyhow!("no root"))?;
    let (certs, _) = app.db.devices(&cert.account)?;
    let saved = json!({
        "url": app.cfg.public_url,
        "trust": { "account": cert.account, "root": root },
        "cert": cert,
        "certs": certs,
        "revocations": app.db.revocations(&cert.account)?,
    });
    let dir = state_dir(&h.sprites, sandbox).await?;
    h.sprites.write_file(sandbox, &format!("{dir}/control.json"), serde_json::to_vec_pretty(&saved)?, 0o600).await?;
    app.db.set_sandbox_state(sandbox, "running")?;
    Ok(())
}

/// Where the box's daemon waits for its enrollment: beside its join
/// request. A box provisioned before the rename runs a script that waits
/// in `~/.local/state/illogical` (#505).
async fn state_dir(sprites: &Sprites, sandbox: &str) -> anyhow::Result<&'static str> {
    const NEW: &str = ".local/state/arugula";
    const OLD: &str = ".local/state/illogical";
    if sprites.read_file(sandbox, &format!("{NEW}/join-request.json")).await?.is_none()
        && sprites.read_file(sandbox, &format!("{OLD}/join-request.json")).await?.is_some()
    {
        return Ok(OLD);
    }
    Ok(NEW)
}

/// My sandboxes, with any join waiting for my device's approval.
pub async fn list(State(app): State<Arc<App>>, s: Session) -> R {
    let mut out = Vec::new();
    for x in app.db.sandboxes(&s.account)? {
        if x.deleted.is_some() {
            continue;
        }
        let join = app.db.sandbox_join(&x.id, now_ms())?;
        out.push(json!({
            "id": x.id, "state": x.state, "created": x.created, "device": x.device,
            "join": join.map(|(code, cert)| json!({ "code": code, "cert": cert })),
        }));
    }
    let open = app.hosted.as_ref().is_some_and(|h| h.allow.iter().any(|a| a == &s.account || a == "*"))
        || (app.hosted.is_some() && app.stripe.is_some() && crate::billing::paid(&app, &s.account)?);
    Ok(Json(json!({ "sandboxes": out, "open": open })))
}

pub async fn delete(State(app): State<Arc<App>>, s: Session, Path(id): Path<String>) -> R {
    let x = app
        .db
        .sandbox(&id)?
        .filter(|x| x.account == s.account)
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "no such sandbox"))?;
    remove(&app, &x.id).await?;
    Ok(Json(json!({})))
}

/// The sandbox's daemon says its last session closed: it's done.
pub async fn done(State(app): State<Arc<App>>, d: DaemonAuth) -> R {
    // Its name is the sandbox's id; it must be the daemon that joined it.
    let ours = app.db.sandbox_daemon(&d.cert.name)?.is_some_and(|daemon| daemon == d.cert.device);
    if !ours {
        return Err(err(StatusCode::NOT_FOUND, "not a hosted sandbox"));
    }
    remove(&app, &d.cert.name).await?;
    crate::reply(&arugula_control_wire::Ack {})
}

pub async fn remove(app: &App, id: &str) -> anyhow::Result<()> {
    if let Some(h) = &app.hosted {
        h.sprites.delete(id).await?;
    }
    if let Some(daemon) = app.db.sandbox_daemon(id)? {
        app.db.drop_daemon(&daemon)?;
        app.relay.drop_daemon(&daemon);
    }
    app.db.end_sandbox(id, now_ms())?;
    info!(sandbox = id, "hosted sandbox deleted");
    Ok(())
}

/// A ticket from a sandbox's service: which sandbox it is.
pub fn ticket(app: &App, ticket: &str) -> anyhow::Result<Option<String>> {
    app.db.sandbox_by_ticket(&hash(ticket))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use axum::{Router, extract::Query, routing::get};

    use super::*;

    /// A provider whose box holds `files` (relative to its home).
    async fn provider(files: &'static [&'static str]) -> Sprites {
        let app = Router::new().route(
            "/v1/sprites/{name}/fs/read",
            get(move |Query(q): Query<HashMap<String, String>>| async move {
                if files.contains(&q["path"].as_str()) { (StatusCode::OK, "{}") } else { (StatusCode::NOT_FOUND, "") }
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let at = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        Sprites::new(&format!("http://{at}"), "t".into()).unwrap()
    }

    /// A box provisioned before the rename waits for its enrollment in the
    /// old state directory (#505); a new one in the new.
    #[tokio::test]
    async fn enrollment_goes_where_the_box_asked() {
        let new = provider(&[".local/state/arugula/join-request.json"]).await;
        assert_eq!(state_dir(&new, "ilc-1").await.unwrap(), ".local/state/arugula");
        let old = provider(&[".local/state/illogical/join-request.json"]).await;
        assert_eq!(state_dir(&old, "ilc-1").await.unwrap(), ".local/state/illogical");
        let none = provider(&[]).await;
        assert_eq!(state_dir(&none, "ilc-1").await.unwrap(), ".local/state/arugula");
    }
}
