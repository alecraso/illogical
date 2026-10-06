//! End-to-end encryption between clients and daemons (the control track;
//! design in docs/control-e2e.md).
//!
//! - [`keys`]: a device's two key pairs (X25519 for Noise, Ed25519 for
//!   signing) and their file.
//! - [`cert`]: device certificates, revocations, and checking a chain of
//!   them back to an account's pinned root, which is how a daemon decides
//!   who may connect without trusting control.
//! - [`channel`]: the Noise IK channel and what travels inside it.

pub mod cert;
pub mod channel;
pub mod keys;
pub mod mux;
pub mod push;
pub mod team;

#[cfg(test)]
mod frozen;

pub use cert::{Cert, Kind, Refusal, Revocation, Trust};
pub use keys::DeviceKeys;

/// Random bytes from the OS.
pub fn random<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).expect("the OS random source");
    b
}

/// Milliseconds since the epoch.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}
