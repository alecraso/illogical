//! Device certificates and revocations, and deciding which devices an
//! account trusts.
//!
//! Control stores and hands these out, but it holds no signing key, so it
//! can't make one that checks out: every certificate is signed by a device
//! the account already trusts, back to the account's first device (the
//! root, self-signed). A daemon pins that root when it joins and evaluates
//! whatever control sends against it ([`Trust::evaluate`]).
//!
//! What's signed is a fixed line format, so browsers (WebCrypto Ed25519)
//! and Rust produce the same bytes without agreeing on JSON:
//!
//! ```text
//! illogical device v1
//! account <id>
//! device <id>
//! kind browser|cli|daemon|recovery
//! name <text: no control characters, at most 64 chars>
//! noise <hex>
//! sign <hex>
//! created <ms>
//! approver <device id>
//! ```

use std::collections::HashMap;

use anyhow::{bail, ensure};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::keys::{DeviceKeys, device_id, hex32};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Browser,
    Cli,
    Daemon,
    /// A recovery code: approves a new device when every other is lost.
    Recovery,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Browser => "browser",
            Kind::Cli => "cli",
            Kind::Daemon => "daemon",
            Kind::Recovery => "recovery",
        }
    }

    /// May sign other devices' certificates.
    pub fn approves(self) -> bool {
        matches!(self, Kind::Browser | Kind::Cli | Kind::Recovery)
    }

    /// May open channels to daemons.
    pub fn connects(self) -> bool {
        matches!(self, Kind::Browser | Kind::Cli)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cert {
    pub v: u32,
    pub account: String,
    pub device: String,
    pub kind: Kind,
    pub name: String,
    /// X25519 public key, hex (zeros for a recovery code).
    pub noise: String,
    /// Ed25519 public key, hex.
    pub sign: String,
    pub created: u64,
    /// The device that signed this; the device itself for the root.
    pub approver: String,
    /// Ed25519 signature by the approver over [`Cert::body`], hex; empty
    /// until approved.
    #[serde(default)]
    pub sig: String,
}

fn plain(s: &str, max: usize) -> bool {
    !s.is_empty() && s.chars().count() <= max && !s.chars().any(char::is_control)
}

fn token(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

impl Cert {
    /// An unsigned certificate for `keys`.
    pub fn new(keys: &DeviceKeys, account: &str, kind: Kind, name: &str) -> Self {
        Self {
            v: 1,
            account: account.to_owned(),
            device: keys.id(),
            kind,
            name: name.to_owned(),
            noise: hex::encode(keys.noise_public),
            sign: hex::encode(keys.sign_public()),
            created: crate::now_ms(),
            approver: String::new(),
            sig: String::new(),
        }
    }

    pub fn body(&self) -> String {
        // Frozen (#504): signed into every certificate; see `frozen.rs`.
        format!(
            "illogical device v1\naccount {}\ndevice {}\nkind {}\nname {}\nnoise {}\nsign {}\ncreated {}\napprover {}\n",
            self.account,
            self.device,
            self.kind.as_str(),
            self.name,
            self.noise,
            self.sign,
            self.created,
            self.approver
        )
    }

    /// Well formed, with an id that matches its keys. Says nothing about
    /// the signature.
    pub fn check_form(&self) -> anyhow::Result<()> {
        ensure!(self.v == 1, "unknown certificate version {}", self.v);
        ensure!(token(&self.account), "bad account id");
        ensure!(token(&self.approver), "no approver");
        ensure!(plain(&self.name, 64), "a device name is 1 to 64 characters, none of them control characters");
        let (noise, sign) = (hex32(&self.noise)?, hex32(&self.sign)?);
        ensure!(device_id(&noise, &sign) == self.device, "device id doesn't match its keys");
        Ok(())
    }

    /// A request to be approved: well formed apart from the account and
    /// approver, which the approver fills in.
    pub fn check_request(&self) -> anyhow::Result<()> {
        ensure!(self.v == 1, "unknown certificate version {}", self.v);
        ensure!(plain(&self.name, 64), "a device name is 1 to 64 characters, none of them control characters");
        let (noise, sign) = (hex32(&self.noise)?, hex32(&self.sign)?);
        ensure!(device_id(&noise, &sign) == self.device, "device id doesn't match its keys");
        Ok(())
    }

    /// The same device, kind and name: what an approval may not change.
    pub fn same_request(&self, other: &Cert) -> bool {
        (&self.device, &self.noise, &self.sign, self.kind, &self.name)
            == (&other.device, &other.noise, &other.sign, other.kind, &other.name)
    }

    pub fn noise_key(&self) -> [u8; 32] {
        hex32(&self.noise).unwrap_or_default()
    }

    fn verifying_key(&self) -> anyhow::Result<VerifyingKey> {
        Ok(VerifyingKey::from_bytes(&hex32(&self.sign)?)?)
    }

    /// Sign as `approver` (setting `approver` first).
    pub fn sign_with(&mut self, approver: &DeviceKeys) {
        self.approver = approver.id();
        self.sig = hex::encode(approver.signature(self.body().as_bytes()));
    }

    /// The signature checks out against `approver`'s key.
    pub fn signed_by(&self, approver: &Cert) -> bool {
        approver.device == self.approver && verify(&approver.verifying_key(), self.body().as_bytes(), &self.sig)
    }
}

/// `sig_hex` is `sign_hex`'s Ed25519 signature of `msg`.
pub fn verify_hex(sign_hex: &str, msg: &[u8], sig_hex: &str) -> bool {
    let key = hex32(sign_hex).and_then(|k| Ok(VerifyingKey::from_bytes(&k)?));
    verify(&key, msg, sig_hex)
}

fn verify(key: &anyhow::Result<VerifyingKey>, msg: &[u8], sig_hex: &str) -> bool {
    let Ok(key) = key else { return false };
    let Ok(sig) = hex::decode(sig_hex) else { return false };
    let Ok(sig) = <[u8; 64]>::try_from(sig.as_slice()) else { return false };
    key.verify_strict(msg, &Signature::from_bytes(&sig)).is_ok()
}

/// A device taken off the account, signed by one of its devices.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Revocation {
    pub v: u32,
    pub account: String,
    pub device: String,
    pub at: u64,
    pub by: String,
    pub sig: String,
}

impl Revocation {
    pub fn new(account: &str, device: &str, by: &DeviceKeys) -> Self {
        let mut r = Self {
            v: 1,
            account: account.into(),
            device: device.into(),
            at: crate::now_ms(),
            by: by.id(),
            sig: String::new(),
        };
        r.sig = hex::encode(by.signature(r.body().as_bytes()));
        r
    }

    pub fn signed_by(&self, signer: &Cert) -> bool {
        signer.device == self.by && verify(&signer.verifying_key(), self.body().as_bytes(), &self.sig)
    }

    pub fn body(&self) -> String {
        format!(
            "illogical revoke v1\naccount {}\ndevice {}\nat {}\nby {}\n",
            self.account, self.device, self.at, self.by
        )
    }
}

/// What a daemon pinned when it joined: the account and its root device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trust {
    pub account: String,
    pub root: String,
}

/// The devices an account trusts now.
#[derive(Debug, Default, Clone)]
pub struct Trusted {
    pub devices: HashMap<String, Cert>,
}

impl Trusted {
    pub fn get(&self, id: &str) -> Option<&Cert> {
        self.devices.get(id)
    }

    /// The device whose Noise key this is.
    pub fn by_noise(&self, key: &[u8]) -> Option<&Cert> {
        let hex_key = hex::encode(key);
        self.devices.values().find(|c| c.noise == hex_key)
    }
}

impl Trust {
    /// Which of `certs` chain back to the pinned root, net of
    /// `revocations`. A device revoked at time T no longer counts, nor
    /// does anything it approved after T; what it approved before T stays.
    /// Anything that doesn't check out is ignored, not an error: control
    /// may send junk, and it can only ever shrink the set.
    pub fn evaluate(&self, certs: &[Cert], revocations: &[Revocation]) -> Trusted {
        let mut by_id: HashMap<&str, Vec<&Cert>> = HashMap::new();
        for c in certs {
            if c.account == self.account && c.check_form().is_ok() {
                by_id.entry(c.device.as_str()).or_default().push(c);
            }
        }
        // Revocations count when their signer is valid; twice, so a
        // revoked device's own revocations stop counting.
        let mut revoked: HashMap<String, u64> = HashMap::new();
        for _ in 0..2 {
            let mut next = HashMap::new();
            for r in revocations {
                if r.v != 1 || r.account != self.account {
                    continue;
                }
                let signer = self.valid_cert(&by_id, &revoked, &r.by, r.at, 0);
                if signer.is_some_and(|s| s.kind.approves() && verify(&s.verifying_key(), r.body().as_bytes(), &r.sig))
                {
                    let at = next.entry(r.device.clone()).or_insert(r.at);
                    *at = (*at).min(r.at);
                }
            }
            revoked = next;
        }
        let now = u64::MAX;
        let mut out = Trusted::default();
        for id in by_id.keys() {
            if let Some(c) = self.valid_cert(&by_id, &revoked, id, now, 0) {
                out.devices.insert(c.device.clone(), c.clone());
            }
        }
        out
    }

    /// Why `cert` wouldn't count with the account's `certs` (#327), or
    /// `None` when it does.
    pub fn refusal(&self, certs: &[Cert], revocations: &[Revocation], cert: &Cert) -> Option<Refusal> {
        let mut all = certs.to_vec();
        all.push(cert.clone());
        if self.evaluate(&all, revocations).get(&cert.device) == Some(cert) {
            return None;
        }
        all.pop();
        let now = self.evaluate(&all, revocations);
        // A revocation stands for good; one signed by a device trusted now.
        let revoked = revocations.iter().any(|r| {
            r.device == cert.device && r.account == self.account && now.get(&r.by).is_some_and(|by| r.signed_by(by))
        });
        Some(match now.get(&cert.approver) {
            _ if revoked => Refusal::Revoked,
            None => Refusal::ApproverUntrusted,
            Some(a) if !cert.signed_by(a) => Refusal::BadSignature,
            Some(a) if !a.kind.approves() => Refusal::CantApprove,
            Some(a) if a.kind == Kind::Recovery && cert.kind == Kind::Daemon => Refusal::RecoveryForMachine,
            Some(_) => Refusal::NoChain,
        })
    }

    /// `id`'s certificate, if it was valid at time `at`: not revoked by
    /// then, and signed by a device that was valid when this was created.
    fn valid_cert<'a>(
        &self,
        by_id: &HashMap<&str, Vec<&'a Cert>>,
        revoked: &HashMap<String, u64>,
        id: &str,
        at: u64,
        depth: usize,
    ) -> Option<&'a Cert> {
        if depth > 64 || revoked.get(id).is_some_and(|&t| t <= at) {
            return None;
        }
        for c in by_id.get(id)? {
            if id == self.root {
                if c.approver == c.device && c.kind.approves() && c.kind != Kind::Recovery && c.signed_by(c) {
                    return Some(c);
                }
                continue;
            }
            if c.approver == c.device {
                continue;
            }
            let Some(approver) = self.valid_cert(by_id, revoked, &c.approver, c.created, depth + 1) else { continue };
            // Recovery codes approve people's devices, not daemons.
            let allowed = approver.kind.approves() && !(approver.kind == Kind::Recovery && c.kind == Kind::Daemon);
            if allowed && c.signed_by(approver) {
                return Some(c);
            }
        }
        None
    }
}

/// Which check an approval failed (#327).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The device was revoked: its key can't be approved again.
    Revoked,
    /// The approving device isn't one the account trusts (now).
    ApproverUntrusted,
    /// The signature isn't the approving device's.
    BadSignature,
    /// The approving device is a machine, which approves nothing.
    CantApprove,
    /// A recovery code approving a machine.
    RecoveryForMachine,
    /// Anything else: its form, its kind, or when it was made.
    NoChain,
}

impl Refusal {
    /// A code for clients to act on.
    pub fn code(self) -> &'static str {
        match self {
            Refusal::Revoked => "revoked",
            Refusal::ApproverUntrusted => "approver_untrusted",
            Refusal::BadSignature => "bad_signature",
            Refusal::CantApprove => "cant_approve",
            Refusal::RecoveryForMachine => "recovery_for_machine",
            Refusal::NoChain => "no_chain",
        }
    }

    /// The check that failed, as a clause.
    pub fn check(self) -> &'static str {
        match self {
            Refusal::Revoked => {
                "its key was removed from the account, so it can't be approved again: it needs a new key"
            }
            Refusal::ApproverUntrusted => "the approving device isn't one the account trusts",
            Refusal::BadSignature => "the signature doesn't verify with the approving device's key",
            Refusal::CantApprove => "the approving device can't approve others",
            Refusal::RecoveryForMachine => "a recovery code approves browsers and phones, not machines",
            Refusal::NoChain => "it doesn't chain to the account's first device (form, kind or time)",
        }
    }
}

/// The code a daemon shows when joining, derived from its keys: the device
/// What a daemon signs with its own key when it asks to join (0.17 and
/// newer): that whoever asks holds the key, not just its certificate.
pub fn join_proof_body(cert: &Cert, ms: u64) -> String {
    format!("illogical join proof v1\n{ms}\n{}", cert.body())
}

/// approving it recomputes it from the certificate control shows, so
/// control can't swap in a key of its own. Ten base32 characters, as
/// `XXXXX-XXXXX`.
pub fn join_code(cert: &Cert) -> String {
    use sha2::{Digest, Sha256};
    const ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTVWXYZ0123456789";
    let h = Sha256::digest(format!("illogical join\n{}\n{}\n", cert.noise, cert.sign).as_bytes());
    let mut bits: u64 = u64::from_be_bytes(h[..8].try_into().unwrap());
    let mut s = String::new();
    for i in 0..10 {
        if i == 5 {
            s.push('-');
        }
        s.push(ALPHABET[(bits >> 59) as usize] as char);
        bits <<= 5;
    }
    s
}

/// A signed request to control, as the `x-illogical-auth` header's value:
/// `v2 <device id> <ms> <nonce> <sig>`, signed over the method, the path
/// and query, the time, a fresh nonce and the body's SHA-256. Daemons sign
/// every request this way, and so does the CLI (M49).
pub fn request_auth(keys: &DeviceKeys, method: &str, path_and_query: &str, body: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let ms = crate::now_ms();
    let nonce = hex::encode(crate::random::<16>());
    let digest = hex::encode(Sha256::digest(body));
    let msg = format!("illogical daemon auth v2\n{method}\n{path_and_query}\n{ms}\n{nonce}\n{digest}\n");
    format!("v2 {} {ms} {nonce} {}", keys.id(), hex::encode(keys.signature(msg.as_bytes())))
}

/// Parse a code as typed: case, spaces and dashes don't matter.
pub fn normalize_code(s: &str) -> anyhow::Result<String> {
    let c: String = s.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_uppercase()).collect();
    if c.len() != 10 {
        bail!("a join code is ten letters and digits");
    }
    Ok(format!("{}-{}", &c[..5], &c[5..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(account: &str) -> (DeviceKeys, Cert) {
        let k = DeviceKeys::generate();
        let mut c = Cert::new(&k, account, Kind::Browser, "laptop");
        c.sign_with(&k);
        (k, c)
    }

    fn approved(by: &DeviceKeys, account: &str, kind: Kind, name: &str) -> (DeviceKeys, Cert) {
        let k = DeviceKeys::generate();
        let mut c = Cert::new(&k, account, kind, name);
        c.sign_with(by);
        (k, c)
    }

    fn trust(c: &Cert) -> Trust {
        Trust { account: c.account.clone(), root: c.device.clone() }
    }

    #[test]
    fn chain_back_to_the_root() {
        let (rk, rc) = root("acct");
        let (pk, pc) = approved(&rk, "acct", Kind::Browser, "phone");
        let (_, dc) = approved(&pk, "acct", Kind::Daemon, "geek");
        let t = trust(&rc).evaluate(&[rc.clone(), pc.clone(), dc.clone()], &[]);
        assert_eq!(t.devices.len(), 3);
        assert_eq!(t.by_noise(&pk.noise_public).unwrap().name, "phone");
    }

    #[test]
    fn control_cannot_add_a_device() {
        let (_, rc) = root("acct");
        // Control makes its own "root" and a device under it.
        let (fk, mut fc) = root("acct");
        let (_, mut sneaky) = approved(&fk, "acct", Kind::Browser, "control");
        let t = trust(&rc).evaluate(&[rc.clone(), fc.clone(), sneaky.clone()], &[]);
        assert_eq!(t.devices.len(), 1);
        // Or claims the real root approved it.
        sneaky.approver = rc.device.clone();
        fc.approver = rc.device.clone();
        let t = trust(&rc).evaluate(&[rc.clone(), fc, sneaky], &[]);
        assert_eq!(t.devices.len(), 1);
    }

    #[test]
    fn tampered_names_and_keys_fail() {
        let (rk, rc) = root("acct");
        let (_, mut pc) = approved(&rk, "acct", Kind::Browser, "phone");
        pc.name = "laptop".into();
        let t = trust(&rc).evaluate(&[rc.clone(), pc.clone()], &[]);
        assert_eq!(t.devices.len(), 1);
        let (other, _) = root("acct");
        let (_, mut pc) = approved(&rk, "acct", Kind::Browser, "phone");
        pc.noise = hex::encode(other.noise_public);
        assert!(pc.check_form().is_err());
    }

    #[test]
    fn revoking_keeps_earlier_approvals() {
        let (rk, rc) = root("acct");
        let (pk, mut pc) = approved(&rk, "acct", Kind::Browser, "phone");
        pc.created = 1;
        pc.sign_with(&rk);
        let (_, mut before) = approved(&pk, "acct", Kind::Daemon, "before");
        before.created = 10;
        before.sign_with(&pk);
        let (_, mut after) = approved(&pk, "acct", Kind::Daemon, "after");
        after.created = 30;
        after.sign_with(&pk);
        let mut rev = Revocation::new("acct", &pc.device, &rk);
        rev.at = 20;
        rev.sig = hex::encode(rk.signature(rev.body().as_bytes()));
        let t = trust(&rc).evaluate(&[rc.clone(), pc.clone(), before.clone(), after.clone()], &[rev]);
        let names: std::collections::BTreeSet<_> = t.devices.values().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["before", "laptop"].into());
    }

    #[test]
    fn a_revoked_device_cannot_revoke() {
        let (rk, rc) = root("acct");
        let (pk, pc) = approved(&rk, "acct", Kind::Browser, "phone");
        let (_, qc) = approved(&rk, "acct", Kind::Browser, "tablet");
        let mut kill_phone = Revocation::new("acct", &pc.device, &rk);
        kill_phone.at = 1;
        kill_phone.sig = hex::encode(rk.signature(kill_phone.body().as_bytes()));
        let spite = Revocation::new("acct", &qc.device, &pk);
        let t = trust(&rc).evaluate(&[rc.clone(), pc, qc], &[kill_phone, spite]);
        let names: std::collections::BTreeSet<_> = t.devices.values().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["laptop", "tablet"].into());
    }

    #[test]
    fn recovery_codes_approve_people_not_daemons() {
        let (rk, rc) = root("acct");
        let (ck, cc) = approved(&rk, "acct", Kind::Recovery, "recovery 1");
        let (_, newc) = approved(&ck, "acct", Kind::Browser, "new laptop");
        let (_, dc) = approved(&ck, "acct", Kind::Daemon, "box");
        let t = trust(&rc).evaluate(&[rc.clone(), cc, newc, dc], &[]);
        assert!(t.devices.values().any(|c| c.name == "new laptop"));
        assert!(!t.devices.values().any(|c| c.name == "box"));
    }

    /// #327: which check an approval failed.
    #[test]
    fn refusals_name_the_check() {
        let (rk, rc) = root("acct");
        let t = trust(&rc);
        let (ck, cc) = approved(&rk, "acct", Kind::Recovery, "recovery 1");
        let (dk, dc) = approved(&rk, "acct", Kind::Daemon, "box");
        let have = [rc.clone(), cc.clone(), dc.clone()];
        let new = |by: &DeviceKeys, kind: Kind| {
            let mut c = Cert::new(&DeviceKeys::generate(), "acct", kind, "new");
            c.sign_with(by);
            c
        };
        assert_eq!(t.refusal(&have, &[], &new(&rk, Kind::Browser)), None);
        let (stranger, _) = root("acct");
        assert_eq!(t.refusal(&have, &[], &new(&stranger, Kind::Browser)), Some(Refusal::ApproverUntrusted));
        let mut forged = new(&rk, Kind::Browser);
        forged.name = "renamed".into();
        assert_eq!(t.refusal(&have, &[], &forged), Some(Refusal::BadSignature));
        assert_eq!(t.refusal(&have, &[], &new(&dk, Kind::Browser)), Some(Refusal::CantApprove));
        assert_eq!(t.refusal(&have, &[], &new(&ck, Kind::Daemon)), Some(Refusal::RecoveryForMachine));
        // A removed machine, approved again with its old key.
        let mut again = Cert::new(&dk, "acct", Kind::Daemon, "box");
        again.created = dc.created + 1;
        again.sign_with(&rk);
        let gone = Revocation::new("acct", &dc.device, &rk);
        assert_eq!(t.refusal(&have, &[gone], &again), Some(Refusal::Revoked));
        let mut other = new(&rk, Kind::Browser);
        other.account = "elsewhere".into();
        other.sign_with(&rk);
        assert_eq!(t.refusal(&have, &[], &other), Some(Refusal::NoChain));
    }

    #[test]
    fn join_codes() {
        let (_, c) = root("acct");
        let code = join_code(&c);
        assert_eq!(code.len(), 11);
        assert_eq!(normalize_code(&code.to_lowercase().replace('-', " ")).unwrap(), code);
    }
}
