//! A stub delivery service: what control would be for team channels. It
//! orders, stores and fans out ciphertext. It sees the group id, epoch and
//! content type of each message (MLS leaves those in the clear) and who
//! uploaded it, and nothing else.

use openmls::prelude::tls_codec::Deserialize as _;
use openmls::prelude::*;
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct Entry {
    pub seq: u64,
    pub epoch: u64,
    pub commit: bool,
    pub from: String,
    pub bytes: Vec<u8>,
}

#[derive(Default)]
pub struct Ds {
    pub log: Vec<Entry>,
    /// The current epoch as far as the DS knows (the next commit must be for it).
    pub epoch: u64,
    /// Latest GroupInfo, uploaded with each commit, for external joins.
    pub group_info: Option<Vec<u8>>,
    pub welcomes: HashMap<String, Vec<u8>>,
    /// Per device: the next seq it hasn't fetched.
    cursors: HashMap<String, u64>,
    next_seq: u64,
}

/// What the DS can read off a message without any key.
pub fn header(bytes: &[u8]) -> (u64, bool) {
    let m = MlsMessageIn::tls_deserialize_exact(bytes).unwrap();
    let pm = m.try_into_protocol_message().unwrap();
    (pm.epoch().as_u64(), pm.content_type() == ContentType::Commit)
}

impl Ds {
    pub fn new(epoch: u64) -> Self {
        Ds { epoch, ..Default::default() }
    }

    /// First commit for the current epoch wins; a later one for the same
    /// epoch is refused, and its sender has to catch up and try again.
    pub fn submit_commit(&mut self, from: &str, bytes: Vec<u8>, group_info: Vec<u8>) -> Result<u64, String> {
        let (epoch, commit) = header(&bytes);
        assert!(commit);
        if epoch != self.epoch {
            return Err(format!("stale: commit is for epoch {epoch}, the group is at {}", self.epoch));
        }
        self.epoch += 1;
        self.group_info = Some(group_info);
        Ok(self.push(from, epoch, true, bytes))
    }

    pub fn submit(&mut self, from: &str, bytes: Vec<u8>) -> u64 {
        let (epoch, commit) = header(&bytes);
        assert!(!commit);
        self.push(from, epoch, false, bytes)
    }

    fn push(&mut self, from: &str, epoch: u64, commit: bool, bytes: Vec<u8>) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.log.push(Entry { seq, epoch, commit, from: from.into(), bytes });
        seq
    }

    /// Everything after the device's cursor that it didn't send itself.
    pub fn fetch(&mut self, device: &str) -> Vec<Entry> {
        let c = self.cursors.entry(device.into()).or_insert(0);
        let out = self.log.iter().filter(|e| e.seq >= *c && e.from != device).cloned().collect();
        *c = self.next_seq;
        out
    }

    /// A hostile or forgetful DS can put anything in the log, skipping its own rules.
    pub fn inject(&mut self, from: &str, bytes: Vec<u8>) -> u64 {
        let (epoch, commit) = header(&bytes);
        self.push(from, epoch, commit, bytes)
    }

    /// Retention: forget everything before `seq`.
    pub fn prune_before(&mut self, seq: u64) {
        self.log.retain(|e| e.seq >= seq);
    }

    pub fn entry(&self, seq: u64) -> &Entry {
        self.log.iter().find(|e| e.seq == seq).unwrap()
    }

    /// Start a device's cursor at the end (it joined now).
    pub fn mark_joined(&mut self, device: &str) {
        self.cursors.insert(device.into(), self.next_seq);
    }
}
