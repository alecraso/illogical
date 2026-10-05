//! Who the daemon trusts for outgoing TLS. The platform's roots first: they
//! carry a company's own CA. When the machine has none (a bare server or
//! container without `ca-certificates`, where install.sh drops the static
//! binary) the platform verifier won't start, so the bundled Mozilla roots
//! (`webpki-root-certs`) stand in, with one warning. Every HTTP client the
//! daemon builds starts from [`http`].

use std::sync::{Arc, LazyLock};

use tokio_rustls::rustls::{self, client::danger::ServerCertVerifier, crypto::CryptoProvider};
use tracing::warn;

/// Whether this machine has no usable system roots. Asked once, on first use.
static MISSING: LazyLock<bool> = LazyLock::new(|| {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    match rustls_platform_verifier::Verifier::new(provider) {
        Ok(_) => false,
        Err(e) => {
            warn!(error = %e, "no system CA certificates: trusting the bundled Mozilla roots (install ca-certificates to use the system's)");
            true
        }
    }
});

/// The bundled roots, as reqwest takes them.
static BUNDLED: LazyLock<Vec<reqwest::Certificate>> = LazyLock::new(|| {
    webpki_root_certs::TLS_SERVER_ROOT_CERTS.iter().filter_map(|c| reqwest::Certificate::from_der(c).ok()).collect()
});

/// A reqwest client builder that verifies with the platform's roots, or the
/// bundled ones when there are none. Use it instead of
/// `reqwest::Client::builder()` / `Client::new()`, which fail (or panic) on
/// such a machine.
pub fn http() -> reqwest::ClientBuilder {
    with(*MISSING)
}

fn with(missing: bool) -> reqwest::ClientBuilder {
    let b = reqwest::Client::builder();
    if missing { b.tls_certs_only(BUNDLED.iter().cloned()) } else { b }
}

/// `http().build()`, for the places that took `Client::new()`. With the
/// bundled roots to fall back on, building no longer depends on the machine.
pub fn client() -> reqwest::Client {
    http().build().expect("an HTTP client")
}

/// A rustls verifier on the same terms, for raw TLS connections.
pub fn verifier(provider: Arc<CryptoProvider>) -> anyhow::Result<Arc<dyn ServerCertVerifier>> {
    if !*MISSING {
        return Ok(Arc::new(rustls_platform_verifier::Verifier::new(provider)?));
    }
    let mut roots = rustls::RootCertStore::empty();
    roots.add_parsable_certificates(webpki_root_certs::TLS_SERVER_ROOT_CERTS.iter().cloned());
    Ok(rustls::client::WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider).build()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_roots_make_a_client() {
        assert!(BUNDLED.len() > 100, "{} roots", BUNDLED.len());
        with(true).build().unwrap();
    }
}
