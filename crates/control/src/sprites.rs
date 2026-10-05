//! A small Sprites API client (M20): what hosted sandboxes need from the
//! provider. Create one, put the daemon in it and run it as a service,
//! open a connection to its port (which wakes it), delete it. The same API
//! as the daemon's provider (`crates/daemon/src/provider/sprites.rs`); wisp
//! speaks it too, which is how the tests run.

use std::io;

use futures_util::{SinkExt, StreamExt};
use reqwest::{StatusCode, Url};
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};

pub struct Sprites {
    base: Url,
    token: String,
    http: reqwest::Client,
}

#[derive(Deserialize)]
struct Entry {
    #[serde(default)]
    status: String,
}

impl Sprites {
    pub fn new(base: &str, token: String) -> anyhow::Result<Self> {
        Ok(Self { base: Url::parse(base)?, token, http: crate::roots::client(std::time::Duration::from_secs(300))? })
    }

    fn url(&self, path: &str) -> Url {
        let mut u = self.base.clone();
        u.set_path(&format!("/v1/sprites{path}"));
        u
    }

    /// Its state (`running`, `warm`, `cold`), or `None` if there's none.
    pub async fn status(&self, name: &str) -> anyhow::Result<Option<String>> {
        let r = self.http.get(self.url(&format!("/{name}"))).bearer_auth(&self.token).send().await?;
        match r.status() {
            s if s.is_success() => Ok(Some(r.json::<Entry>().await?.status)),
            StatusCode::NOT_FOUND => Ok(None),
            s => anyhow::bail!("looking up {name}: {s}"),
        }
    }

    pub async fn create(&self, name: &str) -> anyhow::Result<()> {
        let r = self
            .http
            .post(self.url(""))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({ "name": name }))
            .send()
            .await?;
        let status = r.status();
        let text = r.text().await.unwrap_or_default();
        match status {
            s if s.is_success() => Ok(()),
            StatusCode::BAD_REQUEST | StatusCode::CONFLICT if text.contains("name_taken") => Ok(()),
            s => anyhow::bail!("creating {name}: {s} {}", text.trim()),
        }
    }

    pub async fn delete(&self, name: &str) -> anyhow::Result<()> {
        let r = self.http.delete(self.url(&format!("/{name}"))).bearer_auth(&self.token).send().await?;
        match r.status() {
            s if s.is_success() || s == StatusCode::NOT_FOUND => Ok(()),
            s => anyhow::bail!("deleting {name}: {s}"),
        }
    }

    /// Write a file (relative paths are from the sandbox user's home).
    pub async fn write_file(&self, name: &str, path: &str, data: Vec<u8>, mode: u32) -> anyhow::Result<()> {
        let mut u = self.url(&format!("/{name}/fs/write"));
        u.query_pairs_mut()
            .append_pair("path", path)
            .append_pair("mode", &format!("{mode:o}"))
            .append_pair("mkdirParents", "true");
        let r = self.http.put(u).bearer_auth(&self.token).body(data).send().await?;
        let status = r.status();
        if !status.is_success() {
            anyhow::bail!("writing {path} in {name}: {status} {}", r.text().await.unwrap_or_default().trim());
        }
        Ok(())
    }

    /// A small file's contents, or `None` if it isn't there (yet).
    pub async fn read_file(&self, name: &str, path: &str) -> anyhow::Result<Option<Vec<u8>>> {
        let mut u = self.url(&format!("/{name}/fs/read"));
        u.query_pairs_mut().append_pair("path", path);
        let r = self.http.get(u).bearer_auth(&self.token).send().await?;
        if !r.status().is_success() {
            return Ok(None);
        }
        Ok(Some(r.bytes().await?.to_vec()))
    }

    /// Define (or replace) a service, which starts now, on every boot, and
    /// again when it exits.
    pub async fn put_service(&self, name: &str, service: &str, cmd: &str, args: &[String]) -> anyhow::Result<()> {
        let mut u = self.url(&format!("/{name}/services/{service}"));
        u.query_pairs_mut().append_pair("duration", "2s");
        let body = serde_json::json!({ "cmd": cmd, "args": args, "env": {} });
        let r = self.http.put(u).bearer_auth(&self.token).json(&body).send().await?;
        let status = r.status();
        let text = r.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("starting service {service} in {name}: {status} {}", text.trim());
        }
        for line in text.lines() {
            let v: serde_json::Value = serde_json::from_str(line).unwrap_or_default();
            if v["type"] == "error" {
                anyhow::bail!("service {service} in {name}: {}", v["data"].as_str().unwrap_or("failed"));
            }
        }
        Ok(())
    }

    /// A connection to `port` inside it, through the provider's proxy (which
    /// wakes it).
    pub async fn dial(&self, name: &str, port: u16) -> io::Result<DuplexStream> {
        let mut u = self.url(&format!("/{name}/proxy"));
        let scheme = if u.scheme() == "https" { "wss" } else { "ws" };
        let _ = u.set_scheme(scheme);
        let mut req = u.as_str().into_client_request().map_err(io::Error::other)?;
        req.headers_mut().insert("Authorization", format!("Bearer {}", self.token).parse().map_err(io::Error::other)?);
        let (mut ws, _) =
            tokio_tungstenite::connect_async(req).await.map_err(|e| io::Error::other(format!("Sprites proxy: {e}")))?;
        ws.send(Message::Text(serde_json::json!({ "host": "localhost", "port": port }).to_string().into()))
            .await
            .map_err(io::Error::other)?;
        loop {
            match ws.next().await {
                Some(Ok(Message::Text(t))) => {
                    let v: serde_json::Value = serde_json::from_str(&t).unwrap_or_default();
                    if v["status"] == "connected" {
                        break;
                    }
                    let why = v["error"].as_str().unwrap_or("refused").to_owned();
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionRefused,
                        format!("nothing on port {port}: {why}"),
                    ));
                }
                Some(Ok(Message::Close(_))) | None => {
                    return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "proxy closed"));
                }
                Some(Err(e)) => return Err(io::Error::other(e)),
                Some(Ok(_)) => {}
            }
        }
        let (ours, theirs) = tokio::io::duplex(64 * 1024);
        tokio::spawn(async move {
            let (mut from_us, mut to_us) = tokio::io::split(theirs);
            let (mut sink, mut stream) = ws.split();
            let up = async {
                let mut buf = vec![0u8; 32 * 1024];
                while let Ok(n) = from_us.read(&mut buf).await {
                    if n == 0 || sink.send(Message::Binary(buf[..n].to_vec().into())).await.is_err() {
                        break;
                    }
                }
                let _ = sink.close().await;
            };
            let down = async {
                while let Some(Ok(m)) = stream.next().await {
                    match m {
                        Message::Binary(b) => {
                            if to_us.write_all(&b).await.is_err() {
                                break;
                            }
                        }
                        Message::Close(_) => break,
                        _ => {}
                    }
                }
                let _ = to_us.shutdown().await;
            };
            tokio::join!(up, down);
        });
        Ok(ours)
    }
}
