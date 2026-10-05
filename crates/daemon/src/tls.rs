//! TLS for block sites: one wildcard certificate, `*.<block domain>`.
//!
//! Either you hand the daemon the files (`--block-cert`, `--block-key`; it
//! picks up replaced files), or it gets and renews the certificate itself
//! from an ACME CA (Let's Encrypt) with a DNS-01 challenge through
//! Cloudflare's API. DNS-01 is the only challenge a CA accepts for a
//! wildcard, and it works without the CA reaching this host, which only the
//! tailnet can.
//!
//! The ACME state lives in one directory per CA (so a staging certificate
//! is never taken for a real one): `account.json`, `cert.pem`, `key.pem`.
//! A certificate is renewed two thirds of the way through its life.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::{Duration, SystemTime},
};

use anyhow::{Context, anyhow, bail};
use instant_acme::{
    Account, AccountCredentials, AuthorizationStatus, ChallengeType, Identifier, NewAccount, NewOrder, OrderStatus,
    RetryPolicy,
};
use serde_json::{Value, json};
use tokio_rustls::rustls::{
    self, ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
    server::{ClientHello, ResolvesServerCert},
    sign::CertifiedKey,
};
use tracing::{info, warn};

use crate::store::write_atomic;

pub const LETS_ENCRYPT: &str = "https://acme-v02.api.letsencrypt.org/directory";
pub const LETS_ENCRYPT_STAGING: &str = "https://acme-staging-v02.api.letsencrypt.org/directory";

/// The certificate being served, reloaded from its files when they change.
pub struct CertStore {
    cert: PathBuf,
    key: PathBuf,
    current: RwLock<Option<(SystemTime, Arc<CertifiedKey>)>>,
}

impl std::fmt::Debug for CertStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CertStore").field("cert", &self.cert).finish_non_exhaustive()
    }
}

impl CertStore {
    pub fn new(cert: PathBuf, key: PathBuf) -> Arc<Self> {
        Arc::new(Self { cert, key, current: RwLock::new(None) })
    }

    /// Load the pair if it changed since the last load.
    pub fn load(&self) -> anyhow::Result<()> {
        let mtime = std::fs::metadata(&self.cert)
            .and_then(|m| m.modified())
            .with_context(|| format!("{}", self.cert.display()))?;
        if self.current.read().unwrap().as_ref().is_some_and(|(t, _)| *t == mtime) {
            return Ok(());
        }
        let certs = CertificateDer::pem_file_iter(&self.cert)
            .and_then(|i| i.collect::<Result<Vec<_>, _>>())
            .map_err(|e| anyhow!("{}: {e}", self.cert.display()))?;
        if certs.is_empty() {
            bail!("{}: no certificate", self.cert.display());
        }
        let key = PrivateKeyDer::from_pem_file(&self.key).map_err(|e| anyhow!("{}: {e}", self.key.display()))?;
        let signer = rustls::crypto::aws_lc_rs::sign::any_supported_type(&key)?;
        *self.current.write().unwrap() = Some((mtime, Arc::new(CertifiedKey::new(certs, signer))));
        info!(cert = %self.cert.display(), "block certificate loaded");
        Ok(())
    }

    /// Pick up a pair something else renewed.
    pub async fn watch(self: Arc<Self>) {
        loop {
            tokio::time::sleep(Duration::from_secs(600)).await;
            if let Err(e) = self.load() {
                warn!(error = %e, "can't reload the block certificate");
            }
        }
    }
}

impl ResolvesServerCert for CertStore {
    fn resolve(&self, _hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        self.current.read().unwrap().as_ref().map(|(_, k)| k.clone())
    }
}

/// HTTP/1.1 only: WebSocket upgrades are plain there.
pub fn server_config(store: Arc<CertStore>) -> anyhow::Result<Arc<ServerConfig>> {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let mut config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_cert_resolver(store);
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

/// When a certificate should be replaced, if it's for `*.<domain>`.
fn renew_at(pem: &[u8], domain: &str) -> Option<SystemTime> {
    let (_, pem) = x509_parser::pem::parse_x509_pem(pem).ok()?;
    let cert = pem.parse_x509().ok()?;
    let wildcard = format!("*.{domain}");
    let names = cert.subject_alternative_name().ok()??;
    let covers =
        names.value.general_names.iter().any(
            |n| matches!(n, x509_parser::extensions::GeneralName::DNSName(d) if d.eq_ignore_ascii_case(&wildcard)),
        );
    if !covers {
        return None;
    }
    let (from, to) = (cert.validity().not_before.timestamp(), cert.validity().not_after.timestamp());
    let at = to - (to - from) / 3;
    Some(SystemTime::UNIX_EPOCH + Duration::from_secs(at.max(0) as u64))
}

/// Gets and renews `*.<domain>` from an ACME CA.
pub struct Acme {
    pub domain: String,
    pub email: Option<String>,
    pub directory: String,
    /// Where state is kept, one directory per CA beneath it.
    pub dir: PathBuf,
    pub dns: Cloudflare,
}

impl Acme {
    fn ca_dir(&self) -> PathBuf {
        let host = reqwest::Url::parse(&self.directory).ok().and_then(|u| u.host_str().map(str::to_owned));
        self.dir.join(host.unwrap_or_else(|| "ca".into()))
    }

    pub fn cert_file(&self) -> PathBuf {
        self.ca_dir().join("cert.pem")
    }

    pub fn key_file(&self) -> PathBuf {
        self.ca_dir().join("key.pem")
    }

    /// Whether the certificate on disk is good for a while yet.
    fn current(&self) -> bool {
        std::fs::read(self.cert_file())
            .ok()
            .and_then(|pem| renew_at(&pem, &self.domain))
            .is_some_and(|at| SystemTime::now() < at)
    }

    /// Get the certificate if there's none that's current, then keep it
    /// renewed. Serves what's on disk meanwhile.
    pub async fn run(self, store: Arc<CertStore>) {
        let _ = store.load();
        let mut backoff = Duration::from_secs(300);
        loop {
            let mut wait = Duration::from_secs(12 * 3600);
            if !self.current() {
                info!(name = %format!("*.{}", self.domain), ca = self.directory, "requesting block certificate");
                let got = tokio::time::timeout(Duration::from_secs(600), self.obtain()).await;
                match got.unwrap_or_else(|_| Err(anyhow!("timed out"))).and_then(|()| store.load()) {
                    Ok(()) => backoff = Duration::from_secs(300),
                    Err(e) => {
                        // CAs limit failed validations per hour: don't hammer.
                        warn!(error = %format!("{e:#}"), retry_in = ?backoff, "block certificate request failed");
                        wait = backoff;
                        backoff = (backoff * 2).min(Duration::from_secs(6 * 3600));
                    }
                }
            }
            tokio::time::sleep(wait).await;
        }
    }

    async fn account(&self) -> anyhow::Result<Account> {
        let dir = self.ca_dir();
        crate::store::private_dir(&dir)?;
        let path = dir.join("account.json");
        if let Ok(saved) = std::fs::read(&path) {
            let creds: AccountCredentials = serde_json::from_slice(&saved).context("account.json")?;
            return Ok(Account::builder()?.from_credentials(creds).await?);
        }
        let contact: Vec<String> = self.email.iter().map(|e| format!("mailto:{e}")).collect();
        let contact: Vec<&str> = contact.iter().map(String::as_str).collect();
        let new = NewAccount { contact: &contact, terms_of_service_agreed: true, only_return_existing: false };
        let (account, creds) = Account::builder()?.create(&new, self.directory.clone(), None).await?;
        write_atomic(&path, &serde_json::to_vec(&creds)?)?;
        Ok(account)
    }

    async fn obtain(&self) -> anyhow::Result<()> {
        let account = self.account().await?;
        let name = format!("*.{}", self.domain);
        let ids = [Identifier::Dns(name.clone())];
        let mut order = account.new_order(&NewOrder::new(&ids)).await?;
        let mut records = vec![];
        let mut auths = order.authorizations();
        let mut result = Ok(());
        while let Some(a) = auths.next().await {
            let mut a = a?;
            match a.status {
                AuthorizationStatus::Pending => {}
                AuthorizationStatus::Valid => continue,
                s => {
                    result = Err(anyhow!("authorization is {s:?}"));
                    break;
                }
            }
            let mut ch = a.challenge(ChallengeType::Dns01).ok_or_else(|| anyhow!("the CA offers no DNS-01"))?;
            let fqdn = format!("_acme-challenge.{}", self.domain);
            let value = ch.key_authorization().dns_value();
            match self.dns.present(&fqdn, &value).await {
                Ok(r) => records.push(r),
                Err(e) => {
                    result = Err(e);
                    break;
                }
            }
            if let Err(e) = self.dns.wait_for(&fqdn, &value).await {
                result = Err(e);
                break;
            }
            if let Err(e) = ch.set_ready().await {
                result = Err(e.into());
                break;
            }
        }
        let result = match result {
            Ok(()) => self.finish(&mut order).await,
            Err(e) => Err(e),
        };
        // Whatever happened, the challenge records go.
        for (zone, id) in records {
            if let Err(e) = self.dns.remove(&zone, &id).await {
                warn!(error = %e, "can't remove an ACME challenge record");
            }
        }
        result
    }

    async fn finish(&self, order: &mut instant_acme::Order) -> anyhow::Result<()> {
        let retry = RetryPolicy::new().timeout(Duration::from_secs(180));
        let status = order.poll_ready(&retry).await?;
        if status != OrderStatus::Ready {
            bail!("order is {status:?}");
        }
        let key = order.finalize().await?;
        let chain = order.poll_certificate(&retry).await?;
        // Key first: a new certificate must never meet an old key.
        write_atomic(&self.key_file(), key.as_bytes())?;
        write_atomic(&self.cert_file(), chain.as_bytes())?;
        info!(cert = %self.cert_file().display(), "block certificate issued");
        Ok(())
    }
}

/// Cloudflare's DNS API, for ACME challenge records. The token needs
/// Zone:Read and DNS:Edit on the zone, nothing else.
pub struct Cloudflare {
    token: String,
    api: String,
    /// DNS over HTTPS, to see the record before asking the CA to look.
    doh: String,
    http: reqwest::Client,
}

impl Cloudflare {
    pub fn new(token: String) -> Self {
        Self::with_api(token, "https://api.cloudflare.com/client/v4", "https://cloudflare-dns.com/dns-query")
    }

    fn with_api(token: String, api: &str, doh: &str) -> Self {
        let http = crate::roots::http().timeout(Duration::from_secs(30)).build().expect("http client");
        Self { token, api: api.trim_end_matches('/').to_owned(), doh: doh.to_owned(), http }
    }

    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
        let token = std::fs::read_to_string(path).with_context(|| format!("{}", path.display()))?;
        Ok(Self::new(token.trim().to_owned()))
    }

    async fn call(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> anyhow::Result<Value> {
        let mut req = self.http.request(method.clone(), format!("{}{path}", self.api)).bearer_auth(&self.token);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let res = req.send().await?;
        let status = res.status();
        let v: Value = res.json().await.map_err(|_| anyhow!("cloudflare {method} {path}: {status}"))?;
        if v["success"].as_bool() != Some(true) {
            let e = &v["errors"][0];
            bail!(
                "cloudflare {method} {path}: {} (code {})",
                e["message"].as_str().unwrap_or(status.as_str()),
                e["code"]
            );
        }
        Ok(v["result"].clone())
    }

    /// The zone holding `fqdn`, trying each parent in turn.
    async fn zone(&self, fqdn: &str) -> anyhow::Result<String> {
        let labels: Vec<&str> = fqdn.trim_end_matches('.').split('.').collect();
        for i in 0..labels.len().saturating_sub(1) {
            let name = labels[i..].join(".");
            let mut u = reqwest::Url::parse(&format!("{}/zones", self.api))?;
            u.query_pairs_mut().append_pair("name", &name);
            let zones = self.call(reqwest::Method::GET, &format!("/zones?{}", u.query().unwrap_or("")), None).await?;
            if let Some(id) = zones[0]["id"].as_str() {
                return Ok(id.to_owned());
            }
        }
        bail!("the Cloudflare token sees no zone for {fqdn}")
    }

    /// Publish a TXT record: (zone, record id), for `remove`.
    pub async fn present(&self, fqdn: &str, value: &str) -> anyhow::Result<(String, String)> {
        let zone = self.zone(fqdn).await?;
        let body = json!({
            "type": "TXT",
            "name": fqdn.trim_end_matches('.'),
            // Cloudflare wants TXT content quoted.
            "content": format!("\"{value}\""),
            "ttl": 60,
            "comment": "illogical ACME challenge; safe to delete",
        });
        let rec = self.call(reqwest::Method::POST, &format!("/zones/{zone}/dns_records"), Some(body)).await?;
        let id = rec["id"].as_str().ok_or_else(|| anyhow!("cloudflare: no record id"))?.to_owned();
        Ok((zone, id))
    }

    pub async fn remove(&self, zone: &str, id: &str) -> anyhow::Result<()> {
        self.call(reqwest::Method::DELETE, &format!("/zones/{zone}/dns_records/{id}"), None).await?;
        Ok(())
    }

    /// Until a public resolver sees the record (up to two minutes).
    async fn wait_for(&self, fqdn: &str, value: &str) -> anyhow::Result<()> {
        for _ in 0..40 {
            let mut u = reqwest::Url::parse(&self.doh)?;
            u.query_pairs_mut().append_pair("name", fqdn).append_pair("type", "TXT");
            let seen = match self.http.get(u).header("accept", "application/dns-json").send().await {
                Ok(r) => r.json::<Value>().await.ok().is_some_and(|v| {
                    v["Answer"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|a| a["data"].as_str().unwrap_or("").contains(value))
                }),
                Err(_) => false,
            };
            if seen {
                // Other resolvers (the CA's) may be a moment behind.
                tokio::time::sleep(Duration::from_secs(5)).await;
                return Ok(());
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
        bail!("the challenge record for {fqdn} never showed up in DNS")
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use axum::{
        Json, Router,
        extract::{Path as UrlPath, Query, State},
        http::HeaderMap,
        routing::{delete, get},
    };

    use super::*;

    /// A self-signed certificate for `names`, as (cert PEM, key PEM).
    fn self_signed(names: &[&str]) -> (String, String) {
        let names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
        let c = rcgen::generate_simple_self_signed(names).unwrap();
        (c.cert.pem(), c.signing_key.serialize_pem())
    }

    #[test]
    fn renewal_and_names() {
        let (cert, _) = self_signed(&["*.illogical.example.com"]);
        let at = renew_at(cert.as_bytes(), "illogical.example.com").unwrap();
        assert!(at > SystemTime::now(), "a new certificate isn't due yet");
        assert!(renew_at(cert.as_bytes(), "example.com").is_none(), "*.illogical.example.com isn't *.example.com");
        let (plain, _) = self_signed(&["illogical.example.com"]);
        assert!(renew_at(plain.as_bytes(), "illogical.example.com").is_none(), "not a wildcard");
        assert!(renew_at(b"junk", "illogical.example.com").is_none());
    }

    #[test]
    fn the_store_picks_up_new_files() {
        let dir = std::env::temp_dir().join(format!("ilg-tls-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (cert, key) = (dir.join("cert.pem"), dir.join("key.pem"));
        let store = CertStore::new(cert.clone(), key.clone());
        assert!(store.load().is_err(), "no files yet");
        assert!(store.current.read().unwrap().is_none());
        let (c, k) = self_signed(&["*.one.test"]);
        std::fs::write(&key, k).unwrap();
        std::fs::write(&cert, c).unwrap();
        store.load().unwrap();
        let first = store.current.read().unwrap().as_ref().unwrap().1.clone();
        // Unchanged: kept as is.
        store.load().unwrap();
        assert!(Arc::ptr_eq(&first, &store.current.read().unwrap().as_ref().unwrap().1));
        std::thread::sleep(Duration::from_millis(20));
        let (c, k) = self_signed(&["*.two.test"]);
        std::fs::write(&key, k).unwrap();
        std::fs::write(&cert, c).unwrap();
        store.load().unwrap();
        assert!(!Arc::ptr_eq(&first, &store.current.read().unwrap().as_ref().unwrap().1));
        assert!(server_config(store).is_ok());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[derive(Default)]
    struct FakeCloudflare {
        records: Mutex<Vec<(String, Value)>>,
        auth: Mutex<Vec<String>>,
    }

    #[tokio::test]
    async fn cloudflare_records_come_and_go() {
        type S = State<Arc<FakeCloudflare>>;
        let fake = Arc::new(FakeCloudflare::default());
        let app = Router::new()
            .route(
                "/zones",
                get(
                    |State(f): S, h: HeaderMap, Query(q): Query<std::collections::HashMap<String, String>>| async move {
                        f.auth.lock().unwrap().push(h["authorization"].to_str().unwrap().to_owned());
                        let zones = if q["name"] == "example.com" { json!([{"id": "z1"}]) } else { json!([]) };
                        Json(json!({"success": true, "result": zones}))
                    },
                ),
            )
            .route(
                "/zones/{zone}/dns_records",
                axum::routing::post(|State(f): S, UrlPath(zone): UrlPath<String>, Json(b): Json<Value>| async move {
                    f.records.lock().unwrap().push((zone, b));
                    Json(json!({"success": true, "result": {"id": "r1"}}))
                }),
            )
            .route(
                "/zones/{zone}/dns_records/{id}",
                delete(|State(f): S, UrlPath((zone, id)): UrlPath<(String, String)>| async move {
                    assert_eq!((zone.as_str(), id.as_str()), ("z1", "r1"));
                    f.records.lock().unwrap().clear();
                    Json(json!({"success": true, "result": {"id": "r1"}}))
                }),
            )
            .with_state(fake.clone());
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(axum::serve(l, app).into_future());

        let cf = Cloudflare::with_api("tok".into(), &api, "http://127.0.0.1:1/");
        let (zone, id) = cf.present("_acme-challenge.illogical.example.com", "v4lue").await.unwrap();
        assert_eq!((zone.as_str(), id.as_str()), ("z1", "r1"));
        {
            let recs = fake.records.lock().unwrap();
            assert_eq!(recs.len(), 1);
            assert_eq!(recs[0].1["name"], "_acme-challenge.illogical.example.com");
            assert_eq!(recs[0].1["content"], "\"v4lue\"");
            assert_eq!(recs[0].1["type"], "TXT");
        }
        assert!(fake.auth.lock().unwrap().iter().all(|a| a == "Bearer tok"));
        cf.remove(&zone, &id).await.unwrap();
        assert!(fake.records.lock().unwrap().is_empty());
        assert!(cf.present("_acme-challenge.other.test", "x").await.is_err(), "no zone for it");
    }
}
