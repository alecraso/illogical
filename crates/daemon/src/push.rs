//! Web Push: tell a phone that a pane needs you, even with the app closed.
//!
//! The browser subscribes through its push service (FCM for Chrome, Apple's
//! for Safari) and hands us an endpoint plus keys; we encrypt the message for
//! that browser (RFC 8291, aes128gcm) and sign a VAPID token (RFC 8292) with
//! our own key so the push service accepts it. Keys and subscriptions live in
//! the state directory.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use p256::{
    SecretKey,
    ecdsa::{Signature, SigningKey, signature::Signer},
};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::store::{now_ms, write_atomic};

/// A browser's subscription, as `PushSubscription.toJSON()` gives it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subscription {
    pub endpoint: String,
    pub keys: SubscriptionKeys,
    /// Whose it is (M29): a principal id; none is the owner's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub who: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscriptionKeys {
    pub p256dh: String,
    pub auth: String,
}

#[derive(Clone)]
pub struct Push {
    key: SecretKey,
    subs: Arc<Mutex<Vec<Subscription>>>,
    path: PathBuf,
    subject: String,
    http: reqwest::Client,
}

pub(crate) fn random<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).expect("the OS's random source");
    b
}

pub(crate) fn new_secret() -> SecretKey {
    loop {
        if let Ok(k) = SecretKey::from_slice(&random::<32>()) {
            return k;
        }
    }
}

fn public_bytes(key: &SecretKey) -> Vec<u8> {
    key.public_key().to_sec1_bytes().to_vec()
}

impl Push {
    /// Load (or create) the VAPID key and the saved subscriptions.
    /// `subject` identifies the sender to push services (a `mailto:`).
    pub fn open(dir: PathBuf, subject: String) -> std::io::Result<Self> {
        std::fs::create_dir_all(&dir)?;
        let key_path = dir.join("vapid.key");
        let key = match std::fs::read_to_string(&key_path)
            .ok()
            .and_then(|s| B64.decode(s.trim()).ok())
            .and_then(|b| SecretKey::from_slice(&b).ok())
        {
            Some(k) => k,
            None => {
                let k = new_secret();
                write_atomic(&key_path, B64.encode(k.to_bytes()).as_bytes())?;
                k
            }
        };
        let path = dir.join("subscriptions.json");
        let subs = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let http =
            crate::roots::http().timeout(std::time::Duration::from_secs(15)).build().map_err(std::io::Error::other)?;
        Ok(Self { key, subs: Arc::new(Mutex::new(subs)), path, subject, http })
    }

    /// The public key browsers subscribe with (`applicationServerKey`).
    pub fn public_key(&self) -> String {
        B64.encode(public_bytes(&self.key))
    }

    pub fn subscribe(&self, sub: Subscription) -> std::io::Result<()> {
        let mut subs = self.subs.lock().unwrap();
        subs.retain(|s| s.endpoint != sub.endpoint);
        subs.push(sub);
        write_atomic(&self.path, &serde_json::to_vec_pretty(&*subs).map_err(std::io::Error::other)?)
    }

    pub fn subscriptions(&self) -> usize {
        self.subs.lock().unwrap().len()
    }

    /// Notify every subscribed browser; in the background. `extra` fields
    /// go in the payload too (an agent's pending approval, which the
    /// service worker turns into Approve and Deny actions).
    /// To the owner's subscriptions only.
    pub fn send(&self, pane: u32, title: &str, body: &str, extra: Option<serde_json::Value>) {
        self.send_to(pane, title, body, extra, |who| who == "owner");
    }

    /// To whoever's subscriptions `to` picks, by principal id (M29).
    pub fn send_to(
        &self,
        pane: u32,
        title: &str,
        body: &str,
        extra: Option<serde_json::Value>,
        to: impl Fn(&str) -> bool,
    ) {
        let subs: Vec<Subscription> =
            self.subs.lock().unwrap().iter().filter(|s| to(s.who.as_deref().unwrap_or("owner"))).cloned().collect();
        if subs.is_empty() {
            return;
        }
        let payload = serde_json::Value::Object(payload(pane, title, body, extra)).to_string();
        let this = self.clone();
        tokio::spawn(async move {
            for sub in subs {
                match this.deliver(&sub, payload.as_bytes()).await {
                    Ok(status) if status == 404 || status == 410 => {
                        info!(endpoint = %sub.endpoint, "push subscription expired; dropping it");
                        let mut subs = this.subs.lock().unwrap();
                        subs.retain(|s| s.endpoint != sub.endpoint);
                        let _ = write_atomic(&this.path, &serde_json::to_vec_pretty(&*subs).unwrap_or_default());
                    }
                    Ok(status) if !(200..300).contains(&status) => warn!(status, "push rejected"),
                    Ok(_) => {}
                    Err(e) => warn!(error = %e, "push failed"),
                }
            }
        });
    }

    async fn deliver(&self, sub: &Subscription, payload: &[u8]) -> anyhow::Result<u16> {
        let ua_public = B64.decode(&sub.keys.p256dh)?;
        let auth = B64.decode(&sub.keys.auth)?;
        let body = encrypt(payload, &ua_public, &auth, &new_secret(), &random::<16>())?;
        let url = reqwest::Url::parse(&sub.endpoint)?;
        let audience = audience(&url);
        let jwt = vapid_jwt(&self.key, &audience, &self.subject, now_ms() / 1000 + 12 * 3600);
        let res = self
            .http
            .post(url)
            .header("TTL", "3600")
            .header("Urgency", "high")
            .header("Content-Encoding", "aes128gcm")
            .header("Content-Type", "application/octet-stream")
            .header("Authorization", format!("vapid t={jwt}, k={}", self.public_key()))
            .body(body)
            .send()
            .await?;
        Ok(res.status().as_u16())
    }
}

/// What a notification says: tagged `pane-<n>` (one per pane, the newest
/// replacing the last) unless `extra` names a tag of its own (`invite-7`),
/// and `extra`'s fields with it. Here and through control (M21).
pub fn payload(
    pane: u32,
    title: &str,
    body: &str,
    extra: Option<serde_json::Value>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut p = serde_json::Map::new();
    p.insert("title".into(), title.into());
    p.insert("body".into(), body.into());
    p.insert("pane".into(), pane.into());
    p.insert("tag".into(), format!("pane-{pane}").into());
    if let Some(serde_json::Value::Object(extra)) = extra {
        p.extend(extra);
    }
    p
}

/// RFC 8291, shared with control (which encrypts its own notices).
pub use illogical_e2e::push::encrypt;

/// RFC 8292's `aud`: the push resource's origin, with its port if it has
/// one (a push service on loopback in the tests has one).
fn audience(endpoint: &reqwest::Url) -> String {
    endpoint.origin().ascii_serialization()
}

/// RFC 8292: a short-lived ES256 token naming the push service and us.
fn vapid_jwt(key: &SecretKey, audience: &str, subject: &str, exp: u64) -> String {
    let header = B64.encode(br#"{"typ":"JWT","alg":"ES256"}"#);
    let claims = B64.encode(serde_json::json!({ "aud": audience, "exp": exp, "sub": subject }).to_string());
    let signing_input = format!("{header}.{claims}");
    let sig: Signature = SigningKey::from(key.clone()).sign(signing_input.as_bytes());
    format!("{signing_input}.{}", B64.encode(sig.to_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_audience_is_the_endpoints_origin() {
        let a = |u: &str| audience(&reqwest::Url::parse(u).unwrap());
        assert_eq!(a("https://fcm.googleapis.com/fcm/send/abc"), "https://fcm.googleapis.com");
        assert_eq!(a("https://web.push.apple.com:443/x"), "https://web.push.apple.com");
        assert_eq!(a("http://127.0.0.1:4567/push/phone"), "http://127.0.0.1:4567");
    }

    /// #232: a caller's tag (an invite's) in place of the pane's.
    #[test]
    fn a_tag_of_its_own_replaces_the_panes() {
        let p = payload(3, "t", "b", Some(serde_json::json!({ "tag": "invite-7", "invite": 7 })));
        assert_eq!((p["tag"].as_str(), p["invite"].as_u64(), p["pane"].as_u64()), (Some("invite-7"), Some(7), Some(3)));
        assert_eq!(payload(3, "t", "b", None)["tag"], "pane-3");
        assert_eq!(payload(3, "t", "b", Some(serde_json::json!({ "ask": 1 })))["tag"], "pane-3");
    }

    /// RFC 8291 Appendix A, byte for byte.
    #[test]
    fn rfc8291_example() {
        let d = |s: &str| B64.decode(s).unwrap();
        let local = SecretKey::from_slice(&d("yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw")).unwrap();
        assert_eq!(
            B64.encode(public_bytes(&local)),
            "BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8"
        );
        let ua_public = d("BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4");
        let auth = d("BTBZMqHH6r4Tts7J_aSIgg");
        let salt: [u8; 16] = d("DGv6ra1nlYgDCS1FRnbzlw").try_into().unwrap();
        let body = encrypt(b"When I grow up, I want to be a watermelon", &ua_public, &auth, &local, &salt).unwrap();
        assert_eq!(
            B64.encode(body),
            "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN"
        );
    }

    #[test]
    fn vapid_token_verifies() {
        use p256::ecdsa::{VerifyingKey, signature::Verifier};
        let key = new_secret();
        let jwt = vapid_jwt(&key, "https://fcm.googleapis.com", "mailto:me@example.com", 1);
        let (input, sig) = jwt.rsplit_once('.').unwrap();
        let sig = Signature::from_slice(&B64.decode(sig).unwrap()).unwrap();
        VerifyingKey::from(key.public_key()).verify(input.as_bytes(), &sig).unwrap();
        let claims: serde_json::Value =
            serde_json::from_slice(&B64.decode(input.split('.').nth(1).unwrap()).unwrap()).unwrap();
        assert_eq!(claims["aud"], "https://fcm.googleapis.com");
    }
}
