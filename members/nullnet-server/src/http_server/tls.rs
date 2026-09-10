use crate::events::{Event, EventStore};
use axum_server::tls_rustls::RustlsConfig;
use nullnet_grpc_lib::certificate_names::name_matches;
use nullnet_grpc_lib::nullnet_grpc::{CertBundle, TlsCertificate};
use tokio::sync::watch;
use x509_parser::extensions::GeneralName;

pub(super) async fn configure(
    certificates: watch::Receiver<CertBundle>,
    events: EventStore,
) -> RustlsConfig {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
        .expect("failed to generate self-signed certificate");
    let config = RustlsConfig::from_pem(
        cert.cert.pem().into_bytes(),
        cert.signing_key.serialize_pem().into_bytes(),
    )
    .await
    .expect("failed to build TLS config");
    if let Some(domain) = crate::env::UI_TLS_DOMAIN.as_ref() {
        let mut ui = ManagedTls {
            domain: domain.clone(),
            config: config.clone(),
            active: None,
            failure: None,
        };
        let mut certificates = certificates;
        let bundle = certificates.borrow_and_update().clone();
        if let Some(event) = ui.update(&bundle).await {
            events.emit(event).await;
        }
        tokio::spawn(async move {
            while certificates.changed().await.is_ok() {
                let bundle = certificates.borrow_and_update().clone();
                if let Some(event) = ui.update(&bundle).await {
                    events.emit(event).await;
                }
            }
        });
    }
    config
}

struct ManagedTls {
    domain: String,
    config: RustlsConfig,
    active: Option<TlsCertificate>,
    failure: Option<String>,
}

impl ManagedTls {
    async fn update(&mut self, bundle: &CertBundle) -> Option<Event> {
        let result = match select(bundle, &self.domain) {
            Some(cert) if self.active.as_ref() == Some(cert) => Ok(cert),
            Some(cert) => match validate(cert, &self.domain) {
                Ok(()) => self
                    .config
                    .reload_from_pem(
                        cert.fullchain_pem.as_bytes().to_vec(),
                        cert.key_pem.as_bytes().to_vec(),
                    )
                    .await
                    .map(|()| cert)
                    .map_err(|e| format!("invalid UI TLS certificate/key: {e}")),
                Err(reason) => Err(reason),
            },
            None => Err("no matching certificate available in the certificate store".to_string()),
        };
        match result {
            Ok(cert) => {
                let recovered = self.failure.take().is_some();
                if self.active.as_ref() == Some(cert) && !recovered {
                    return None;
                }
                self.active = Some(cert.clone());
                println!("UI TLS certificate active for '{}'", self.domain);
                Some(Event::ui_tls_certificate_active(self.domain.clone()))
            }
            Err(reason) => {
                if self.failure.as_ref() == Some(&reason) {
                    return None;
                }
                eprintln!(
                    "UI TLS certificate unavailable for '{}': {reason}",
                    self.domain
                );
                self.failure = Some(reason.clone());
                Some(Event::ui_tls_certificate_unavailable(
                    self.domain.clone(),
                    reason,
                    self.active.is_none(),
                ))
            }
        }
    }
}

fn select<'a>(bundle: &'a CertBundle, domain: &str) -> Option<&'a TlsCertificate> {
    bundle
        .certificates
        .iter()
        .find(|cert| cert.domain.eq_ignore_ascii_case(domain))
        .or_else(|| {
            bundle
                .certificates
                .iter()
                .find(|cert| name_matches(&cert.domain, domain))
        })
}

fn validate(cert: &TlsCertificate, domain: &str) -> Result<(), String> {
    let (_, pem) = x509_parser::pem::parse_x509_pem(cert.fullchain_pem.as_bytes())
        .map_err(|_| "invalid certificate PEM")?;
    let leaf = pem.parse_x509().map_err(|_| "invalid X.509 certificate")?;
    if !leaf.validity().is_valid() {
        return Err("certificate is expired or not yet valid".to_string());
    }
    let covers_domain =
        leaf.subject_alternative_name()
            .map_err(|_| "invalid certificate subject alternative names")?
            .is_some_and(|san| {
                san.value.general_names.iter().any(
                    |name| matches!(name, GeneralName::DNSName(dns) if name_matches(dns, domain)),
                )
            });
    if !covers_domain {
        return Err(format!("certificate does not cover UI hostname '{domain}'"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn certificate(domain: &str) -> TlsCertificate {
        let cert = rcgen::generate_simple_self_signed(vec![domain.to_string()]).unwrap();
        TlsCertificate {
            domain: domain.to_string(),
            fullchain_pem: cert.cert.pem(),
            key_pem: cert.signing_key.serialize_pem(),
        }
    }

    #[test]
    fn selection_prefers_exact_and_limits_wildcard_to_one_label() {
        let wildcard = certificate("*.example.com");
        let exact = certificate("ui.example.com");
        let bundle = CertBundle {
            certificates: vec![wildcard.clone(), exact.clone()],
        };
        assert_eq!(select(&bundle, "UI.EXAMPLE.COM"), Some(&exact));
        assert_eq!(select(&bundle, "other.example.com"), Some(&wildcard));
        assert!(select(&bundle, "example.com").is_none());
        assert!(select(&bundle, "nested.ui.example.com").is_none());
    }

    #[test]
    fn validates_actual_san_and_dates_not_store_label() {
        let mut cert = certificate("*.example.com");
        assert!(validate(&cert, "ui.example.com").is_ok());
        assert!(validate(&cert, "example.com").is_err());
        cert.domain = "unrelated.test".to_string();
        assert!(validate(&cert, "unrelated.test").is_err());
        let mut params = rcgen::CertificateParams::new(vec!["ui.example.com".to_string()]).unwrap();
        params.not_before = rcgen::date_time_ymd(2000, 1, 1);
        params.not_after = rcgen::date_time_ymd(2001, 1, 1);
        let key = rcgen::KeyPair::generate().unwrap();
        cert.fullchain_pem = params.self_signed(&key).unwrap().pem();
        assert!(validate(&cert, "ui.example.com").is_err());
        params.not_before = rcgen::date_time_ymd(2998, 1, 1);
        params.not_after = rcgen::date_time_ymd(2999, 1, 1);
        cert.fullchain_pem = params.self_signed(&key).unwrap().pem();
        assert!(validate(&cert, "ui.example.com").is_err());
    }

    #[tokio::test]
    async fn bootstrap_renewal_failure_retention_and_recovery() {
        let fallback = certificate("localhost");
        let config = RustlsConfig::from_pem(
            fallback.fullchain_pem.into_bytes(),
            fallback.key_pem.into_bytes(),
        )
        .await
        .unwrap();
        let mut ui = ManagedTls {
            domain: "ui.example.com".to_string(),
            config,
            active: None,
            failure: None,
        };
        let empty = CertBundle::default();
        assert!(matches!(
            ui.update(&empty).await,
            Some(Event::UiTlsCertificateUnavailable {
                using_self_signed: true,
                ..
            })
        ));
        assert!(ui.update(&empty).await.is_none());
        let mut bundle = CertBundle {
            certificates: vec![certificate("*.example.com")],
        };
        assert!(matches!(
            ui.update(&bundle).await,
            Some(Event::UiTlsCertificateActive { .. })
        ));
        let first = ui.config.get_inner();
        assert!(ui.update(&bundle).await.is_none());
        assert!(Arc::ptr_eq(&first, &ui.config.get_inner()));
        bundle.certificates[0] = certificate("*.example.com");
        assert!(matches!(
            ui.update(&bundle).await,
            Some(Event::UiTlsCertificateActive { .. })
        ));
        assert!(!Arc::ptr_eq(&first, &ui.config.get_inner()));
        let renewed = ui.config.get_inner();
        let good = bundle.clone();
        bundle.certificates[0].key_pem = certificate("*.example.com").key_pem;
        assert!(matches!(
            ui.update(&bundle).await,
            Some(Event::UiTlsCertificateUnavailable {
                using_self_signed: false,
                ..
            })
        ));
        assert!(Arc::ptr_eq(&renewed, &ui.config.get_inner()));
        assert!(ui.update(&bundle).await.is_none());
        assert!(matches!(
            ui.update(&empty).await,
            Some(Event::UiTlsCertificateUnavailable {
                using_self_signed: false,
                ..
            })
        ));
        assert!(Arc::ptr_eq(&renewed, &ui.config.get_inner()));
        assert!(matches!(
            ui.update(&good).await,
            Some(Event::UiTlsCertificateActive { .. })
        ));
        assert!(Arc::ptr_eq(&renewed, &ui.config.get_inner()));
    }
}
