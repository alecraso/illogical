//! Push through control (M21): a device's Web Push subscription, signed by
//! the device, so control can't swap in keys of its own and read what
//! daemons send. Daemons encrypt each notification to the subscription
//! (RFC 8291); control only adds the VAPID signature and posts it.
//! Control encrypts only its own notices (a device or a person waiting to
//! be let in), which hold nothing secret.
//!
//! ```text
//! illogical push v1
//! account <id>
//! device <id>
//! endpoint <url>
//! p256dh <base64url>
//! auth <base64url>
//! at <ms>
//! ```

use aes_gcm::{Aes128Gcm, KeyInit, aead::Aead};
use hkdf::Hkdf;
use p256::{PublicKey, SecretKey};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::{Cert, DeviceKeys, cert::verify_hex};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushSub {
    pub v: u32,
    pub account: String,
    pub device: String,
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
    pub at: u64,
    pub sig: String,
}

fn clean(s: &str) -> bool {
    !s.is_empty() && s.len() <= 1000 && !s.chars().any(|c| c.is_whitespace() || c.is_control())
}

impl PushSub {
    pub fn body(&self) -> String {
        // Frozen (#504): signed; see `frozen.rs`.
        format!(
            "illogical push v1\naccount {}\ndevice {}\nendpoint {}\np256dh {}\nauth {}\nat {}\n",
            self.account, self.device, self.endpoint, self.p256dh, self.auth, self.at
        )
    }

    pub fn well_formed(&self) -> bool {
        self.v == 1 && [&self.account, &self.device, &self.endpoint, &self.p256dh, &self.auth].iter().all(|s| clean(s))
    }

    pub fn sign_with(&mut self, keys: &DeviceKeys) {
        self.device = keys.id();
        self.sig = hex::encode(keys.signature(self.body().as_bytes()));
    }

    /// Signed by `cert`'s device, of the same account.
    pub fn signed_by(&self, cert: &Cert) -> bool {
        self.well_formed()
            && cert.device == self.device
            && cert.account == self.account
            && verify_hex(&cert.sign, self.body().as_bytes(), &self.sig)
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
    let as_public = local.public_key().to_sec1_bytes().to_vec();
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

/// A fresh P-256 key, for one notification.
pub fn ephemeral() -> SecretKey {
    loop {
        if let Ok(k) = SecretKey::from_slice(&crate::random::<32>()) {
            return k;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Kind;

    #[test]
    fn signed_subscriptions() {
        let k = DeviceKeys::generate();
        let mut c = Cert::new(&k, "acct", Kind::Browser, "phone");
        c.sign_with(&k);
        let mut s = PushSub {
            v: 1,
            account: "acct".into(),
            device: String::new(),
            endpoint: "https://fcm.googleapis.com/fcm/send/abc".into(),
            p256dh: "BLc4".into(),
            auth: "4vQK".into(),
            at: 1,
            sig: String::new(),
        };
        s.sign_with(&k);
        assert!(s.signed_by(&c));
        // Control swapping the key in is caught.
        let mut swapped = s.clone();
        swapped.p256dh = "BXXX".into();
        assert!(!swapped.signed_by(&c));
    }
}
