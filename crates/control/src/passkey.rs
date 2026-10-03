//! Passkeys (WebAuthn) as a first-class sign-in: on their own (a new
//! account with no GitHub), or added to an account. Like GitHub sign-in,
//! a passkey opens control's API only; devices are still approved by
//! devices.
//!
//! Verified here rather than with a WebAuthn crate (which brings OpenSSL,
//! and the static builds can't have it). Only what's needed:
//!
//! - registration with `attestation: "none"` (the authenticator's make
//!   and model aren't checked; the key is what matters);
//! - ES256 (P-256) and Ed25519 credential keys;
//! - user verification required, the RP id is control's host, and the
//!   origin must be control's own.

use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use axum::{
    Json,
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use ciborium::value::Value as Cbor;
use illogical_e2e::now_ms;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::{ApiError, App, auth, err};

const CHALLENGE_TTL_MS: u64 = 5 * 60 * 1000;
const ES256: i64 = -7;
const EDDSA: i64 = -8;

/// Flags in authenticator data.
const UP: u8 = 0x01;
const UV: u8 = 0x04;
const AT: u8 = 0x40;

#[derive(Clone)]
enum Purpose {
    /// Register: for this account, or a new one by this name (#102).
    Register {
        account: Option<String>,
        name: String,
    },
    Login,
}

#[derive(Default)]
pub struct Challenges {
    open: Mutex<HashMap<String, (Purpose, u64)>>,
}

impl Challenges {
    fn issue(&self, p: Purpose) -> String {
        let c = B64.encode(illogical_e2e::random::<32>());
        let now = now_ms();
        let mut open = self.open.lock().unwrap();
        open.retain(|_, (_, at)| now - *at < CHALLENGE_TTL_MS);
        open.insert(c.clone(), (p, now));
        c
    }

    /// Each challenge works once.
    fn take(&self, c: &str) -> Option<Purpose> {
        let (p, at) = self.open.lock().unwrap().remove(c)?;
        (now_ms() - at < CHALLENGE_TTL_MS).then_some(p)
    }
}

fn rp_id(app: &App) -> String {
    url::Url::parse(&app.cfg.public_url).ok().and_then(|u| u.host_str().map(str::to_owned)).unwrap_or_default()
}

fn bad(why: &str) -> ApiError {
    err(StatusCode::BAD_REQUEST, why)
}

/// Optional session: registering adds to it, or starts a new account.
async fn session_of(app: &Arc<App>, headers: &HeaderMap) -> Option<String> {
    let token = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == auth::SESSION_COOKIE)
        .map(|(_, v)| v.to_owned())?;
    app.db.session(&auth::hash(&token), now_ms()).ok().flatten()
}

fn same_origin(app: &App, headers: &HeaderMap) -> Result<(), ApiError> {
    match headers.get(header::ORIGIN).and_then(|o| o.to_str().ok()) {
        Some(o) if o == app.cfg.origin => Ok(()),
        _ => Err(err(StatusCode::FORBIDDEN, "cross-origin request refused")),
    }
}

#[derive(Deserialize, Default)]
pub struct Start {
    /// A new account's name: what teammates see (#102).
    #[serde(default)]
    name: Option<String>,
}

pub async fn register_start(
    State(app): State<Arc<App>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Option<Json<Start>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    same_origin(&app, &headers)?;
    let account = session_of(&app, &headers).await;
    let name = match &account {
        Some(a) => app.db.account(a)?.map(|x| x.name).unwrap_or_default(),
        None => {
            let asked = body.and_then(|Json(b)| b.name).unwrap_or_default();
            let name = crate::api::display_name(&asked).map_err(|_| bad("your name, as teammates will see it"))?;
            // A new account each time: limited.
            app.limits.check(crate::limit::ACCOUNTS, app.limits.client_ip(peer, &headers))?;
            name
        }
    };
    let user_id = illogical_e2e::random::<16>().to_vec();
    let challenge = app.passkeys.issue(Purpose::Register { account: account.clone(), name: name.clone() });
    let name = if name.is_empty() { "illogical".to_owned() } else { name };
    Ok(Json(json!({
        "challenge": challenge,
        "rp": { "id": rp_id(&app), "name": "illogical" },
        "user": { "id": B64.encode(&user_id), "name": name, "displayName": name },
        "pubKeyCredParams": [{ "type": "public-key", "alg": EDDSA }, { "type": "public-key", "alg": ES256 }],
        "authenticatorSelection": { "residentKey": "required", "userVerification": "required" },
        "attestation": "none",
        "timeout": CHALLENGE_TTL_MS,
    })))
}

#[derive(Deserialize)]
pub struct Registration {
    /// base64url, as `PublicKeyCredential.rawId`.
    id: String,
    #[serde(rename = "clientDataJSON")]
    client_data: String,
    #[serde(rename = "attestationObject")]
    attestation: String,
}

#[derive(Deserialize)]
struct ClientData {
    #[serde(rename = "type")]
    kind: String,
    challenge: String,
    origin: String,
}

fn client_data(app: &App, b64: &str, kind: &str) -> Result<(ClientData, Vec<u8>), ApiError> {
    let raw = B64.decode(b64).map_err(|_| bad("bad clientDataJSON"))?;
    let cd: ClientData = serde_json::from_slice(&raw).map_err(|_| bad("bad clientDataJSON"))?;
    if cd.kind != kind {
        return Err(bad("wrong WebAuthn ceremony"));
    }
    if cd.origin != app.cfg.origin {
        return Err(err(StatusCode::FORBIDDEN, "passkey from another site"));
    }
    Ok((cd, raw))
}

/// rpIdHash, flags, signCount; the rest.
fn auth_data<'a>(app: &App, d: &'a [u8]) -> Result<(u8, u32, &'a [u8]), ApiError> {
    if d.len() < 37 {
        return Err(bad("short authenticator data"));
    }
    if d[..32] != Sha256::digest(rp_id(app).as_bytes())[..] {
        return Err(bad("passkey for another site"));
    }
    let flags = d[32];
    if flags & UP == 0 || flags & UV == 0 {
        return Err(bad("the passkey must verify you (PIN, fingerprint or face)"));
    }
    Ok((flags, u32::from_be_bytes(d[33..37].try_into().unwrap()), &d[37..]))
}

fn map_get(m: &[(Cbor, Cbor)], k: i64) -> Option<&Cbor> {
    m.iter().find(|(key, _)| key.as_integer().and_then(|i| i64::try_from(i).ok()) == Some(k)).map(|(_, v)| v)
}

/// A COSE key as (alg, public key bytes): SEC1 for P-256, raw for Ed25519.
fn cose_key(v: &Cbor) -> Result<(i64, Vec<u8>), ApiError> {
    let m = v.as_map().ok_or_else(|| bad("bad credential key"))?;
    let int = |k| map_get(m, k).and_then(|v| v.as_integer()).and_then(|i| i64::try_from(i).ok());
    let bytes = |k| map_get(m, k).and_then(|v| v.as_bytes()).cloned();
    match (int(1), int(3)) {
        // EC2, ES256, P-256.
        (Some(2), Some(ES256)) if int(-1) == Some(1) => {
            let (x, y) = bytes(-2).zip(bytes(-3)).ok_or_else(|| bad("bad P-256 key"))?;
            let mut sec1 = vec![4u8];
            sec1.extend_from_slice(&x);
            sec1.extend_from_slice(&y);
            p256::ecdsa::VerifyingKey::from_sec1_bytes(&sec1).map_err(|_| bad("bad P-256 key"))?;
            Ok((ES256, sec1))
        }
        // OKP, EdDSA, Ed25519.
        (Some(1), Some(EDDSA)) if int(-1) == Some(6) => Ok((EDDSA, bytes(-2).ok_or_else(|| bad("bad Ed25519 key"))?)),
        _ => Err(bad("unsupported passkey algorithm (ES256 or Ed25519 only)")),
    }
}

pub async fn register_finish(State(app): State<Arc<App>>, headers: HeaderMap, Json(r): Json<Registration>) -> Response {
    match register(&app, &headers, r).await {
        Ok((account, new_session)) => {
            let mut res = Json(json!({ "account": account })).into_response();
            if let Some(cookie) = new_session {
                res.headers_mut().append(header::SET_COOKIE, cookie);
            }
            res
        }
        Err(e) => e.into_response(),
    }
}

async fn register(
    app: &Arc<App>,
    headers: &HeaderMap,
    r: Registration,
) -> Result<(String, Option<header::HeaderValue>), ApiError> {
    same_origin(app, headers)?;
    let (cd, _) = client_data(app, &r.client_data, "webauthn.create")?;
    let Some(Purpose::Register { account, name }) = app.passkeys.take(&cd.challenge) else {
        return Err(bad("that request expired; try again"));
    };
    let att: Cbor = ciborium::from_reader(B64.decode(&r.attestation).map_err(|_| bad("bad attestation"))?.as_slice())
        .map_err(|_| bad("bad attestation"))?;
    let m = att.as_map().ok_or_else(|| bad("bad attestation"))?;
    let data = m
        .iter()
        .find(|(k, _)| k.as_text() == Some("authData"))
        .and_then(|(_, v)| v.as_bytes())
        .ok_or_else(|| bad("no authenticator data"))?;
    let (flags, _count, rest) = auth_data(app, data)?;
    if flags & AT == 0 || rest.len() < 18 {
        return Err(bad("no credential in the attestation"));
    }
    let id_len = u16::from_be_bytes([rest[16], rest[17]]) as usize;
    let cred_id = rest.get(18..18 + id_len).ok_or_else(|| bad("short credential"))?;
    if B64.encode(cred_id) != r.id {
        return Err(bad("credential id mismatch"));
    }
    let key: Cbor = ciborium::from_reader(&rest[18 + id_len..]).map_err(|_| bad("bad credential key"))?;
    let (alg, public) = cose_key(&key)?;
    let now = now_ms();
    // Signed in: add it to the account. Not: it is a new account.
    let (account, cookie) = match account {
        Some(a) => (a, None),
        None => {
            let a = app.db.account_for("passkey", &r.id, "", &auth::new_account_id(), now)?;
            app.db.set_name(&a, &name)?;
            (a.clone(), Some(auth::start_session(app, &a)?))
        }
    };
    app.db.add_passkey(&r.id, &account, alg, &public, now)?;
    Ok((account, cookie))
}

pub async fn login_start(
    State(app): State<Arc<App>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    same_origin(&app, &headers)?;
    app.limits.check(crate::limit::SIGN_INS, app.limits.client_ip(peer, &headers))?;
    let challenge = app.passkeys.issue(Purpose::Login);
    Ok(Json(
        json!({ "challenge": challenge, "rpId": rp_id(&app), "userVerification": "required", "timeout": CHALLENGE_TTL_MS }),
    ))
}

#[derive(Deserialize)]
pub struct Assertion {
    id: String,
    #[serde(rename = "clientDataJSON")]
    client_data: String,
    #[serde(rename = "authenticatorData")]
    authenticator_data: String,
    signature: String,
}

pub async fn login_finish(State(app): State<Arc<App>>, headers: HeaderMap, Json(a): Json<Assertion>) -> Response {
    match login(&app, &headers, a) {
        Ok(cookie) => {
            let mut res = Json(json!({})).into_response();
            res.headers_mut().append(header::SET_COOKIE, cookie);
            res
        }
        Err(e) => e.into_response(),
    }
}

fn login(app: &Arc<App>, headers: &HeaderMap, a: Assertion) -> Result<header::HeaderValue, ApiError> {
    same_origin(app, headers)?;
    let (cd, raw) = client_data(app, &a.client_data, "webauthn.get")?;
    if !matches!(app.passkeys.take(&cd.challenge), Some(Purpose::Login)) {
        return Err(bad("that request expired; try again"));
    }
    let pk =
        app.db.passkey(&a.id)?.ok_or_else(|| err(StatusCode::UNAUTHORIZED, "this passkey isn't registered here"))?;
    let data = B64.decode(&a.authenticator_data).map_err(|_| bad("bad authenticator data"))?;
    let (_, count, _) = auth_data(app, &data)?;
    let sig = B64.decode(&a.signature).map_err(|_| bad("bad signature"))?;
    let mut msg = data.clone();
    msg.extend_from_slice(&Sha256::digest(&raw));
    let ok = match pk.alg {
        ES256 => {
            use p256::ecdsa::signature::Verifier;
            let key = p256::ecdsa::VerifyingKey::from_sec1_bytes(&pk.public).map_err(|_| bad("stored key"))?;
            let s = p256::ecdsa::Signature::from_der(&sig).map_err(|_| bad("bad signature"))?;
            key.verify(&msg, &s).is_ok()
        }
        EDDSA => {
            let key: [u8; 32] = pk.public.as_slice().try_into().map_err(|_| bad("stored key"))?;
            let sig: [u8; 64] = sig.as_slice().try_into().map_err(|_| bad("bad signature"))?;
            ed25519_dalek::VerifyingKey::from_bytes(&key)
                .map(|k| k.verify_strict(&msg, &ed25519_dalek::Signature::from_bytes(&sig)).is_ok())
                .unwrap_or(false)
        }
        _ => false,
    };
    if !ok {
        return Err(err(StatusCode::UNAUTHORIZED, "the passkey's signature doesn't check out"));
    }
    // A counter that goes backwards means a cloned authenticator. Synced
    // passkeys report 0 throughout.
    if count != 0 && pk.sign_count != 0 && count <= pk.sign_count {
        return Err(err(StatusCode::UNAUTHORIZED, "this passkey's counter went backwards; it may have been copied"));
    }
    app.db.passkey_used(&a.id, count)?;
    auth::start_session(app, &pk.account)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cose_keys() {
        let ed = Cbor::Map(vec![
            (Cbor::from(1), Cbor::from(1)),
            (Cbor::from(3), Cbor::from(-8)),
            (Cbor::from(-1), Cbor::from(6)),
            (Cbor::from(-2), Cbor::Bytes(vec![7; 32])),
        ]);
        assert_eq!(cose_key(&ed).unwrap(), (EDDSA, vec![7; 32]));
        let rsa = Cbor::Map(vec![(Cbor::from(1), Cbor::from(3)), (Cbor::from(3), Cbor::from(-257))]);
        assert!(cose_key(&rsa).is_err());
    }
}
