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
//! - ES256 (P-256), Ed25519 and RS256 credential keys (RS256 for the
//!   Windows Hello setups and security keys that only do RSA, #205);
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
const RS256: i64 = -257;

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
        "pubKeyCredParams": [
            { "type": "public-key", "alg": EDDSA },
            { "type": "public-key", "alg": ES256 },
            { "type": "public-key", "alg": RS256 },
        ],
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

/// A COSE key as (alg, public key bytes): SEC1 for P-256, raw for
/// Ed25519, X.509 SubjectPublicKeyInfo DER for RSA.
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
        // RSA, RS256 (PKCS#1 v1.5 with SHA-256): n is -1, e is -2.
        (Some(3), Some(RS256)) => {
            use aws_lc_rs::encoding::AsDer;
            let (n, e) = bytes(-1).zip(bytes(-2)).ok_or_else(|| bad("bad RSA key"))?;
            let strip = |b: &[u8]| b.iter().position(|&x| x != 0).map(|i| b[i..].to_vec()).unwrap_or_default();
            let key = aws_lc_rs::rsa::PublicKeyComponents { n: strip(&n), e: strip(&e) };
            // 2048 bits at least, as verification will insist.
            if key.n.len() < 256 {
                return Err(bad("RSA passkey shorter than 2048 bits"));
            }
            let der = key.as_der().map_err(|_| bad("bad RSA key"))?;
            Ok((RS256, der.as_ref().to_vec()))
        }
        _ => Err(bad("unsupported passkey algorithm (ES256, Ed25519 or RS256 only)")),
    }
}

/// What keeps a passkey, from the AAGUID its authenticator reports (all
/// zeros when it won't say). A few common ones, from the community list at
/// github.com/passkeydeveloper/passkey-authenticator-aaguids.
fn provider(aaguid: &str) -> Option<&'static str> {
    Some(match aaguid {
        "fbfc3007154e4ecc8c0b6e020557d7bd" | "dd4ec289e01d41c9bb8970fa845d4bf2" => "iCloud Keychain",
        "ea9b8d664d011d213ce4b6b48cb575d4" => "Google Password Manager",
        "adce000235bcc60a648b0b25f1f05503" => "Chrome on Mac",
        "08987058cadc4b81b6e130de50dcbe96"
        | "9ddd1817af5a4672a2b93e3dd95000a9"
        | "6028b017b1d44c02b4b3afcdafc96bb2" => "Windows Hello",
        "bada5566a7aa401fbd9645619a55120d" => "1Password",
        "d548826e79b4db40a3d811116f7e8349" => "Bitwarden",
        _ => return None,
    })
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
    let aaguid = hex::encode(&rest[..16]);
    app.db.note_passkey(&r.id, &crate::account::agent(headers), provider(&aaguid))?;
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
        RS256 => {
            aws_lc_rs::signature::UnparsedPublicKey::new(&aws_lc_rs::signature::RSA_PKCS1_2048_8192_SHA256, &pk.public)
                .verify(&msg, &sig)
                .is_ok()
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
    app.db.passkey_used(&a.id, count, now_ms())?;
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
        // RSA with no modulus or exponent.
        let rsa = Cbor::Map(vec![(Cbor::from(1), Cbor::from(3)), (Cbor::from(3), Cbor::from(-257))]);
        assert!(cose_key(&rsa).is_err());
        // An algorithm nobody offered.
        let ps256 = Cbor::Map(vec![(Cbor::from(1), Cbor::from(3)), (Cbor::from(3), Cbor::from(-37))]);
        assert!(cose_key(&ps256).is_err());
    }

    fn rsa_cose(k: &aws_lc_rs::rsa::KeyPair) -> Cbor {
        use aws_lc_rs::signature::KeyPair as _;
        let c = aws_lc_rs::rsa::PublicKeyComponents::<Vec<u8>>::from(k.public_key());
        Cbor::Map(vec![
            (Cbor::from(1), Cbor::from(3)),
            (Cbor::from(3), Cbor::from(RS256)),
            (Cbor::from(-1), Cbor::Bytes(c.n)),
            (Cbor::from(-2), Cbor::Bytes(c.e)),
        ])
    }

    #[tokio::test]
    async fn rs256_is_offered() {
        let app = App::for_tests("http://control.test");
        let mut h = HeaderMap::new();
        h.insert(header::ORIGIN, app.cfg.origin.parse().unwrap());
        let opts = register_start(
            State(Arc::new(app)),
            ConnectInfo("127.0.0.1:1".parse().unwrap()),
            h,
            Some(Json(Start { name: Some("Sam Stranger".into()) })),
        )
        .await
        .unwrap();
        let algs: Vec<i64> =
            opts.0["pubKeyCredParams"].as_array().unwrap().iter().map(|p| p["alg"].as_i64().unwrap()).collect();
        assert_eq!(algs, [EDDSA, ES256, RS256]);
    }

    /// An RSA passkey registers (a new account) and signs in.
    #[tokio::test]
    async fn rs256_registers_and_signs_in() {
        use aws_lc_rs::{rand::SystemRandom, rsa::KeySize, signature::RSA_PKCS1_SHA256};
        let app = Arc::new(App::for_tests("http://control.test"));
        let mut h = HeaderMap::new();
        h.insert(header::ORIGIN, app.cfg.origin.parse().unwrap());
        h.insert(header::USER_AGENT, "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Chrome/140.0".parse().unwrap());
        let client = |kind: &str, challenge: &str| {
            let cd = json!({ "type": kind, "challenge": challenge, "origin": app.cfg.origin });
            B64.encode(serde_json::to_vec(&cd).unwrap())
        };
        let rp = Sha256::digest(rp_id(&app).as_bytes()).to_vec();

        let k = aws_lc_rs::rsa::KeyPair::generate(KeySize::Rsa2048).unwrap();
        let cred = vec![9u8; 16];
        let mut data = rp.clone();
        data.push(UP | UV | AT);
        data.extend_from_slice(&0u32.to_be_bytes());
        // Windows Hello's AAGUID.
        data.extend_from_slice(&hex::decode("6028b017b1d44c02b4b3afcdafc96bb2").unwrap());
        data.extend_from_slice(&(cred.len() as u16).to_be_bytes());
        data.extend_from_slice(&cred);
        ciborium::into_writer(&rsa_cose(&k), &mut data).unwrap();
        let att = Cbor::Map(vec![
            (Cbor::from("fmt"), Cbor::from("none")),
            (Cbor::from("attStmt"), Cbor::Map(vec![])),
            (Cbor::from("authData"), Cbor::Bytes(data)),
        ]);
        let mut att_bytes = vec![];
        ciborium::into_writer(&att, &mut att_bytes).unwrap();
        let challenge = app.passkeys.issue(Purpose::Register { account: None, name: "Sam Stranger".into() });
        let r = Registration {
            id: B64.encode(&cred),
            client_data: client("webauthn.create", &challenge),
            attestation: B64.encode(&att_bytes),
        };
        let (account, cookie) = register(&app, &h, r).await.unwrap();
        assert!(cookie.is_some());
        let stored = app.db.passkey(&B64.encode(&cred)).unwrap().unwrap();
        assert_eq!((stored.alg, stored.account.as_str()), (RS256, account.as_str()));

        let sign_in = |sig_with: &aws_lc_rs::rsa::KeyPair| {
            let challenge = app.passkeys.issue(Purpose::Login);
            let cd = client("webauthn.get", &challenge);
            let mut data = rp.clone();
            data.push(UP | UV);
            data.extend_from_slice(&0u32.to_be_bytes());
            let mut msg = data.clone();
            msg.extend_from_slice(&Sha256::digest(B64.decode(&cd).unwrap()));
            let mut sig = vec![0; sig_with.public_modulus_len()];
            sig_with.sign(&RSA_PKCS1_SHA256, &SystemRandom::new(), &msg, &mut sig).unwrap();
            login(
                &app,
                &h,
                Assertion {
                    id: B64.encode(&cred),
                    client_data: cd,
                    authenticator_data: B64.encode(&data),
                    signature: B64.encode(&sig),
                },
            )
        };
        // Listed by what keeps it and the browser that added it (#208).
        let listed = &app.db.passkeys(&account).unwrap()[0];
        assert_eq!(listed.provider.as_deref(), Some("Windows Hello"));
        assert!(listed.agent.contains("Windows NT"));
        assert_eq!(listed.used, None);
        assert!(sign_in(&k).is_ok());
        assert!(app.db.passkeys(&account).unwrap()[0].used.is_some());
        // Another key's signature doesn't.
        let other = aws_lc_rs::rsa::KeyPair::generate(KeySize::Rsa2048).unwrap();
        assert!(sign_in(&other).is_err());
    }

    #[test]
    fn short_rsa_keys_are_refused() {
        let mut short = rsa_cose(&aws_lc_rs::rsa::KeyPair::generate(aws_lc_rs::rsa::KeySize::Rsa2048).unwrap());
        if let Cbor::Map(m) = &mut short {
            m[2].1 = Cbor::Bytes(vec![0xc5; 128]);
        }
        assert!(cose_key(&short).is_err());
    }
}
