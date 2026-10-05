//! A cut-down copy of illogical's identity model (docs/control-e2e.md), just
//! enough to bind an MLS leaf to a device certificate.
//!
//! - An account has a root Ed25519 key. In illogical that's the first device
//!   (self-signed) and later devices chain to it; here the root signs every
//!   device certificate directly.
//! - A device certificate names the device's Ed25519 `sign` key.
//! - The device's MLS signature key is a *separate* Ed25519 key, made inside
//!   the MLS library, and bound to the certificate by a signature from the
//!   device's `sign` key ("illogical mls leaf v1"). See FINDINGS.md for why it
//!   can't be the `sign` key itself in a browser.
//! - The MLS credential is a BasicCredential whose identity bytes are the
//!   JSON of `DeviceCredential` (certificate + binding).

use ed25519_dalek::{Signature, Signer as _, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub fn sha256(b: &[u8]) -> [u8; 32] {
    Sha256::digest(b).into()
}

/// Domain-separated signature: Ed25519(key, context || 0x00 || msg).
pub fn sign(key: &SigningKey, context: &str, msg: &[u8]) -> String {
    let mut m = context.as_bytes().to_vec();
    m.push(0);
    m.extend_from_slice(msg);
    hex::encode(key.sign(&m).to_bytes())
}

pub fn verify(pub_hex: &str, context: &str, msg: &[u8], sig_hex: &str) -> bool {
    let Ok(pk) = hex::decode(pub_hex) else { return false };
    let Ok(pk) = <[u8; 32]>::try_from(pk.as_slice()) else { return false };
    let Ok(pk) = VerifyingKey::from_bytes(&pk) else { return false };
    let Ok(sig) = hex::decode(sig_hex) else { return false };
    let Ok(sig) = <[u8; 64]>::try_from(sig.as_slice()) else { return false };
    let mut m = context.as_bytes().to_vec();
    m.push(0);
    m.extend_from_slice(msg);
    pk.verify(&m, &Signature::from_bytes(&sig)).is_ok()
}

pub fn pub_hex(k: &SigningKey) -> String {
    hex::encode(k.verifying_key().to_bytes())
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct DeviceCert {
    pub account: String,
    pub device: String,
    /// The device's own Ed25519 `sign` key (hex).
    pub sign: String,
    /// Ed25519(account root, "illogical device v1" || body).
    pub sig: String,
}

impl DeviceCert {
    fn body(&self) -> Vec<u8> {
        format!("{}|{}|{}", self.account, self.device, self.sign).into_bytes()
    }
    pub fn verify(&self, account_root_hex: &str) -> bool {
        verify(account_root_hex, "illogical device v1", &self.body(), &self.sig)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct MlsBinding {
    /// The MLS leaf's signature public key (hex), which openmls holds.
    pub mls_key: String,
    /// Ed25519(device sign key, "illogical mls leaf v1" || mls_key).
    pub sig: String,
}

/// What goes in the BasicCredential identity bytes.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct DeviceCredential {
    pub cert: DeviceCert,
    pub binding: MlsBinding,
}

impl DeviceCredential {
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap()
    }
    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        serde_json::from_slice(b).ok()
    }
    /// The certificate chains to `account_root_hex`, the binding is signed by
    /// the certificate's device key, and the leaf's MLS key is the bound one.
    pub fn verify(&self, account_root_hex: &str, leaf_signature_key: &[u8]) -> Result<(), String> {
        if !self.cert.verify(account_root_hex) {
            return Err(format!("{}: certificate doesn't chain to account {}", self.cert.device, self.cert.account));
        }
        if !verify(&self.cert.sign, "illogical mls leaf v1", self.binding.mls_key.as_bytes(), &self.binding.sig) {
            return Err(format!("{}: MLS key isn't bound to the device key", self.cert.device));
        }
        if hex::encode(leaf_signature_key) != self.binding.mls_key {
            return Err(format!("{}: leaf signs with a key the certificate doesn't name", self.cert.device));
        }
        Ok(())
    }
}

pub struct Account {
    pub id: String,
    pub root: SigningKey,
}

impl Account {
    pub fn new(id: &str, seed: [u8; 32]) -> Self {
        Account { id: id.into(), root: SigningKey::from_bytes(&seed) }
    }
    pub fn root_pub(&self) -> String {
        pub_hex(&self.root)
    }
    pub fn issue(&self, device: &str, device_sign_pub: &str) -> DeviceCert {
        let mut c = DeviceCert { account: self.id.clone(), device: device.into(), sign: device_sign_pub.into(), sig: String::new() };
        c.sig = sign(&self.root, "illogical device v1", &c.body());
        c
    }
}

/// A device revocation (illogical: "Remove" on a device, signed by another
/// device of the same account). Here the account root signs it.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Revocation {
    pub account: String,
    pub device: String,
    pub sig: String,
}

impl Revocation {
    pub fn new(acct: &Account, device: &str) -> Self {
        Revocation { account: acct.id.clone(), device: device.into(), sig: sign(&acct.root, "illogical revoke v1", device.as_bytes()) }
    }
    pub fn verify(&self, account_root_hex: &str) -> bool {
        verify(account_root_hex, "illogical revoke v1", self.device.as_bytes(), &self.sig)
    }
}
