//! One device: its keys, its MLS group state, and the roster check it runs on
//! every commit before merging it.

use crate::ident::{self, Account, DeviceCert, DeviceCredential, MlsBinding};
use crate::roster::{CommitAad, RosterChain};
use ed25519_dalek::SigningKey;
use openmls::prelude::tls_codec::Deserialize as _;
use openmls::prelude::*;
use openmls::treesync::LeafNodeParameters;
use openmls_basic_credential::SignatureKeyPair;
#[cfg(not(feature = "libcrux"))]
pub type Provider = openmls_rust_crypto::OpenMlsRustCrypto;
#[cfg(feature = "libcrux")]
pub type Provider = openmls_libcrux_crypto::Provider;
use openmls_traits::OpenMlsProvider;
use openmls_traits::random::OpenMlsRand;
use openmls_traits::types::SignatureScheme;

/// X25519 HPKE, AES-128-GCM, SHA-256, Ed25519. See FINDINGS.md.
pub const CS: Ciphersuite = Ciphersuite::MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519;

pub fn random32(p: &Provider) -> [u8; 32] {
    p.rand().random_array().unwrap()
}

pub fn create_config(max_past_epochs: usize) -> MlsGroupCreateConfig {
    MlsGroupCreateConfig::builder()
        .ciphersuite(CS)
        .use_ratchet_tree_extension(true)
        .wire_format_policy(MIXED_CIPHERTEXT_WIRE_FORMAT_POLICY)
        .max_past_epochs(max_past_epochs)
        .sender_ratchet_configuration(SenderRatchetConfiguration::new(20, 1000))
        .build()
}

pub fn join_config(max_past_epochs: usize) -> MlsGroupJoinConfig {
    MlsGroupJoinConfig::builder()
        .use_ratchet_tree_extension(true)
        .wire_format_policy(MIXED_CIPHERTEXT_WIRE_FORMAT_POLICY)
        .max_past_epochs(max_past_epochs)
        .sender_ratchet_configuration(SenderRatchetConfiguration::new(20, 1000))
        .build()
}

pub struct Out {
    pub commit: Vec<u8>,
    pub welcome: Option<Vec<u8>>,
    /// GroupInfo for the new epoch, with the ratchet tree, for external joins.
    pub group_info: Vec<u8>,
}

#[derive(Debug)]
pub enum Event {
    App { from: String, text: String, epoch: u64 },
    Commit { from: String, epoch: u64, summary: String },
}

pub struct Device {
    pub name: String,
    pub account: String,
    pub provider: Provider,
    pub signer: SignatureKeyPair,
    pub cred: CredentialWithKey,
    pub group: Option<MlsGroup>,
    pub max_past_epochs: usize,
    /// The device's illogical `sign` key and certificate (signs rosters, invites).
    pub device_key: SigningKey,
    pub cert: DeviceCert,
    /// Rosters and revocations this device has verified. With `Some`, every
    /// commit is checked against them before it's merged.
    pub chain: Option<RosterChain>,
    /// The roster version and revocation count the group's state matches.
    pub applied: (u64, usize),
}

pub fn label(cred: &Credential) -> String {
    let bc = BasicCredential::try_from(cred.clone()).ok();
    match bc.as_ref().and_then(|b| DeviceCredential::from_bytes(b.identity())) {
        Some(dc) => format!("{}/{}", dc.cert.account, dc.cert.device),
        None => bc.map(|b| String::from_utf8_lossy(b.identity()).into_owned()).unwrap_or_else(|| "?".into()),
    }
}

fn identity_bytes(cred: &Credential) -> Vec<u8> {
    BasicCredential::try_from(cred.clone()).map(|b| b.identity().to_vec()).unwrap_or_default()
}

impl Device {
    /// A device of `acct`: a fresh device `sign` key, a certificate from the
    /// account, a fresh MLS signature key, and the binding between them.
    pub fn new(acct: &Account, device: &str, max_past_epochs: usize) -> Self {
        let provider = Provider::default();
        let device_key = SigningKey::from_bytes(&random32(&provider));
        let cert = acct.issue(device, &ident::pub_hex(&device_key));
        let signer = SignatureKeyPair::new(SignatureScheme::ED25519).unwrap();
        signer.store(provider.storage()).unwrap();
        let mls_key = hex::encode(signer.public());
        let binding = MlsBinding { sig: ident::sign(&device_key, "illogical mls leaf v1", mls_key.as_bytes()), mls_key };
        let dc = DeviceCredential { cert: cert.clone(), binding };
        let cred = CredentialWithKey { credential: BasicCredential::new(dc.to_bytes()).into(), signature_key: signer.public().into() };
        Device { name: format!("{}/{}", acct.id, device), account: acct.id.clone(), provider, signer, cred, group: None, max_past_epochs, device_key, cert, chain: None, applied: (0, 0) }
    }

    pub fn g(&self) -> &MlsGroup {
        self.group.as_ref().expect("not in a group")
    }
    pub fn epoch(&self) -> u64 {
        self.group.as_ref().map(|g| g.epoch().as_u64()).unwrap_or(0)
    }
    pub fn epoch_auth(&self) -> String {
        hex::encode(&self.g().epoch_authenticator().as_slice()[..6])
    }
    pub fn members(&self) -> Vec<String> {
        self.g().members().map(|m| label(&m.credential)).collect()
    }
    pub fn leaves_of(&self, pred: impl Fn(&str) -> bool) -> Vec<LeafNodeIndex> {
        self.g().members().filter(|m| pred(&label(&m.credential))).map(|m| m.index).collect()
    }

    pub fn key_package(&self) -> Vec<u8> {
        let kpb = KeyPackage::builder().build(CS, &self.provider, &self.signer, self.cred.clone()).unwrap();
        MlsMessageOut::from(kpb.key_package().clone()).to_bytes().unwrap()
    }

    pub fn create_group(&mut self, id: &[u8]) {
        let g = MlsGroup::new_with_group_id(&self.provider, &self.signer, &create_config(self.max_past_epochs), GroupId::from_slice(id), self.cred.clone()).unwrap();
        self.group = Some(g);
    }

    fn group_info(&self) -> Vec<u8> {
        self.g().export_group_info(self.provider.crypto(), &self.signer, true).unwrap().to_bytes().unwrap()
    }

    pub fn add(&mut self, key_packages: &[Vec<u8>], aad: Vec<u8>) -> Result<Out, String> {
        let kps: Vec<KeyPackage> = key_packages
            .iter()
            .map(|b| match MlsMessageIn::tls_deserialize_exact(b).unwrap().extract() {
                MlsMessageBodyIn::KeyPackage(k) => k.validate(self.provider.crypto(), ProtocolVersion::Mls10).unwrap(),
                _ => panic!("not a key package"),
            })
            .collect();
        let (p, s) = (&self.provider, &self.signer);
        let g = self.group.as_mut().unwrap();
        g.set_aad(aad);
        let (commit, welcome, _) = g.add_members(p, s, &kps).map_err(|e| format!("{e:?}"))?;
        g.merge_pending_commit(p).unwrap();
        Ok(Out { commit: commit.to_bytes().unwrap(), welcome: Some(welcome.to_bytes().unwrap()), group_info: self.group_info() })
    }

    pub fn remove(&mut self, leaves: &[LeafNodeIndex], aad: Vec<u8>) -> Result<Out, String> {
        let (p, s) = (&self.provider, &self.signer);
        let g = self.group.as_mut().unwrap();
        g.set_aad(aad);
        let (commit, _, _) = g.remove_members(p, s, leaves).map_err(|e| format!("{e:?}"))?;
        g.merge_pending_commit(p).unwrap();
        Ok(Out { commit: commit.to_bytes().unwrap(), welcome: None, group_info: self.group_info() })
    }

    pub fn self_update(&mut self, aad: Vec<u8>) -> Result<Out, String> {
        let (p, s) = (&self.provider, &self.signer);
        let g = self.group.as_mut().unwrap();
        g.set_aad(aad);
        let b = g.self_update(p, s, LeafNodeParameters::default()).map_err(|e| format!("{e:?}"))?;
        g.merge_pending_commit(p).unwrap();
        Ok(Out { commit: b.into_commit().to_bytes().unwrap(), welcome: None, group_info: self.group_info() })
    }

    /// Stage a removal without merging it (to show others refusing it).
    pub fn remove_staged(&mut self, leaves: &[LeafNodeIndex], aad: Vec<u8>) -> Vec<u8> {
        let (p, s) = (&self.provider, &self.signer);
        let g = self.group.as_mut().unwrap();
        g.set_aad(aad);
        g.remove_members(p, s, leaves).unwrap().0.to_bytes().unwrap()
    }

    /// Stage a commit without merging it (for the fork tests).
    pub fn self_update_staged(&mut self, aad: Vec<u8>) -> Vec<u8> {
        let (p, s) = (&self.provider, &self.signer);
        let g = self.group.as_mut().unwrap();
        g.set_aad(aad);
        g.self_update(p, s, LeafNodeParameters::default()).unwrap().into_commit().to_bytes().unwrap()
    }
    pub fn merge_pending(&mut self) {
        let p = &self.provider;
        self.group.as_mut().unwrap().merge_pending_commit(p).unwrap();
    }
    pub fn clear_pending(&mut self) {
        let p = &self.provider;
        self.group.as_mut().unwrap().clear_pending_commit(p.storage()).unwrap();
    }

    pub fn join_welcome(&mut self, welcome: &[u8]) -> Result<(), String> {
        let msg = MlsMessageIn::tls_deserialize_exact(welcome).map_err(|e| e.to_string())?;
        let MlsMessageBodyIn::Welcome(w) = msg.extract() else { return Err("not a welcome".into()) };
        let staged = StagedWelcome::new_from_welcome(&self.provider, &join_config(self.max_past_epochs), w, None).map_err(|e| format!("{e:?}"))?;
        self.group = Some(staged.into_group(&self.provider).map_err(|e| format!("{e:?}"))?);
        Ok(())
    }

    /// Join (or rejoin) from a published GroupInfo with an external commit.
    pub fn join_external(&mut self, group_info: &[u8], aad: Vec<u8>) -> Result<Out, String> {
        let msg = MlsMessageIn::tls_deserialize_exact(group_info).map_err(|e| e.to_string())?;
        let MlsMessageBodyIn::GroupInfo(gi) = msg.extract() else { return Err("not a group info".into()) };
        let p = &self.provider;
        let (mut g, bundle) = MlsGroup::external_commit_builder()
            .with_config(join_config(self.max_past_epochs))
            .with_aad(aad)
            .build_group(p, gi, self.cred.clone())
            .map_err(|e| format!("{e:?}"))?
            .load_psks(p.storage())
            .map_err(|e| format!("{e:?}"))?
            .build(p.rand(), p.crypto(), &self.signer, |_| true)
            .map_err(|e| format!("{e:?}"))?
            .finalize(p)
            .map_err(|e| format!("{e:?}"))?;
        g.merge_pending_commit(p).unwrap();
        // An old group state, if any, is replaced.
        self.group = Some(g);
        Ok(Out { commit: bundle.into_commit().to_bytes().unwrap(), welcome: None, group_info: self.group_info() })
    }

    pub fn send(&mut self, text: &str) -> Vec<u8> {
        let (p, s) = (&self.provider, &self.signer);
        self.group.as_mut().unwrap().create_message(p, s, text.as_bytes()).unwrap().to_bytes().unwrap()
    }

    /// The AAD for a commit from this device: the newest roster state it has verified.
    pub fn aad(&self) -> Vec<u8> {
        self.chain.as_ref().map(|c| c.aad()).unwrap_or_default()
    }
    /// Mark the group state as matching the newest roster state (after this
    /// device commits, or when it joins).
    pub fn applied_now(&mut self) {
        if let Some(c) = &self.chain {
            self.applied = (c.head().v, c.revocations.len());
        }
    }

    /// Process one message from the delivery service. With a roster chain,
    /// every commit is checked against the signed roster version its AAD
    /// names before it's merged; a commit that fails is not merged.
    pub fn receive(&mut self, bytes: &[u8]) -> Result<Event, String> {
        let msg = MlsMessageIn::tls_deserialize_exact(bytes).map_err(|e| e.to_string())?;
        let pm = msg.try_into_protocol_message().map_err(|e| format!("{e:?}"))?;
        let p = &self.provider;
        let g = self.group.as_mut().ok_or("not in a group")?;
        let processed = g.process_message(p, pm).map_err(|e| format!("{e}"))?;
        let from = label(processed.credential());
        let epoch = processed.epoch().as_u64();
        let aad = processed.aad().to_vec();
        let sender = processed.sender().clone();
        let credential = processed.credential().clone();
        match processed.into_content() {
            ProcessedMessageContent::ApplicationMessage(m) => Ok(Event::App { from, text: String::from_utf8_lossy(&m.into_bytes()).into(), epoch }),
            ProcessedMessageContent::StagedCommitMessage(staged) => {
                let summary = summarize(g, &staged, &sender, &credential);
                if let Some(chain) = &self.chain {
                    let g = self.group.as_ref().unwrap();
                    match check_commit(g, p, &staged, &aad, chain, self.applied) {
                        Ok(a) => self.applied = a,
                        Err(e) => return Err(format!("rejected commit from {from}: {e}")),
                    }
                }
                let g = self.group.as_mut().unwrap();
                g.merge_staged_commit(p, *staged).map_err(|e| format!("{e:?}"))?;
                Ok(Event::Commit { from, epoch: g.epoch().as_u64(), summary })
            }
            _ => Err("unexpected proposal".into()),
        }
    }
}

fn summarize(g: &MlsGroup, staged: &StagedCommit, sender: &Sender, cred: &Credential) -> String {
    let mut parts = vec![];
    if matches!(sender, Sender::NewMemberCommit) {
        parts.push(format!("external join by {}", label(cred)));
    }
    for a in staged.add_proposals() {
        parts.push(format!("add {}", label(a.add_proposal().key_package().leaf_node().credential())));
    }
    for r in staged.remove_proposals() {
        let who = g.member(r.remove_proposal().removed()).map(label).unwrap_or_else(|| "?".into());
        parts.push(format!("remove {who}"));
    }
    if parts.is_empty() {
        parts.push("update".into());
    }
    parts.join(", ")
}

/// The roster check (FINDINGS.md, "Binding MLS to the roster"):
/// 1. the AAD names a roster version this client has verified, no older than the one already applied;
/// 2. every leaf in the new epoch's tree is a device the roster allows (certificate chains to a
///    member account, MLS key bound to the device key, not revoked, owner-only when locked);
/// 3. every leaf the commit drops is one the roster no longer allows (no kicking valid members).
fn check_commit(g: &MlsGroup, p: &Provider, staged: &StagedCommit, aad: &[u8], chain: &RosterChain, applied: (u64, usize)) -> Result<(u64, usize), String> {
    let a = CommitAad::from_bytes(aad).ok_or("no roster version in the AAD")?;
    let roster = chain.get(&a.roster).ok_or_else(|| format!("roster v{} isn't one this device has verified (fetch it)", a.roster.v))?;
    let revs = a.revocations;
    if revs > chain.revocations.len() {
        return Err(format!("names {revs} revocations but this device has {} (fetch them)", chain.revocations.len()));
    }
    if roster.v < applied.0 || revs < applied.1 {
        return Err(format!("names roster v{} with {revs} revocations, older than the applied v{} with {}", roster.v, applied.0, applied.1));
    }
    let Some(tree) = staged.export_ratchet_tree(p.crypto(), g.export_ratchet_tree()).map_err(|e| format!("{e:?}"))? else {
        // This device is the one removed, so it doesn't get the new tree. It
        // goes along only if the roster no longer allows it.
        let me = g.own_leaf_node().ok_or("no own leaf")?;
        if chain.authorize(roster, revs, &identity_bytes(me.credential()), me.signature_key().as_slice()).is_ok() {
            return Err(format!("removes this device, which roster v{} still allows", roster.v));
        }
        return Ok((roster.v, revs));
    };
    let after: Vec<(Vec<u8>, Vec<u8>)> = tree.leaves().map(|l| (identity_bytes(l.credential()), l.signature_key().as_slice().to_vec())).collect();
    for (cred, key) in &after {
        chain.authorize(roster, revs, cred, key)?;
    }
    for m in g.members() {
        if !after.iter().any(|(_, k)| k == m.signature_key.as_slice()) && chain.authorize(roster, revs, &identity_bytes(&m.credential), &m.signature_key).is_ok() {
            return Err(format!("removes {}, whom roster v{} still allows", label(&m.credential), roster.v));
        }
    }
    Ok((roster.v, revs))
}
