use arc_swap::ArcSwap;
use async_trait::async_trait;
use nullnet_grpc_lib::nullnet_grpc::CertBundle;
use nullnet_liberror::{Error, ErrorHandler, Location, location};
use openssl::asn1::Asn1Time;
use openssl::nid::Nid;
use openssl::ssl::NameType;
use pingora_core::listeners::TlsAccept;
use pingora_core::protocols::tls::TlsRef;
use pingora_openssl::ext;
use pingora_openssl::pkey::{PKey, Private};
use pingora_openssl::ssl::{SslContextBuilder, SslMethod};
use pingora_openssl::x509::{X509, X509Ref};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::Arc;

/// A parsed TLS certificate: leaf + intermediate chain + matching private key.
pub struct Certificate {
    leaf: X509,
    chain: Vec<X509>,
    private_key: PKey<Private>,
}

impl Certificate {
    /// Validate a cert/key pair for `domain`. Every failure is logged via
    /// `handle_err`; the returned `Error`'s message doubles as the short reason
    /// surfaced to the operator as an event (`Error::to_str`).
    fn new(domain: &str, cert_pem: &str, key_pem: &str) -> Result<Self, Error> {
        let mut certs = X509::stack_from_pem(cert_pem.as_bytes())
            .map_err(|e| format!("invalid certificate PEM: {e}"))
            .handle_err(location!())?;
        if certs.is_empty() {
            return Err::<Self, _>("no certificate found in PEM").handle_err(location!());
        }
        let leaf = certs.remove(0);
        let chain = certs;
        let private_key = PKey::private_key_from_pem(key_pem.as_bytes())
            .map_err(|e| format!("invalid private key PEM: {e}"))
            .handle_err(location!())?;

        // ensure the private key actually matches the leaf certificate
        let mut builder = SslContextBuilder::new(SslMethod::tls()).handle_err(location!())?;
        builder.set_certificate(&leaf).handle_err(location!())?;
        builder
            .set_private_key(&private_key)
            .map_err(|_| "private key does not match certificate")
            .handle_err(location!())?;
        builder
            .check_private_key()
            .map_err(|_| "private key does not match certificate")
            .handle_err(location!())?;

        // reject expired / not-yet-valid leaf certificates
        let now = Asn1Time::days_from_now(0).handle_err(location!())?;
        if leaf.not_after().compare(&now).handle_err(location!())? == Ordering::Less {
            return Err::<Self, _>("certificate has expired").handle_err(location!());
        }
        if leaf.not_before().compare(&now).handle_err(location!())? == Ordering::Greater {
            return Err::<Self, _>("certificate is not yet valid").handle_err(location!());
        }

        // ensure the cert actually covers the domain it is filed under
        if !cert_covers_domain(&leaf, domain) {
            return Err::<Self, _>(format!("certificate does not cover domain '{domain}'"))
                .handle_err(location!());
        }

        Ok(Self {
            leaf,
            chain,
            private_key,
        })
    }
}

/// Whether `leaf`'s SAN dNSNames (or CN, as fallback) cover `domain`, using
/// standard single-label wildcard rules. `domain` is the SNI key, exact
/// (`color.com`) or wildcard (`*.color.com`).
fn cert_covers_domain(leaf: &X509Ref, domain: &str) -> bool {
    if let Some(sans) = leaf.subject_alt_names()
        && sans
            .iter()
            .filter_map(|n| n.dnsname())
            .any(|dns| name_matches(dns, domain))
    {
        return true;
    }
    leaf.subject_name()
        .entries_by_nid(Nid::COMMONNAME)
        .next()
        .and_then(|e| e.data().as_utf8().ok())
        .is_some_and(|cn| name_matches(&cn, domain))
}

/// Match a cert name (`san`) against a target `domain`: exact (case-insensitive)
/// or a `*.`-prefixed wildcard covering exactly one label.
fn name_matches(san: &str, domain: &str) -> bool {
    if san.eq_ignore_ascii_case(domain) {
        return true;
    }
    if let (Some(suffix), Some((_, parent))) = (san.strip_prefix("*."), domain.split_once('.')) {
        return parent.eq_ignore_ascii_case(suffix);
    }
    false
}

/// In-memory certificate store keyed by domain (SNI). Rebuilt wholesale from a
/// `CertBundle` pushed by the control service and swapped in atomically.
///
/// Keys are SNI names: exact (`color.com`) or wildcard (`*.color.com`).
#[derive(Default)]
pub struct CertStore {
    certs: HashMap<String, Arc<Certificate>>,
}

impl CertStore {
    /// Build a store from a bundle received over gRPC, validating each
    /// certificate/key pair. Returns the store plus the `(domain, reason)` of
    /// every certificate that failed validation and was skipped.
    pub fn from_bundle(bundle: &CertBundle) -> (Self, Vec<(String, String)>) {
        let mut certs = HashMap::new();
        let mut failures = Vec::new();
        for c in &bundle.certificates {
            match Certificate::new(&c.domain, &c.fullchain_pem, &c.key_pem) {
                Ok(cert) => {
                    certs.insert(c.domain.clone(), Arc::new(cert));
                }
                Err(e) => failures.push((c.domain.clone(), e.to_str().to_string())),
            }
        }
        (Self { certs }, failures)
    }

    /// Number of valid certificates held.
    pub fn len(&self) -> usize {
        self.certs.len()
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
/// Reads the live `ArcSwap` so hot-reloaded certs are picked up immediately.
pub struct TlsResolver {
    store: Arc<ArcSwap<CertStore>>,
}

impl TlsResolver {
    pub fn new(store: Arc<ArcSwap<CertStore>>) -> Self {
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
        let store = self.store.load();
        let Some(cert) = store.get(hostname) else {
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

#[cfg(test)]
mod tests {
    use super::*;
    use nullnet_grpc_lib::nullnet_grpc::TlsCertificate;
    use openssl::hash::MessageDigest;
    use openssl::rsa::Rsa;
    use openssl::x509::extension::SubjectAlternativeName;
    use openssl::x509::{X509Builder, X509NameBuilder};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn now_unix() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    /// Self-signed cert (PEM) + matching PKCS#8 key (PEM) for the given names
    /// and validity window (unix seconds).
    fn gen_cert(cn: &str, sans: &[&str], not_before: i64, not_after: i64) -> (String, String) {
        let pkey = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();

        let mut name = X509NameBuilder::new().unwrap();
        name.append_entry_by_nid(Nid::COMMONNAME, cn).unwrap();
        let name = name.build();

        let mut b = X509Builder::new().unwrap();
        b.set_version(2).unwrap();
        b.set_subject_name(&name).unwrap();
        b.set_issuer_name(&name).unwrap();
        b.set_pubkey(&pkey).unwrap();
        b.set_not_before(&Asn1Time::from_unix(not_before).unwrap())
            .unwrap();
        b.set_not_after(&Asn1Time::from_unix(not_after).unwrap())
            .unwrap();
        if !sans.is_empty() {
            let mut san = SubjectAlternativeName::new();
            for s in sans {
                san.dns(s);
            }
            let ext = san.build(&b.x509v3_context(None, None)).unwrap();
            b.append_extension(ext).unwrap();
        }
        b.sign(&pkey, MessageDigest::sha256()).unwrap();
        let cert = b.build();

        (
            String::from_utf8(cert.to_pem().unwrap()).unwrap(),
            String::from_utf8(pkey.private_key_to_pem_pkcs8().unwrap()).unwrap(),
        )
    }

    fn valid(cn: &str, sans: &[&str]) -> (String, String) {
        let now = now_unix();
        gen_cert(cn, sans, now - 3600, now + 3600)
    }

    #[test]
    fn name_matches_exact_and_case_insensitive() {
        assert!(name_matches("color.com", "color.com"));
        assert!(name_matches("Color.COM", "color.com"));
        assert!(!name_matches("other.com", "color.com"));
    }

    #[test]
    fn name_matches_wildcard_single_label_only() {
        assert!(name_matches("*.color.com", "app.color.com"));
        assert!(name_matches("*.color.com", "*.color.com"));
        // wildcard does not cover the apex...
        assert!(!name_matches("*.color.com", "color.com"));
        // ...nor more than one label
        assert!(!name_matches("*.color.com", "a.b.color.com"));
    }

    #[test]
    fn accepts_valid_exact_cert() {
        let (cert, key) = valid("color.com", &["color.com"]);
        assert!(Certificate::new("color.com", &cert, &key).is_ok());
    }

    #[test]
    fn wildcard_cert_covers_subdomain_but_not_apex() {
        let (cert, key) = valid("*.color.com", &["*.color.com"]);
        // filed for a subdomain -> covered by the wildcard SAN
        assert!(Certificate::new("www.color.com", &cert, &key).is_ok());
        // filed for the apex -> wildcard does not cover it
        let err = Certificate::new("color.com", &cert, &key).err().unwrap();
        assert!(err.to_str().contains("does not cover"), "{}", err.to_str());
    }

    #[test]
    fn rejects_domain_mismatch() {
        let (cert, key) = valid("color.com", &["color.com"]);
        let err = Certificate::new("other.com", &cert, &key).err().unwrap();
        assert!(err.to_str().contains("does not cover"), "{}", err.to_str());
    }

    #[test]
    fn rejects_expired_cert() {
        let now = now_unix();
        let (cert, key) = gen_cert("color.com", &["color.com"], now - 7200, now - 3600);
        let err = Certificate::new("color.com", &cert, &key).err().unwrap();
        assert!(err.to_str().contains("expired"), "{}", err.to_str());
    }

    #[test]
    fn rejects_not_yet_valid_cert() {
        let now = now_unix();
        let (cert, key) = gen_cert("color.com", &["color.com"], now + 3600, now + 7200);
        let err = Certificate::new("color.com", &cert, &key).err().unwrap();
        assert!(err.to_str().contains("not yet valid"), "{}", err.to_str());
    }

    #[test]
    fn rejects_key_mismatch() {
        let (cert, _) = valid("color.com", &["color.com"]);
        let (_, other_key) = valid("color.com", &["color.com"]);
        let err = Certificate::new("color.com", &cert, &other_key)
            .err()
            .unwrap();
        assert!(err.to_str().contains("does not match"), "{}", err.to_str());
    }

    #[test]
    fn from_bundle_splits_valid_and_invalid() {
        let (ok_cert, ok_key) = valid("good.com", &["good.com"]);
        let (bad_cert, bad_key) = valid("good.com", &["good.com"]); // SAN won't cover "bad.com"
        let bundle = CertBundle {
            certificates: vec![
                TlsCertificate {
                    domain: "good.com".to_string(),
                    fullchain_pem: ok_cert,
                    key_pem: ok_key,
                },
                TlsCertificate {
                    domain: "bad.com".to_string(),
                    fullchain_pem: bad_cert,
                    key_pem: bad_key,
                },
            ],
        };
        let (store, failures) = CertStore::from_bundle(&bundle);
        assert_eq!(store.len(), 1);
        assert!(store.has_cert("good.com"));
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].0, "bad.com");
    }

    #[test]
    fn store_get_resolves_wildcard() {
        let (cert, key) = valid("*.color.com", &["*.color.com"]);
        let bundle = CertBundle {
            certificates: vec![TlsCertificate {
                domain: "*.color.com".to_string(),
                fullchain_pem: cert,
                key_pem: key,
            }],
        };
        let (store, failures) = CertStore::from_bundle(&bundle);
        assert!(failures.is_empty());
        assert!(store.has_cert("app.color.com")); // wildcard parent match
        assert!(!store.has_cert("color.com")); // apex not covered
        assert!(!store.has_cert("a.b.color.com")); // multi-level not covered
    }
}
