//! The test certificate, and serving a router over TLS.
//!
//! One self-signed certificate covers every name the spike uses:
//! `control.test` (control's page), `*.blocks.test` (control's block
//! origins: a registrable domain of its own, so a block is cross-site to
//! control's page), `daemon.test` (the daemon's direct Noise endpoint) and
//! `*.direct.test` (the daemon's own block sites, today's path, for the
//! latency baseline). Browsers reach them through the harness's CONNECT
//! proxy, which sends every name to 127.0.0.1; Chromium trusts the
//! certificate by its key hash (`--ignore-certificate-errors-spki-list`),
//! WebKit by `ignoreHTTPSErrors`.

use std::{path::Path, sync::Arc};

use axum::Router;
use base64::Engine;
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn::auto,
    service::TowerToHyperService,
};
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tokio_rustls::{
    TlsAcceptor,
    rustls::{
        ServerConfig,
        pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
    },
};

pub const NAMES: &[&str] = &["control.test", "*.blocks.test", "daemon.test", "*.direct.test"];

/// Write `cert.pem`, `key.pem` and `spki.txt` (the base64 SHA-256 of the
/// key, for Chromium's flag) to `dir`.
pub fn make(dir: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir)?;
    let c = rcgen::generate_simple_self_signed(NAMES.iter().map(|s| s.to_string()).collect::<Vec<_>>())?;
    std::fs::write(dir.join("cert.pem"), c.cert.pem())?;
    std::fs::write(dir.join("key.pem"), c.signing_key.serialize_pem())?;
    let spki = Sha256::digest(rcgen::PublicKeyData::subject_public_key_info(&c.signing_key));
    std::fs::write(dir.join("spki.txt"), base64::engine::general_purpose::STANDARD.encode(spki))?;
    Ok(())
}

pub fn acceptor(cert: &Path, key: &Path) -> anyhow::Result<TlsAcceptor> {
    let certs = CertificateDer::pem_file_iter(cert)?.collect::<Result<Vec<_>, _>>()?;
    let key = PrivateKeyDer::from_pem_file(key)?;
    let provider = Arc::new(tokio_rustls::rustls::crypto::aws_lc_rs::default_provider());
    let mut config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// Serve `router` on `listener`, over TLS when `tls` is given.
pub async fn serve(listener: TcpListener, tls: Option<TlsAcceptor>, router: Router) {
    loop {
        let Ok((tcp, _)) = listener.accept().await else { continue };
        let _ = tcp.set_nodelay(true);
        let (tls, router) = (tls.clone(), router.clone());
        tokio::spawn(async move {
            let svc = TowerToHyperService::new(router);
            let http = auto::Builder::new(TokioExecutor::new());
            let _ = match tls {
                Some(a) => match a.accept(tcp).await {
                    Ok(s) => http.serve_connection_with_upgrades(TokioIo::new(s), svc).await,
                    Err(_) => return,
                },
                None => http.serve_connection_with_upgrades(TokioIo::new(tcp), svc).await,
            };
        });
    }
}
