//! TURN for huddles (M63): short-lived relay credentials for a daemon to
//! hand its huddle's members, from Cloudflare's TURN service
//! (`CLOUDFLARE_TURN_KEY_ID`, `CLOUDFLARE_TURN_API_TOKEN`). The relay
//! only ever carries DTLS-SRTP, which it can't decrypt; Cloudflare sees
//! peers' addresses and how much they send. Without a key, daemons get
//! Cloudflare's public STUN, which is enough unless both ends are behind
//! strict NATs.

use std::sync::Arc;

use axum::{Json, extract::State, http::StatusCode};
use serde_json::{Value, json};

use crate::{ApiError, App, auth::DaemonAuth, err};

pub struct Turn {
    pub key_id: String,
    pub token: String,
    /// `https://rtc.live.cloudflare.com`, or a fake one in tests.
    pub api: String,
}

/// How long the credentials work: a long huddle, with room to spare.
/// Daemons cache them for a fraction of this.
pub const TTL_SECS: u64 = 8 * 3600;

const STUN: &str = "stun:stun.cloudflare.com:3478";

/// `GET /api/daemon/turn`: `{ice_servers: [...], ttl}`, as
/// `RTCPeerConnection` takes them.
pub async fn daemon_turn(State(app): State<Arc<App>>, d: DaemonAuth) -> Result<Json<Value>, ApiError> {
    let Some(t) = &app.turn else {
        return Ok(Json(json!({ "ice_servers": [{ "urls": [STUN] }], "ttl": TTL_SECS, "turn": false })));
    };
    app.limits.check_daemon(crate::limit::TURNS, &d.cert.device)?;
    let url = format!("{}/v1/turn/keys/{}/credentials/generate-ice-servers", t.api.trim_end_matches('/'), t.key_id);
    let res =
        app.http.post(&url).bearer_auth(&t.token).json(&json!({ "ttl": TTL_SECS })).send().await.map_err(|e| {
            tracing::warn!(error = %e, "TURN credentials: Cloudflare didn't answer");
            err(StatusCode::BAD_GATEWAY, "the TURN service didn't answer")
        })?;
    if !res.status().is_success() {
        tracing::warn!(status = %res.status(), "TURN credentials refused");
        return Err(err(StatusCode::BAD_GATEWAY, "the TURN service refused"));
    }
    let body: Value = res.json().await.map_err(|_| err(StatusCode::BAD_GATEWAY, "the TURN service's answer"))?;
    // One object or a list of them, by API version.
    let servers = match body.get("iceServers") {
        Some(Value::Array(a)) => a.clone(),
        Some(o @ Value::Object(_)) => vec![o.clone()],
        _ => return Err(err(StatusCode::BAD_GATEWAY, "the TURN service's answer")),
    };
    Ok(Json(json!({ "ice_servers": servers, "ttl": TTL_SECS, "turn": true })))
}
