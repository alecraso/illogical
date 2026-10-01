//! Web Push: tell a phone that a pane needs you, even with the app closed.
//!
//! The browser subscribes through its push service (FCM for Chrome, Apple's
//! for Safari) and hands us an endpoint plus keys; we encrypt the message for
//! that browser (RFC 8291, aes128gcm) and sign a VAPID token (RFC 8292) with
//! our own key so the push service accepts it. Keys and subscriptions live in
//! the state directory.

use std::{
    io::Read,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use aes_gcm::{Aes128Gcm, KeyInit, aead::Aead};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use hkdf::Hkdf;
use p256::{
    PublicKey, SecretKey,
    ecdsa::{Signature, SigningKey, signature::Signer},
};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use tracing::{info, warn};

use crate::store::{now_ms, write_atomic};

/// A browser's subscription, as `PushSubscription.toJSON()` gives it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subscription {
    pub endpoint: String,
    pub keys: SubscriptionKeys,
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

fn random<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut b)).expect("/dev/urandom");
    b
}

fn new_secret() -> SecretKey {
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
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(std::io::Error::other)?;
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

    /// Notify every subscribed browser; in the background.
    pub fn send(&self, pane: u32, title: &str, body: &str) {
        let subs = self.subs.lock().unwrap().clone();
        if subs.is_empty() {
            return;
        }
        let payload = serde_json::json!({ "title": title, "body": body, "pane": pane, "tag": format!("pane-{pane}") })
            .to_string();
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
        let audience = format!("{}://{}", url.scheme(), url.host_str().unwrap_or_default());
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

/// RFC 8291: encrypt `plaintext` for a browser whose subscription keys are
/// `ua_public` (P-256, uncompressed) and `auth` (16 bytes), using our
/// ephemeral key `local` and a random `salt`. One record, aes128gcm.
pub fn encrypt(
    plaintext: &[u8],
    ua_public: &[u8],
    auth: &[u8],
    local: &SecretKey,
    salt: &[u8; 16],
) -> anyhow::Result<Vec<u8>> {
    let ua = PublicKey::from_sec1_bytes(ua_public).map_err(|_| anyhow::anyhow!("bad p256dh key"))?;
    let as_public = public_bytes(local);
    let shared = p256::ecdh::diffie_hellman(local.to_nonzero_scalar(), ua.as_affine());

    let mut key_info = b"WebPush: info\0".to_vec();
    key_info.extend_from_slice(ua_public);
    key_info.extend_from_slice(&as_public);
    let mut ikm = [0u8; 32];
    Hkdf::<Sha256>::new(Some(auth), shared.raw_secret_bytes().as_ref())
        .expand(&key_info, &mut ikm)
        .map_err(|_| anyhow::anyhow!("hkdf"))?;

    let prk = Hkdf::<Sha256>::new(Some(salt), &ikm);
    let mut cek = [0u8; 16];
    let mut nonce = [0u8; 12];
    prk.expand(b"Content-Encoding: aes128gcm\0", &mut cek).map_err(|_| anyhow::anyhow!("hkdf"))?;
    prk.expand(b"Content-Encoding: nonce\0", &mut nonce).map_err(|_| anyhow::anyhow!("hkdf"))?;

    let mut padded = plaintext.to_vec();
    padded.push(0x02); // the last (and only) record
    let cipher = Aes128Gcm::new_from_slice(&cek).map_err(|_| anyhow::anyhow!("aes key"))?;
    let sealed = cipher.encrypt(&nonce.into(), padded.as_slice()).map_err(|_| anyhow::anyhow!("aes-gcm"))?;

    let mut out = Vec::with_capacity(16 + 4 + 1 + as_public.len() + sealed.len());
    out.extend_from_slice(salt);
    out.extend_from_slice(&4096u32.to_be_bytes());
    out.push(as_public.len() as u8);
    out.extend_from_slice(&as_public);
    out.extend_from_slice(&sealed);
    Ok(out)
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
