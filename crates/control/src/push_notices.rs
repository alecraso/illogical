//! #104 inside control: a new device waiting and a team join request push
//! to the right people's subscriptions (a push service of our own, whose
//! messages are decrypted here as a browser would), once each, saying
//! only that something waits.

use std::{sync::Arc, time::Duration};

use axum::{Json, extract::State};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use illogical_e2e::{
    Cert, DeviceKeys, Kind, now_ms,
    push::PushSub,
    team::{Member, Roster, TeamRole},
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
    sync::mpsc,
};

use crate::{App, auth::Session, db::Team};

/// A browser's push subscription: its keys, and what its service got.
struct Phone {
    ua: p256::SecretKey,
    auth: [u8; 16],
    got: mpsc::UnboundedReceiver<Vec<u8>>,
}

impl Phone {
    /// Subscribed for `account`'s approved device `keys`, at a push service
    /// on a port of its own.
    async fn subscribe(app: &App, account: &str, keys: &DeviceKeys, seed: u8) -> (Self, String) {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let host = l.local_addr().unwrap().to_string();
        let (tx, got) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            loop {
                let Ok((conn, _)) = l.accept().await else { return };
                let tx = tx.clone();
                tokio::spawn(async move {
                    let mut r = BufReader::new(conn);
                    let mut len = 0;
                    loop {
                        let mut line = String::new();
                        r.read_line(&mut line).await.unwrap();
                        if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                            len = v.trim().parse().unwrap();
                        }
                        if line == "\r\n" {
                            break;
                        }
                    }
                    let mut body = vec![0; len];
                    r.read_exact(&mut body).await.unwrap();
                    let _ = tx.send(body);
                    r.into_inner().write_all(b"HTTP/1.1 201 Created\r\ncontent-length: 0\r\n\r\n").await.unwrap();
                });
            }
        });
        let ua = p256::SecretKey::from_slice(&[seed; 32]).unwrap();
        let auth = [seed; 16];
        let mut sub = PushSub {
            v: 1,
            account: account.into(),
            device: String::new(),
            endpoint: format!("http://{host}/push/{seed}"),
            p256dh: B64.encode(ua.public_key().to_sec1_bytes()),
            auth: B64.encode(auth),
            at: now_ms(),
            sig: String::new(),
        };
        sub.sign_with(keys);
        app.db.put_push_sub(&sub.endpoint, account, &serde_json::to_string(&sub).unwrap()).unwrap();
        (Self { ua, auth, got }, host)
    }

    /// The next notification, decrypted (RFC 8291), or none soon.
    async fn next(&mut self) -> Option<Value> {
        use aes_gcm::{Aes128Gcm, KeyInit, aead::Aead};
        use hkdf::Hkdf;
        use sha2::Sha256;

        let body = tokio::time::timeout(Duration::from_secs(5), self.got.recv()).await.ok()??;
        let (salt, rest) = body.split_at(16);
        let id_len = rest[4] as usize;
        let (as_public, sealed) = rest[5..].split_at(id_len);
        let ua_public = self.ua.public_key().to_sec1_bytes();
        let shared = p256::ecdh::diffie_hellman(
            self.ua.to_nonzero_scalar(),
            p256::PublicKey::from_sec1_bytes(as_public).unwrap().as_affine(),
        );
        let mut info = b"WebPush: info\0".to_vec();
        info.extend_from_slice(&ua_public);
        info.extend_from_slice(as_public);
        let mut ikm = [0u8; 32];
        Hkdf::<Sha256>::new(Some(&self.auth), shared.raw_secret_bytes().as_ref()).expand(&info, &mut ikm).unwrap();
        let prk = Hkdf::<Sha256>::new(Some(salt), &ikm);
        let (mut cek, mut nonce) = ([0u8; 16], [0u8; 12]);
        prk.expand(b"Content-Encoding: aes128gcm\0", &mut cek).unwrap();
        prk.expand(b"Content-Encoding: nonce\0", &mut nonce).unwrap();
        let mut plain = Aes128Gcm::new_from_slice(&cek).unwrap().decrypt(&nonce.into(), sealed).unwrap();
        assert_eq!(plain.pop(), Some(2));
        Some(serde_json::from_slice(&plain).unwrap())
    }

    /// Nothing more within a moment.
    async fn quiet(&mut self) -> bool {
        tokio::time::timeout(Duration::from_millis(500), self.got.recv()).await.is_err()
    }
}

/// An account with an approved first device.
fn person(app: &App, provider: &str, subject: &str, login: &str, id: &str) -> DeviceKeys {
    app.db.account_for(provider, subject, login, id, now_ms()).unwrap();
    let keys = DeviceKeys::generate();
    let mut c = Cert::new(&keys, id, Kind::Browser, "laptop");
    c.sign_with(&keys);
    app.db.put_device(&c, true, now_ms()).unwrap();
    keys
}

#[tokio::test]
async fn waiting_devices_and_requests_push_their_owners() {
    let mut app = App::for_tests("http://control.test");
    let jake = person(&app, "github", "1", "jhgaylor", "a1jake");
    let ada = person(&app, "passkey", "p2", "", "a2ada");
    app.db.set_name("a2ada", "Ada Lovelace").unwrap();
    let (mut phone, host) = Phone::subscribe(&app, "a1jake", &jake, 7).await;
    let (mut ada_phone, ada_host) = Phone::subscribe(&app, "a2ada", &ada, 8).await;
    app.cfg.push_hosts = vec![host, ada_host];
    let app = Arc::new(app);

    // A new browser asks into Jake's account: his phone hears, once.
    let asking = Cert::new(&DeviceKeys::generate(), "a1jake", Kind::Browser, "Safari on iPhone");
    let enroll = || {
        crate::api::enroll(
            State(app.clone()),
            Session { account: "a1jake".into() },
            Json(serde_json::from_value(json!({ "cert": asking })).unwrap()),
        )
    };
    assert_eq!(enroll().await.unwrap().0["approved"], false);
    let n = phone.next().await.expect("a push for the waiting device");
    assert_eq!(n["title"], "A new browser wants into your account");
    assert_eq!(n["control"], true);
    // Nothing that would let anyone in: no fingerprint, no device id.
    assert!(!n.to_string().contains(&asking.device));
    assert_eq!(enroll().await.unwrap().0["approved"], false);
    assert!(phone.quiet().await, "the waiting page reloading doesn't push again");
    assert!(ada_phone.quiet().await, "nobody else hears of it");

    // Ada asks to join Jake's team: its owners hear, by her name.
    let team = "0123456789abcdef";
    let jake_root = jake.id();
    let roster = Roster {
        v: 1,
        team: team.into(),
        name: "Acme".into(),
        version: 1,
        at: now_ms(),
        members: vec![Member {
            account: "a1jake".into(),
            root: jake_root.clone(),
            role: TeamRole::Owner,
            name: "jhgaylor".into(),
        }],
        spent: vec![],
        redeem: None,
        by: jake_root.clone(),
        sig: String::new(),
    };
    app.db
        .add_team(
            &Team {
                id: team.into(),
                name: "Acme".into(),
                founder: "a1jake".into(),
                founder_root: jake_root,
                locked: false,
            },
            1,
            &serde_json::to_string(&roster).unwrap(),
            now_ms(),
        )
        .unwrap();
    let code = "invitecode0123456789";
    app.db.add_invite(&crate::auth::hash(code), team, "editor", now_ms() + 60_000, "a1jake").unwrap();
    let accept = || {
        crate::teams::accept_invite(
            State(app.clone()),
            Session { account: "a2ada".into() },
            axum::extract::Path((team.to_owned(), code.to_owned())),
        )
    };
    assert_eq!(accept().await.unwrap().0["pending"], true);
    let n = phone.next().await.expect("a push for the request");
    assert_eq!(n["title"], "Ada Lovelace asks to join Acme");
    assert_eq!(app.db.requests(team).unwrap()[0].name, "Ada-Lovelace");
    assert_eq!(accept().await.unwrap().0["pending"], true);
    assert!(phone.quiet().await, "asking again doesn't push again");
    assert!(ada_phone.quiet().await);
}
