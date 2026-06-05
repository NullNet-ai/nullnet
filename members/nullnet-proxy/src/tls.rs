use async_trait::async_trait;
use nullnet_liberror::{Error, ErrorHandler, Location, location};
use openssl::ssl::NameType;
use pingora_core::listeners::TlsAccept;
use pingora_core::protocols::tls::TlsRef;
use pingora_openssl::ext;
use pingora_openssl::pkey::{PKey, Private};
use pingora_openssl::ssl::{SslContextBuilder, SslMethod};
use pingora_openssl::x509::X509;
use std::collections::HashMap;
use std::sync::Arc;

/// A parsed TLS certificate: leaf + intermediate chain + matching private key.
pub struct Certificate {
    leaf: X509,
    chain: Vec<X509>,
    private_key: PKey<Private>,
}

impl Certificate {
    fn new(cert_pem: &str, key_pem: &str) -> Result<Self, Error> {
        let mut certs = X509::stack_from_pem(cert_pem.as_bytes()).handle_err(location!())?;
        if certs.is_empty() {
            Err::<(), _>("no certificate found in PEM").handle_err(location!())?;
        }
        let leaf = certs.remove(0);
        let chain = certs;
        let private_key = PKey::private_key_from_pem(key_pem.as_bytes()).handle_err(location!())?;

        // ensure the private key actually matches the leaf certificate
        let mut builder = SslContextBuilder::new(SslMethod::tls()).handle_err(location!())?;
        builder.set_certificate(&leaf).handle_err(location!())?;
        builder
            .set_private_key(&private_key)
            .handle_err(location!())?;
        builder.check_private_key().handle_err(location!())?;

        Ok(Self {
            leaf,
            chain,
            private_key,
        })
    }
}

/// In-memory certificate store keyed by domain, loaded once from disk at startup.
///
/// Expected layout: `<dir>/<domain>/fullchain.pem` + `<dir>/<domain>/privkey.pem`.
/// A wildcard cert lives under a directory named `*.example.com`.
pub struct CertStore {
    certs: HashMap<String, Arc<Certificate>>,
}

impl CertStore {
    pub fn load(dir: &str) -> Self {
        let mut certs = HashMap::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            println!("Could not read certs dir '{dir}'; starting with no TLS certificates");
            return Self { certs };
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let Some(domain) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let (Ok(cert_pem), Ok(key_pem)) = (
                std::fs::read_to_string(path.join("fullchain.pem")),
                std::fs::read_to_string(path.join("privkey.pem")),
            ) else {
                continue;
            };
            match Certificate::new(&cert_pem, &key_pem) {
                Ok(cert) => {
                    println!("Loaded TLS certificate for '{domain}'");
                    certs.insert(domain.to_string(), Arc::new(cert));
                }
                Err(_) => println!("Skipping '{domain}': invalid certificate or key"),
            }
        }

        Self { certs }
    }

    /// Resolve a cert for an SNI hostname: exact match first, then wildcard
    /// (`app.example.com` -> `*.example.com`).
    fn get(&self, hostname: &str) -> Option<Arc<Certificate>> {
        if let Some(cert) = self.certs.get(hostname) {
            return Some(cert.clone());
        }
        let (_, parent) = hostname.split_once('.')?;
        self.certs.get(&format!("*.{parent}")).cloned()
    }

    /// Whether a cert (exact or wildcard) is available for the given hostname.
    pub fn has_cert(&self, hostname: &str) -> bool {
        self.get(hostname).is_some()
    }
}

/// SNI-based certificate resolver invoked by pingora during the TLS handshake.
pub struct TlsResolver {
    store: Arc<CertStore>,
}

impl TlsResolver {
    pub fn new(store: Arc<CertStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl TlsAccept for TlsResolver {
    async fn certificate_callback(&self, ssl: &mut TlsRef) {
        let Some(hostname) = ssl.servername(NameType::HOST_NAME) else {
            println!("TLS handshake without SNI; no certificate selected");
            return;
        };
        let Some(cert) = self.store.get(hostname) else {
            println!("No TLS certificate found for '{hostname}'");
            return;
        };

        let _ = ext::ssl_use_certificate(ssl, &cert.leaf);
        let _ = ext::ssl_use_private_key(ssl, &cert.private_key);
        for intermediate in &cert.chain {
            let _ = ext::ssl_add_chain_cert(ssl, intermediate);
        }
    }
}
