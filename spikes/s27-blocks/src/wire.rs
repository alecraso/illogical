//! What a block's channel carries, and the grant that opens one.
//!
//! **The grant.** The parent page (control's page, which holds the device
//! key) makes a one-off X25519 key for one block and signs this with its
//! Ed25519 device key:
//!
//! ```text
//! illogical block grant 1
//! daemon <daemon id>
//! block <block id>
//! key <the block key's X25519 public half, hex>
//! expires <ms since the epoch>
//! ```
//!
//! It goes in Noise message 1's payload (encrypted to the daemon's static
//! key). The daemon checks it before answering: signed by a device it
//! trusts, for this daemon and a block it has, for the key that's doing the
//! handshake, not expired by the daemon's clock. Expiry gates new channels
//! only; a channel lasts until it closes (or the device is revoked).
//!
//! **Streams.** Inside the channel every message is a `Msg::Binary`
//! (`illogical_e2e::channel`) holding one frame: `kind (1) ‖ stream (u32
//! BE) ‖ payload`. The client opens streams (ids it picks), the daemon only
//! answers.
//!
//! ```text
//! client to daemon                 daemon to client
//! 'H' request head (JSON)          'R' response head (JSON)
//! 'D' body bytes                   'D' body bytes
//! 'E' end of body                  'E' end of body
//! 'O' WebSocket open (JSON)        'A' WebSocket accepted (JSON)
//! 'M' message: 0 text / 1 binary   'M' message
//! 'C' close (JSON)                 'C' close (JSON)
//! 'K' cancel the stream            'X' failed (JSON {message})
//! ```

use anyhow::{Context, bail, ensure};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Grant {
    pub daemon: String,
    pub block: String,
    /// The block key's X25519 public half, hex.
    pub key: String,
    pub expires: u64,
    /// The signing device's Ed25519 public key, hex.
    pub by: String,
    pub sig: String,
}

impl Grant {
    pub fn body(&self) -> String {
        format!(
            "illogical block grant 1\ndaemon {}\nblock {}\nkey {}\nexpires {}\n",
            self.daemon, self.block, self.key, self.expires
        )
    }

    /// Whether this grant lets `remote` (the handshake's static key) into
    /// `block` on `daemon` at `now`, signed by one of `trusted`.
    pub fn check(&self, daemon: &str, remote: &[u8; 32], now: u64, trusted: &[String]) -> anyhow::Result<()> {
        ensure!(self.daemon == daemon, "a grant for another daemon");
        ensure!(self.key.eq_ignore_ascii_case(&hex::encode(remote)), "a grant for another key");
        ensure!(self.expires > now, "the grant expired");
        ensure!(
            trusted.iter().any(|t| t.eq_ignore_ascii_case(&self.by)),
            "signed by a device this daemon doesn't trust"
        );
        let key: [u8; 32] = hex::decode(&self.by)?.try_into().map_err(|_| anyhow::anyhow!("bad signing key"))?;
        let sig: [u8; 64] = hex::decode(&self.sig)?.try_into().map_err(|_| anyhow::anyhow!("bad signature"))?;
        VerifyingKey::from_bytes(&key)?
            .verify(self.body().as_bytes(), &Signature::from_bytes(&sig))
            .context("the signature doesn't match")?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ReqHead {
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ResHead {
    pub status: u16,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WsOpen {
    pub path: String,
    #[serde(default)]
    pub protocols: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WsClose {
    #[serde(default)]
    pub code: u16,
    #[serde(default)]
    pub reason: String,
}

pub fn frame(kind: u8, stream: u32, payload: &[u8]) -> Vec<u8> {
    let mut f = Vec::with_capacity(5 + payload.len());
    f.push(kind);
    f.extend_from_slice(&stream.to_be_bytes());
    f.extend_from_slice(payload);
    f
}

pub fn json_frame<T: Serialize>(kind: u8, stream: u32, v: &T) -> Vec<u8> {
    frame(kind, stream, &serde_json::to_vec(v).expect("serialize"))
}

pub fn parse(f: &[u8]) -> anyhow::Result<(u8, u32, &[u8])> {
    if f.len() < 5 {
        bail!("short frame");
    }
    Ok((f[0], u32::from_be_bytes([f[1], f[2], f[3], f[4]]), &f[5..]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn grant(device: &SigningKey, key: &[u8; 32], expires: u64) -> Grant {
        let mut g = Grant {
            daemon: "d1".into(),
            block: "b1".into(),
            key: hex::encode(key),
            expires,
            by: hex::encode(device.verifying_key().to_bytes()),
            sig: String::new(),
        };
        g.sig = hex::encode(device.sign(g.body().as_bytes()).to_bytes());
        g
    }

    #[test]
    fn grants() {
        let device = SigningKey::from_bytes(&[7; 32]);
        let stranger = SigningKey::from_bytes(&[8; 32]);
        let trusted = vec![hex::encode(device.verifying_key().to_bytes())];
        let key = [1u8; 32];
        let g = grant(&device, &key, 2000);
        assert!(g.check("d1", &key, 1000, &trusted).is_ok());
        // Expired, another key, another daemon, nobody we trust.
        assert!(g.check("d1", &key, 2000, &trusted).is_err());
        assert!(g.check("d1", &[2; 32], 1000, &trusted).is_err());
        assert!(g.check("d2", &key, 1000, &trusted).is_err());
        assert!(g.check("d1", &key, 1000, &[]).is_err());
        assert!(grant(&stranger, &key, 2000).check("d1", &key, 1000, &trusted).is_err());
        // Tampered: a longer life, or another block, under the same signature.
        let mut longer = g.clone();
        longer.expires = 9999;
        assert!(longer.check("d1", &key, 1000, &trusted).is_err());
        let mut other = g.clone();
        other.block = "b2".into();
        assert!(other.check("d1", &key, 1000, &trusted).is_err());
    }

    #[test]
    fn frames() {
        let f = frame(b'D', 7, b"abc");
        assert_eq!(parse(&f).unwrap(), (b'D', 7, &b"abc"[..]));
        assert!(parse(&f[..4]).is_err());
    }
}
