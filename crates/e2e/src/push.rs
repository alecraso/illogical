//! Push through control (M21): a device's Web Push subscription, signed by
//! the device, so control can't swap in keys of its own and read what
//! daemons send. Daemons encrypt each notification to the subscription
//! (RFC 8291); control only adds the VAPID signature and posts it.
//!
//! ```text
//! illogical push v1
//! account <id>
//! device <id>
//! endpoint <url>
//! p256dh <base64url>
//! auth <base64url>
//! at <ms>
//! ```

use serde::{Deserialize, Serialize};

use crate::{Cert, DeviceKeys, cert::verify_hex};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushSub {
    pub v: u32,
    pub account: String,
    pub device: String,
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
    pub at: u64,
    pub sig: String,
}

fn clean(s: &str) -> bool {
    !s.is_empty() && s.len() <= 1000 && !s.chars().any(|c| c.is_whitespace() || c.is_control())
}

impl PushSub {
    pub fn body(&self) -> String {
        format!(
            "illogical push v1\naccount {}\ndevice {}\nendpoint {}\np256dh {}\nauth {}\nat {}\n",
            self.account, self.device, self.endpoint, self.p256dh, self.auth, self.at
        )
    }

    pub fn well_formed(&self) -> bool {
        self.v == 1 && [&self.account, &self.device, &self.endpoint, &self.p256dh, &self.auth].iter().all(|s| clean(s))
    }

    pub fn sign_with(&mut self, keys: &DeviceKeys) {
        self.device = keys.id();
        self.sig = hex::encode(keys.signature(self.body().as_bytes()));
    }

    /// Signed by `cert`'s device, of the same account.
    pub fn signed_by(&self, cert: &Cert) -> bool {
        self.well_formed()
            && cert.device == self.device
            && cert.account == self.account
            && verify_hex(&cert.sign, self.body().as_bytes(), &self.sig)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Kind;

    #[test]
    fn signed_subscriptions() {
        let k = DeviceKeys::generate();
        let mut c = Cert::new(&k, "acct", Kind::Browser, "phone");
        c.sign_with(&k);
        let mut s = PushSub {
            v: 1,
            account: "acct".into(),
            device: String::new(),
            endpoint: "https://fcm.googleapis.com/fcm/send/abc".into(),
            p256dh: "BLc4".into(),
            auth: "4vQK".into(),
            at: 1,
            sig: String::new(),
        };
        s.sign_with(&k);
        assert!(s.signed_by(&c));
        // Control swapping the key in is caught.
        let mut swapped = s.clone();
        swapped.p256dh = "BXXX".into();
        assert!(!swapped.signed_by(&c));
    }
}
