//! The channel: Noise IK between a client device (initiator) and a daemon
//! (responder), and what travels inside it.
//!
//! **Handshake.** `Noise_IK_25519_AESGCM_SHA256`, prologue
//! `illogical/1\n<daemon device id>\n`, so a client spliced onto the wrong
//! daemon fails the handshake. Message 1's payload is replayable (it's
//! encrypted to the daemon's static key only), so it carries nothing that
//! acts: the client sends an empty one.
//!
//! **Transport.** Each wire message (one WebSocket message, or one
//! length-prefixed frame on a relay stream) is one Noise message, whose
//! plaintext is `more (1 byte: 0 last, 1 more follows) ‖ chunk`. Chunks
//! join into one [`Msg`]:
//!
//! ```text
//! 'T' text                                          the WebSocket protocol's JSON
//! 'B' bytes                                         its binary frames
//! 'Q' id (u32) ‖ head len (u32) ‖ head JSON ‖ body  an HTTP request
//! 'R' id (u32) ‖ head len (u32) ‖ head JSON ‖ body  its response
//! ```
//!
//! So one channel carries both what a page does over `/ws` and its API
//! calls, and the daemon answers the requests with its own router.
//!
//! **Streamed answers.** A request whose head says `stream` may be answered
//! in parts: every `'R'` for it but the last has `more` set, and the body
//! is the parts joined. The daemon streams only an answer of no fixed
//! length (`events?follow=1`, `tail?follow=1`); a request without `stream`
//! always gets one `'R'`, so an older client sees no change.

use std::sync::Mutex;

use anyhow::{Context, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::keys::DeviceKeys;

pub const PARAMS: &str = "Noise_IK_25519_AESGCM_SHA256";
/// Largest plaintext chunk we send.
pub const CHUNK: usize = 16 * 1024;
/// Largest Noise message.
pub const MAX_WIRE: usize = 65535;
/// Largest joined message we accept (a big snapshot is a few MB).
pub const MAX_MSG: usize = 64 << 20;
const TAG: usize = 16;

pub fn prologue(daemon_id: &str) -> Vec<u8> {
    // Frozen (#504): both ends of every channel must agree; see `frozen.rs`.
    format!("illogical/1\n{daemon_id}\n").into_bytes()
}

fn builder() -> snow::Builder<'static> {
    snow::Builder::new(PARAMS.parse().unwrap())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestHead {
    pub method: String,
    /// Path and query.
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    /// The answer may come in parts (see the module docs).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stream: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseHead {
    pub status: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    /// More parts of this answer follow.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Msg {
    Text(String),
    Binary(Vec<u8>),
    Request { id: u32, head: RequestHead, body: Vec<u8> },
    Response { id: u32, head: ResponseHead, body: Vec<u8> },
}

impl Msg {
    pub fn encode(&self) -> Vec<u8> {
        fn with_head<H: Serialize>(kind: u8, id: u32, head: &H, body: &[u8]) -> Vec<u8> {
            let h = serde_json::to_vec(head).expect("serialize head");
            let mut v = Vec::with_capacity(9 + h.len() + body.len());
            v.push(kind);
            v.extend_from_slice(&id.to_be_bytes());
            v.extend_from_slice(&(h.len() as u32).to_be_bytes());
            v.extend_from_slice(&h);
            v.extend_from_slice(body);
            v
        }
        match self {
            Msg::Text(t) => [b"T", t.as_bytes()].concat(),
            Msg::Binary(b) => [b"B", b.as_slice()].concat(),
            Msg::Request { id, head, body } => with_head(b'Q', *id, head, body),
            Msg::Response { id, head, body } => with_head(b'R', *id, head, body),
        }
    }

    pub fn decode(b: &[u8]) -> anyhow::Result<Self> {
        fn headed(rest: &[u8]) -> anyhow::Result<(u32, &[u8], Vec<u8>)> {
            ensure!(rest.len() >= 8, "short message");
            let id = u32::from_be_bytes(rest[..4].try_into()?);
            let n = u32::from_be_bytes(rest[4..8].try_into()?) as usize;
            ensure!(rest.len() >= 8 + n, "short head");
            Ok((id, &rest[8..8 + n], rest[8 + n..].to_vec()))
        }
        let (&kind, rest) = b.split_first().context("empty message")?;
        Ok(match kind {
            b'T' => Msg::Text(String::from_utf8(rest.to_vec())?),
            b'B' => Msg::Binary(rest.to_vec()),
            b'Q' => {
                let (id, head, body) = headed(rest)?;
                Msg::Request { id, head: serde_json::from_slice(head)?, body }
            }
            b'R' => {
                let (id, head, body) = headed(rest)?;
                Msg::Response { id, head: serde_json::from_slice(head)?, body }
            }
            k => bail!("unknown message kind {k}"),
        })
    }
}

/// An established channel. Both directions can be used from different
/// tasks at once.
pub struct Channel {
    t: Mutex<snow::TransportState>,
    partial: Mutex<Vec<u8>>,
    /// The handshake hash (the same on both ends).
    pub hash: Vec<u8>,
    /// The other end's static key.
    pub remote: [u8; 32],
}

impl Channel {
    fn new(hs: snow::HandshakeState) -> anyhow::Result<Self> {
        let hash = hs.get_handshake_hash().to_vec();
        let remote = hs.get_remote_static().context("no remote static key")?.try_into()?;
        Ok(Self { t: Mutex::new(hs.into_transport_mode()?), partial: Mutex::new(Vec::new()), hash, remote })
    }

    /// The wire messages for `msg`, in order.
    pub fn seal(&self, msg: &Msg) -> anyhow::Result<Vec<Vec<u8>>> {
        let plain = msg.encode();
        let mut t = self.t.lock().unwrap();
        let mut out = Vec::with_capacity(plain.len() / CHUNK + 1);
        let mut chunks = plain.chunks(CHUNK).peekable();
        let mut buf = vec![0u8; CHUNK + 1 + TAG];
        let mut chunk_plain = Vec::with_capacity(CHUNK + 1);
        while let Some(c) = chunks.next() {
            chunk_plain.clear();
            chunk_plain.push(chunks.peek().is_some() as u8);
            chunk_plain.extend_from_slice(c);
            let n = t.write_message(&chunk_plain, &mut buf)?;
            out.push(buf[..n].to_vec());
        }
        Ok(out)
    }

    /// One wire message in; a whole [`Msg`] out when its last chunk came.
    /// An error means the channel is broken (tampering, or a peer that
    /// doesn't follow the protocol) and must be dropped.
    pub fn open(&self, wire: &[u8]) -> anyhow::Result<Option<Msg>> {
        ensure!(wire.len() <= MAX_WIRE, "wire message too large");
        let mut buf = vec![0u8; wire.len()];
        let n = self.t.lock().unwrap().read_message(wire, &mut buf).context("decrypting")?;
        let (&more, chunk) = buf[..n].split_first().context("empty chunk")?;
        let mut partial = self.partial.lock().unwrap();
        ensure!(partial.len() + chunk.len() <= MAX_MSG, "message too large");
        partial.extend_from_slice(chunk);
        if more != 0 {
            return Ok(None);
        }
        let whole = std::mem::take(&mut *partial);
        Ok(Some(Msg::decode(&whole)?))
    }
}

/// The daemon's half of a handshake that's under way.
pub struct Responder {
    hs: snow::HandshakeState,
}

impl Responder {
    /// Take message 1. Returns who is connecting (their static key) so the
    /// caller can decide before answering.
    pub fn read(keys: &DeviceKeys, prologue: &[u8], msg1: &[u8]) -> anyhow::Result<(Self, [u8; 32])> {
        let mut hs = builder().local_private_key(&keys.noise_private)?.prologue(prologue)?.build_responder()?;
        let mut buf = vec![0u8; msg1.len()];
        hs.read_message(msg1, &mut buf).context("handshake message 1")?;
        let who: [u8; 32] = hs.get_remote_static().context("no static key")?.try_into()?;
        Ok((Self { hs }, who))
    }

    /// Message 2, and the channel.
    pub fn finish(mut self, payload: &[u8]) -> anyhow::Result<(Vec<u8>, Channel)> {
        let mut buf = vec![0u8; 64 + payload.len() + TAG];
        let n = self.hs.write_message(payload, &mut buf)?;
        buf.truncate(n);
        Ok((buf, Channel::new(self.hs)?))
    }
}

/// The client's half (the CLI, tests; browsers have their own in TS).
pub struct Initiator {
    hs: snow::HandshakeState,
}

impl Initiator {
    pub fn start(keys: &DeviceKeys, daemon_noise: &[u8; 32], prologue: &[u8]) -> anyhow::Result<(Self, Vec<u8>)> {
        let mut hs = builder()
            .local_private_key(&keys.noise_private)?
            .remote_public_key(daemon_noise)?
            .prologue(prologue)?
            .build_initiator()?;
        let mut buf = vec![0u8; 128];
        let n = hs.write_message(&[], &mut buf)?;
        buf.truncate(n);
        Ok((Self { hs }, buf))
    }

    pub fn finish(mut self, msg2: &[u8]) -> anyhow::Result<(Vec<u8>, Channel)> {
        let mut buf = vec![0u8; msg2.len()];
        let n = self.hs.read_message(msg2, &mut buf).context("handshake message 2")?;
        buf.truncate(n);
        Ok((buf, Channel::new(self.hs)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> (Channel, Channel) {
        let (d, c) = (DeviceKeys::generate(), DeviceKeys::generate());
        let p = prologue(&d.id());
        let (i, m1) = Initiator::start(&c, &d.noise_public, &p).unwrap();
        let (r, who) = Responder::read(&d, &p, &m1).unwrap();
        assert_eq!(who, c.noise_public);
        let (m2, server) = r.finish(b"hello").unwrap();
        let (payload, client) = i.finish(&m2).unwrap();
        assert_eq!(payload, b"hello");
        assert_eq!(client.hash, server.hash);
        (client, server)
    }

    fn deliver(from: &Channel, to: &Channel, m: &Msg) -> Msg {
        let wires = from.seal(m).unwrap();
        let mut got = None;
        for (i, w) in wires.iter().enumerate() {
            assert!(w.len() <= MAX_WIRE);
            let r = to.open(w).unwrap();
            assert_eq!(r.is_some(), i == wires.len() - 1);
            got = r;
        }
        got.unwrap()
    }

    #[test]
    fn messages_round_trip() {
        let (c, s) = pair();
        for m in [
            Msg::Text(r#"{"type":"attach"}"#.into()),
            Msg::Binary(vec![7; 3 * CHUNK + 5]),
            Msg::Binary(vec![]),
            Msg::Request {
                id: 9,
                head: RequestHead {
                    method: "POST".into(),
                    path: "/api/run?x=1".into(),
                    content_type: Some("application/json".into()),
                    stream: true,
                },
                body: b"{}".to_vec(),
            },
        ] {
            assert_eq!(deliver(&c, &s, &m), m);
        }
        let r = Msg::Response {
            id: 9,
            head: ResponseHead { status: 200, content_type: None, more: false },
            body: vec![1; 100_000],
        };
        assert_eq!(deliver(&s, &c, &r), r);
    }

    #[test]
    fn wrong_daemon_fails() {
        let (d, other, c) = (DeviceKeys::generate(), DeviceKeys::generate(), DeviceKeys::generate());
        // The client thinks it's reaching `d`, the relay splices it to `other`.
        let (_, m1) = Initiator::start(&c, &d.noise_public, &prologue(&d.id())).unwrap();
        assert!(Responder::read(&other, &prologue(&other.id()), &m1).is_err());
    }

    #[test]
    fn tampering_breaks_the_channel() {
        let (c, s) = pair();
        let mut w = c.seal(&Msg::Text("hi".into())).unwrap().remove(0);
        w[3] ^= 1;
        assert!(s.open(&w).is_err());
    }
}
