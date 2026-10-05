//! A cut-down copy of the team roster (docs/control-e2e.md, "A team's member
//! list"): versioned, hash-chained, each version signed by a device of an
//! owner in the version before, or by an invite's one-time key.

use crate::ident::{self, DeviceCert, DeviceCredential, Revocation};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub enum Role {
    Owner,
    Member,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RosterMember {
    pub account: String,
    pub root: String,
    pub role: Role,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Invite {
    pub team: String,
    pub role: Role,
    pub expiry: u64,
    /// One-time Ed25519 public key; its private half lives only in the link's #fragment.
    pub otk: String,
    pub by: DeviceCert,
    pub sig: String,
}

impl Invite {
    fn body(&self) -> Vec<u8> {
        format!("{}|{:?}|{}|{}", self.team, self.role, self.expiry, self.otk).into_bytes()
    }
    pub fn new(owner_key: &SigningKey, owner_cert: &DeviceCert, team: &str, role: Role, expiry: u64, otk: &SigningKey) -> Self {
        let mut i = Invite { team: team.into(), role, expiry, otk: ident::pub_hex(otk), by: owner_cert.clone(), sig: String::new() };
        i.sig = ident::sign(owner_key, "illogical team invite v1", &i.body());
        i
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Roster {
    pub team: String,
    pub v: u64,
    pub prev: String,
    pub at: u64,
    pub members: Vec<RosterMember>,
    pub locked: bool,
    pub spent: Vec<String>,
    pub invite: Option<Invite>,
    /// Public key that signed this version: an owner's device key, or an invite's one-time key.
    pub by: String,
    pub by_cert: Option<DeviceCert>,
    pub sig: String,
}

/// What a commit's AAD carries: the roster version it enacts.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RosterRef {
    pub v: u64,
    pub hash: String,
}

/// The AAD on every commit: which roster version it enacts, and how many
/// entries of the device-revocation log it has applied. Receivers check the
/// commit against exactly that state, so every honest member decides the same.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct CommitAad {
    pub roster: RosterRef,
    pub revocations: usize,
}

impl CommitAad {
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap()
    }
    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        serde_json::from_slice(b).ok()
    }
}

impl Roster {
    fn body(&self) -> Vec<u8> {
        let mut r = self.clone();
        r.sig = String::new();
        serde_json::to_vec(&r).unwrap()
    }
    pub fn hash(&self) -> String {
        hex::encode(ident::sha256(&serde_json::to_vec(self).unwrap()))
    }
    pub fn reference(&self) -> RosterRef {
        RosterRef { v: self.v, hash: self.hash() }
    }
    pub fn founding(team: &str, founder: RosterMember, at: u64) -> Self {
        Roster { team: team.into(), v: 1, prev: String::new(), at, members: vec![founder], locked: false, spent: vec![], invite: None, by: String::new(), by_cert: None, sig: String::new() }
    }
    /// The next version, unsigned, with nothing changed yet.
    pub fn next(&self, at: u64) -> Self {
        let mut r = self.clone();
        r.v += 1;
        r.prev = self.hash();
        r.at = at;
        r.invite = None;
        r.by = String::new();
        r.by_cert = None;
        r.sig = String::new();
        r
    }
    pub fn sign_by_device(mut self, key: &SigningKey, cert: &DeviceCert) -> Self {
        self.by = ident::pub_hex(key);
        self.by_cert = Some(cert.clone());
        self.sig = ident::sign(key, "illogical team roster v1", &self.body());
        self
    }
    /// The invitee's device writes the next version, signed by the one-time key.
    pub fn redeem(prev: &Roster, invite: &Invite, joiner: RosterMember, otk: &SigningKey, at: u64) -> Self {
        let mut r = prev.next(at);
        r.members.push(joiner);
        r.spent.push(invite.otk.clone());
        r.invite = Some(invite.clone());
        r.by = ident::pub_hex(otk);
        r.sig = ident::sign(otk, "illogical team roster v1", &r.body());
        r
    }
    pub fn member(&self, account: &str) -> Option<&RosterMember> {
        self.members.iter().find(|m| m.account == account)
    }
    fn signed_by_owner_device(&self, of: &Roster) -> Result<(), String> {
        let cert = self.by_cert.as_ref().ok_or("unsigned")?;
        let owner = of.member(&cert.account).filter(|m| m.role == Role::Owner).ok_or("signer isn't an owner")?;
        if !cert.verify(&owner.root) || cert.sign != self.by {
            return Err("signer's device certificate doesn't chain to an owner".into());
        }
        if !ident::verify(&self.by, "illogical team roster v1", &self.body(), &self.sig) {
            return Err("bad roster signature".into());
        }
        Ok(())
    }
}

/// Every roster version a client has checked, plus revocations it has seen.
#[derive(Clone, Default)]
pub struct RosterChain {
    pub versions: Vec<Roster>,
    pub revocations: Vec<Revocation>,
}

impl RosterChain {
    /// Pinned on first use (illogical: the founding device signs v1).
    pub fn found(v1: Roster) -> Result<Self, String> {
        v1.signed_by_owner_device(&v1)?;
        Ok(RosterChain { versions: vec![v1], revocations: vec![] })
    }
    pub fn head(&self) -> &Roster {
        self.versions.last().unwrap()
    }
    pub fn get(&self, r: &RosterRef) -> Option<&Roster> {
        self.versions.iter().find(|x| x.v == r.v && x.hash() == r.hash)
    }
    /// The checks from control-e2e.md, shortened.
    pub fn push(&mut self, new: Roster) -> Result<(), String> {
        let prev = self.head().clone();
        if new.v != prev.v + 1 || new.prev != prev.hash() || new.at < prev.at || new.team != prev.team {
            return Err(format!("v{} doesn't follow v{}", new.v, prev.v));
        }
        match &new.invite {
            None => new.signed_by_owner_device(&prev)?,
            Some(inv) => {
                let owner = prev.member(&inv.by.account).filter(|m| m.role == Role::Owner).ok_or("invite not from an owner")?;
                if !inv.by.verify(&owner.root) || !ident::verify(&inv.by.sign, "illogical team invite v1", &inv.body(), &inv.sig) {
                    return Err("invite signature".into());
                }
                if inv.role == Role::Owner || inv.team != prev.team || new.at > inv.expiry || prev.spent.contains(&inv.otk) {
                    return Err("invite is for an owner, expired, or spent".into());
                }
                let joiner = new.members.last().ok_or("no joiner")?;
                let mut expect = prev.next(new.at);
                expect.members.push(RosterMember { role: inv.role, ..joiner.clone() });
                expect.spent.push(inv.otk.clone());
                if expect.members != new.members || expect.spent != new.spent || expect.locked != new.locked {
                    return Err("an invite version changed more than the invitee".into());
                }
                if new.by != inv.otk || !ident::verify(&inv.otk, "illogical team roster v1", &new.body(), &new.sig) {
                    return Err("not signed by the invite's one-time key".into());
                }
            }
        }
        self.versions.push(new);
        Ok(())
    }
    pub fn revoke(&mut self, r: Revocation) -> Result<(), String> {
        let root = self.versions.iter().rev().find_map(|v| v.member(&r.account)).ok_or("unknown account")?.root.clone();
        if !r.verify(&root) {
            return Err("bad revocation".into());
        }
        self.revocations.push(r);
        Ok(())
    }
    /// The AAD for a commit that enacts the newest state this device knows.
    pub fn aad(&self) -> Vec<u8> {
        CommitAad { roster: self.head().reference(), revocations: self.revocations.len() }.to_bytes()
    }
    /// May the leaf with this credential be in a channel under roster `r`
    /// and the first `revs` revocations?
    pub fn authorize(&self, r: &Roster, revs: usize, credential: &[u8], leaf_signature_key: &[u8]) -> Result<(String, Role), String> {
        let dc = DeviceCredential::from_bytes(credential).ok_or("credential isn't an illogical device credential")?;
        let m = r.member(&dc.cert.account).ok_or_else(|| format!("{} isn't on roster v{}", dc.cert.account, r.v))?;
        dc.verify(&m.root, leaf_signature_key)?;
        if self.revocations[..revs.min(self.revocations.len())].iter().any(|x| x.device == dc.cert.device && x.account == dc.cert.account) {
            return Err(format!("{} was removed", dc.cert.device));
        }
        if r.locked && m.role != Role::Owner {
            return Err(format!("v{} is locked and {} isn't an owner", r.v, dc.cert.account));
        }
        Ok((dc.cert.device, m.role))
    }
}
