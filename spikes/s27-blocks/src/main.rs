//! S27 spike: blocks through control, end to end encrypted (#148).
//!
//! ```text
//! s27 cert <dir>
//! s27 control --listen <addr> --dial-listen <addr> --dir <cert dir> --web <dist>
//!             [--origin https://control.test:7753] [--block-domain blocks.test] [--marker <text>]
//! s27 daemon --relay ws://<dial addr>/relay/dial --admin <addr> --direct <addr> --dir <cert dir>
//! ```
//!
//! Each prints one JSON line on stdout when it's listening (its addresses,
//! and the daemon's id and Noise key), for the test harness.

mod control;
mod daemon;
mod tls;
mod wire;

use std::{collections::HashMap, path::PathBuf};

use anyhow::Context;
use serde_json::json;
use tokio::net::TcpListener;

fn flags(args: &[String]) -> HashMap<String, String> {
    args.chunks(2).filter_map(|p| Some((p[0].strip_prefix("--")?.to_owned(), p.get(1)?.clone()))).collect()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((cmd, rest)) = args.split_first() else {
        anyhow::bail!("usage: s27 cert|control|daemon …");
    };
    let f = flags(rest);
    let get = |k: &str| f.get(k).cloned().with_context(|| format!("--{k}"));
    match cmd.as_str() {
        "cert" => tls::make(&PathBuf::from(rest.first().context("a directory")?)),
        "control" => {
            let dir = PathBuf::from(get("dir")?);
            let listener = TcpListener::bind(get("listen")?).await?;
            let dialer = TcpListener::bind(get("dial-listen")?).await?;
            let port = listener.local_addr()?.port();
            let origin = f.get("origin").cloned().unwrap_or(format!("https://control.test:{port}"));
            let c = control::Control::new(
                origin,
                f.get("block-domain").cloned().unwrap_or("blocks.test".into()),
                PathBuf::from(get("web")?),
                f.get("marker").cloned().unwrap_or_default(),
            );
            let acceptor = tls::acceptor(&dir.join("cert.pem"), &dir.join("key.pem"))?;
            println!(
                "{}",
                json!({ "listen": listener.local_addr()?.to_string(), "dial": dialer.local_addr()?.to_string() })
            );
            tokio::join!(
                tls::serve(listener, Some(acceptor), control::public(c.clone())),
                tls::serve(dialer, None, control::dial(c)),
            );
            Ok(())
        }
        "daemon" => {
            let dir = PathBuf::from(get("dir")?);
            let d = daemon::Daemon::new();
            let admin = TcpListener::bind(get("admin")?).await?;
            let direct = TcpListener::bind(get("direct")?).await?;
            let acceptor = tls::acceptor(&dir.join("cert.pem"), &dir.join("key.pem"))?;
            println!(
                "{}",
                json!({
                    "id": d.id(),
                    "noise": d.noise(),
                    "admin": admin.local_addr()?.to_string(),
                    "direct": direct.local_addr()?.to_string(),
                })
            );
            tokio::join!(
                daemon::dial(d.clone(), get("relay")?),
                tls::serve(admin, None, daemon::admin(d.clone())),
                tls::serve(direct, Some(acceptor), daemon::direct(d)),
            );
            Ok(())
        }
        c => anyhow::bail!("unknown command {c}"),
    }
}
