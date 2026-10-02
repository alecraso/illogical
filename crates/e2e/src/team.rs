//! Teams (M19): who is in one, signed by the team's owners, so control
//! can't add a member.
//!
//! A roster lists each member's account, the root device that account was
//! known by when added, and their role. It is versioned and signed by a
//! device of an owner: version 1 by the founder, every later one by an
//! owner in the version before it. A team daemon pins the founder when it
//! joins and accepts only a roster that follows from what it has.
//!
//! ```text
//! illogical team v1
//! team <id>
//! name <text>
//! version <n>
//! at <ms>
//! member <account> <root device> <owner|editor|viewer> <name>
//! by <device id>
//! ```

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::{
    Cert, DeviceKeys, Revocation, Trust,
    cert::{Trusted, verify_hex},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TeamRole {
    Owner,
    Editor,
    Viewer,
}

impl TeamRole {
    pub fn as_str(self) -> &'static str {
        match self {
            TeamRole::Owner => "owner",
            TeamRole::Editor => "editor",
            TeamRole::Viewer => "viewer",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    pub account: String,
    /// The account's root device, as the owner who added them saw it.
    pub root: String,
    pub role: TeamRole,
    /// To show (a login); no spaces or control characters.
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Roster {
    pub v: u32,
    pub team: String,
    pub name: String,
    pub version: u64,
    pub at: u64,
    pub members: Vec<Member>,
    /// The signing device.
    pub by: String,
    pub sig: String,
}

/// What a team daemon pins when it joins: the team and its founder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamPin {
    pub team: String,
    pub founder: String,
    pub founder_root: String,
}

/// Every member account's certificates and revocations, as control hands
/// them out.
pub type AccountCerts = HashMap<String, (Vec<Cert>, Vec<Revocation>)>;

fn word(s: &str) -> bool {
    !s.is_empty() && s.len() <= 120 && !s.chars().any(|c| c.is_whitespace() || c.is_control())
}

impl Roster {
    pub fn body(&self) -> String {
        let mut b = format!(
            "illogical team v1\nteam {}\nname {}\nversion {}\nat {}\n",
            self.team, self.name, self.version, self.at
        );
        for m in &self.members {
            b.push_str(&format!("member {} {} {} {}\n", m.account, m.root, m.role.as_str(), m.name));
        }
        b.push_str(&format!("by {}\n", self.by));
        b
    }

    pub fn well_formed(&self) -> bool {
        self.v == 1
            && word(&self.team)
            && !self.name.is_empty()
            && self.name.len() <= 80
            && !self.name.chars().any(char::is_control)
            && self.members.iter().all(|m| word(&m.account) && word(&m.root) && word(&m.name))
            && self.members.iter().any(|m| m.role == TeamRole::Owner)
    }

    pub fn sign_with(&mut self, keys: &DeviceKeys) {
        self.by = keys.id();
        self.sig = hex::encode(keys.signature(self.body().as_bytes()));
    }

    pub fn member(&self, account: &str) -> Option<&Member> {
        self.members.iter().find(|m| m.account == account)
    }

    /// The devices an account in this roster trusts now, from its root
    /// as listed here.
    pub fn devices(&self, account: &str, certs: &AccountCerts) -> Trusted {
        let Some(m) = self.member(account) else { return Trusted::default() };
        let (c, r) = certs.get(account).map(|(c, r)| (c.as_slice(), r.as_slice())).unwrap_or((&[], &[]));
        Trust { account: account.to_owned(), root: m.root.clone() }.evaluate(c, r)
    }

    /// Whether this roster may follow `prev` (or, with none, start the team
    /// as `pin` says): newer, well formed, and signed by a device of an
    /// owner of the previous version (the founder for the first).
    pub fn follows(&self, prev: Option<&Roster>, pin: &TeamPin, certs: &AccountCerts) -> bool {
        if !self.well_formed() || self.team != pin.team {
            return false;
        }
        // Who may sign: the previous version's owners, or the founder.
        let founder;
        let signers: Vec<&Member> = match prev {
            Some(p) => {
                if self.version <= p.version {
                    return false;
                }
                p.members.iter().filter(|m| m.role == TeamRole::Owner).collect()
            }
            None => {
                founder = Member {
                    account: pin.founder.clone(),
                    root: pin.founder_root.clone(),
                    role: TeamRole::Owner,
                    name: String::new(),
                };
                vec![&founder]
            }
        };
        signers.iter().any(|m| {
            let (c, r) = certs.get(&m.account).map(|(c, r)| (c.as_slice(), r.as_slice())).unwrap_or((&[], &[]));
            let trusted = Trust { account: m.account.clone(), root: m.root.clone() }.evaluate(c, r);
            trusted
                .get(&self.by)
                .is_some_and(|d| d.kind.approves() && verify_hex(&d.sign, self.body().as_bytes(), &self.sig))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Kind;

    struct Person {
        account: String,
        keys: DeviceKeys,
        cert: Cert,
    }

    fn person(account: &str) -> Person {
        let keys = DeviceKeys::generate();
        let mut cert = Cert::new(&keys, account, Kind::Browser, "laptop");
        cert.sign_with(&keys);
        Person { account: account.into(), keys, cert }
    }

    fn certs(people: &[&Person]) -> AccountCerts {
        people.iter().map(|p| (p.account.clone(), (vec![p.cert.clone()], vec![]))).collect()
    }

    fn member(p: &Person, role: TeamRole) -> Member {
        Member { account: p.account.clone(), root: p.cert.device.clone(), role, name: format!("{}-login", p.account) }
    }

    fn roster(version: u64, members: Vec<Member>, by: &Person) -> Roster {
        let mut r = Roster {
            v: 1,
            team: "t1".into(),
            name: "Acme".into(),
            version,
            at: 1,
            members,
            by: String::new(),
            sig: String::new(),
        };
        r.sign_with(&by.keys);
        r
    }

    #[test]
    fn owners_sign_each_version() {
        let (alice, bob, mallory) = (person("alice"), person("bob"), person("mallory"));
        let pin = TeamPin { team: "t1".into(), founder: "alice".into(), founder_root: alice.cert.device.clone() };
        let all = certs(&[&alice, &bob, &mallory]);
        let v1 = roster(1, vec![member(&alice, TeamRole::Owner)], &alice);
        assert!(v1.follows(None, &pin, &all));
        let v2 = roster(2, vec![member(&alice, TeamRole::Owner), member(&bob, TeamRole::Editor)], &alice);
        assert!(v2.follows(Some(&v1), &pin, &all));
        // An editor can't sign the next version, nor can an outsider.
        let by_bob = roster(3, vec![member(&alice, TeamRole::Owner), member(&bob, TeamRole::Owner)], &bob);
        assert!(!by_bob.follows(Some(&v2), &pin, &all));
        let by_mallory = roster(3, vec![member(&mallory, TeamRole::Owner)], &mallory);
        assert!(!by_mallory.follows(Some(&v2), &pin, &all));
        // Nor can control start the team over, or go back a version.
        assert!(!by_mallory.follows(None, &pin, &all));
        assert!(!v1.follows(Some(&v2), &pin, &all));
        // Ownership passes on: Alice makes Bob an owner, then Bob signs.
        let v3 = roster(3, vec![member(&alice, TeamRole::Editor), member(&bob, TeamRole::Owner)], &alice);
        assert!(v3.follows(Some(&v2), &pin, &all));
        let v4 = roster(4, vec![member(&bob, TeamRole::Owner)], &bob);
        assert!(v4.follows(Some(&v3), &pin, &all));
    }

    #[test]
    fn tampering_fails() {
        let (alice, bob) = (person("alice"), person("bob"));
        let pin = TeamPin { team: "t1".into(), founder: "alice".into(), founder_root: alice.cert.device.clone() };
        let all = certs(&[&alice, &bob]);
        let v1 = roster(1, vec![member(&alice, TeamRole::Owner), member(&bob, TeamRole::Viewer)], &alice);
        let mut promoted = v1.clone();
        promoted.members[1].role = TeamRole::Owner;
        assert!(!promoted.follows(None, &pin, &all));
        // A member's devices come from the root the roster lists.
        assert_eq!(v1.devices("bob", &all).devices.len(), 1);
        let mut moved = all.clone();
        let other = person("bob");
        moved.insert("bob".into(), (vec![other.cert], vec![]));
        assert_eq!(v1.devices("bob", &moved).devices.len(), 0);
    }
}
