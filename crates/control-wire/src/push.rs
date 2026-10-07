//! Web push through control (M21): the subscriptions a daemon serves, and
//! the encrypted notifications it has control relay. Control sees only
//! ciphertext.

use arugula_e2e::push::PushSub;
use serde::{Deserialize, Serialize};

/// `GET /api/daemon/push-subs`: the subscriptions of the people the daemon
/// serves, for it to check the signatures of. Control keeps each as the
/// device signed it and never reads one, so it answers with
/// `PushSubs<serde_json::Value>`: the stored bodies, verbatim.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushSubs<S = PushSub> {
    pub subs: Vec<S>,
}

/// `POST /api/daemon/push`: one notification, encrypted for one subscription.
/// A daemon sends only `endpoint` and `body`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushRequest {
    /// The subscription's push-service URL.
    pub endpoint: String,
    /// The encrypted notification (aes128gcm), standard base64.
    pub body: String,
    /// Seconds the push service keeps it; control takes an hour when it's
    /// left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl: Option<u32>,
    /// `very-low`, `low`, `normal` or `high`; control takes `high` when it's
    /// left out or isn't one of those.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub urgency: Option<String>,
}

/// Control's answer to a push: what the push service said.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct PushAnswer {
    pub status: u16,
}
