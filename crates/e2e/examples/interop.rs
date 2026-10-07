//! Rust's half of the browser interop check (`web/e2e-interop.ts`):
//!
//! - `fixtures`: certificates, a revocation and what Rust makes of them, as
//!   JSON, for the TS code to evaluate the same way;
//! - `check`: read a certificate chain the TS code signed from stdin, and
//!   say which devices Rust trusts;
//! - `roster`: read `{pin, prev, roster, certs}` from stdin and say whether
//!   the roster follows (a team's next version, a presigned invite's too);
//! - `responder ADDR`: a WebSocket daemon stand-in that answers the Noise
//!   handshake and echoes text and binary messages, and answers requests
//!   with their method, path and body.

use std::io::Read;

use arugula_e2e::{
    Cert, DeviceKeys, Kind, Revocation, Trust,
    cert::join_code,
    channel::{Msg, Responder, ResponseHead, prologue},
    team::{AccountCerts, Roster, TeamPin},
};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

fn signed(keys: &DeviceKeys, by: &DeviceKeys, account: &str, kind: Kind, name: &str, created: u64) -> Cert {
    let mut c = Cert::new(keys, account, kind, name);
    c.created = created;
    c.sign_with(by);
    c
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("fixtures") => {
            let (r, p, d, f) =
                (DeviceKeys::generate(), DeviceKeys::generate(), DeviceKeys::generate(), DeviceKeys::generate());
            let root = signed(&r, &r, "acct", Kind::Browser, "laptop ✓", 1);
            let phone = signed(&p, &r, "acct", Kind::Browser, "phone", 2);
            let daemon = signed(&d, &p, "acct", Kind::Daemon, "geek", 3);
            let late = signed(&DeviceKeys::generate(), &p, "acct", Kind::Daemon, "late", 50);
            let fake = signed(&f, &f, "acct", Kind::Browser, "control's", 4);
            let mut rev = Revocation::new("acct", &phone.device, &r);
            rev.at = 10;
            rev.sig = hex::encode(r.signature(rev.body().as_bytes()));
            let certs = vec![root.clone(), phone, daemon.clone(), late, fake];
            let trust = Trust { account: "acct".into(), root: root.device.clone() };
            let mut trusted: Vec<String> =
                trust.evaluate(&certs, std::slice::from_ref(&rev)).devices.into_keys().collect();
            trusted.sort();
            println!(
                "{}",
                serde_json::json!({
                    "trust": trust, "certs": certs, "revocations": [rev],
                    "trusted": trusted, "joinCode": join_code(&daemon), "daemon": daemon,
                })
            );
        }
        Some("check") => {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s)?;
            let v: serde_json::Value = serde_json::from_str(&s)?;
            let trust: Trust = serde_json::from_value(v["trust"].clone())?;
            let certs: Vec<Cert> = serde_json::from_value(v["certs"].clone())?;
            let mut ids: Vec<String> = trust.evaluate(&certs, &[]).devices.into_keys().collect();
            ids.sort();
            println!("{}", serde_json::to_string(&ids)?);
        }
        Some("roster") => {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s)?;
            let v: serde_json::Value = serde_json::from_str(&s)?;
            let pin: TeamPin = serde_json::from_value(v["pin"].clone())?;
            let prev: Option<Roster> = serde_json::from_value(v["prev"].clone())?;
            // One that doesn't even parse doesn't follow.
            let roster: Option<Roster> = serde_json::from_value(v["roster"].clone()).ok();
            let certs: AccountCerts = serde_json::from_value(v["certs"].clone())?;
            println!("{}", roster.is_some_and(|r| r.follows(prev.as_ref(), &pin, &certs)));
        }
        Some("responder") => {
            let keys = DeviceKeys::generate();
            let l = tokio::net::TcpListener::bind(args.get(1).map(String::as_str).unwrap_or("127.0.0.1:0")).await?;
            println!("{} {} {}", l.local_addr()?, keys.id(), hex::encode(keys.noise_public));
            loop {
                let (sock, _) = l.accept().await?;
                let keys = DeviceKeys {
                    noise_private: keys.noise_private,
                    noise_public: keys.noise_public,
                    sign: keys.sign.clone(),
                };
                tokio::spawn(async move {
                    let mut ws = tokio_tungstenite::accept_async(sock).await?;
                    let Some(Ok(Message::Binary(m1))) = ws.next().await else { anyhow::bail!("no handshake") };
                    let (r, _who) = Responder::read(&keys, &prologue(&keys.id()), &m1)?;
                    let (m2, ch) = r.finish(&[])?;
                    ws.send(Message::Binary(m2.into())).await?;
                    while let Some(Ok(Message::Binary(w))) = ws.next().await {
                        let Some(m) = ch.open(&w)? else { continue };
                        let reply = match m {
                            Msg::Request { id, head, body } => Msg::Response {
                                id,
                                head: ResponseHead {
                                    status: 201,
                                    content_type: Some("application/json".into()),
                                    more: false,
                                },
                                body: serde_json::to_vec(&serde_json::json!({
                                    "method": head.method, "path": head.path, "len": body.len(),
                                }))?,
                            },
                            other => other,
                        };
                        for w in ch.seal(&reply)? {
                            ws.send(Message::Binary(w.into())).await?;
                        }
                    }
                    anyhow::Ok(())
                });
            }
        }
        _ => anyhow::bail!("usage: interop fixtures|check|roster|responder [ADDR]"),
    }
    Ok(())
}
