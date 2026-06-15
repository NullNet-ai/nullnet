use super::AppState;
use crate::cert::{self, CertificateAuthority, DnsProviderCredentials};
use crate::certs::CERTS_DIR;
use crate::events::Event;
use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize)]
struct CertJson {
    domain: String,
    /// Leaf `notAfter` as unix seconds (best-effort; `None` if unparseable).
    expires_at: Option<i64>,
}

#[derive(Serialize)]
struct ErrorJson {
    error: String,
}

fn bad_request(error: &str) -> axum::response::Response {
    (
        StatusCode::BAD_REQUEST,
        axum::Json(ErrorJson {
            error: error.to_string(),
        }),
    )
        .into_response()
}

/// List installed certs (domain + best-effort expiry). Never returns keys or
/// credentials.
pub(super) async fn list_handler() -> impl IntoResponse {
    let mut certs: Vec<CertJson> = Vec::new();
    for domain in crate::certs::cert_domains().await {
        let expires_at = crate::certs::read_expiry(&domain).await;
        certs.push(CertJson { domain, expires_at });
    }
    certs.sort_by(|a, b| a.domain.cmp(&b.domain));
    axum::Json(certs)
}

/// Write `fullchain.pem` + the encrypted key into `./certs/<domain>/` and emit an
/// install/renew event. The certs watcher propagates the write to the proxies,
/// which validate it and report any problem back as an event.
async fn persist_cert(
    state: &AppState,
    domain: &str,
    fullchain_pem: &str,
    key_pem: &str,
) -> axum::response::Response {
    let renewal = match crate::certs::write_cert(domain, fullchain_pem, key_pem).await {
        Ok(existed) => existed,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(ErrorJson {
                    error: "failed to write certificate".to_string(),
                }),
            )
                .into_response();
        }
    };
    let event = if renewal {
        Event::certificate_renewed(domain.to_string())
    } else {
        Event::certificate_installed(domain.to_string())
    };
    state.events.emit(event).await;
    StatusCode::NO_CONTENT.into_response()
}

/// Default seconds to wait for a TXT record to propagate before asking the CA to
/// validate the DNS-01 challenge.
const DEFAULT_DNS_PROPAGATION_SECS: u64 = 30;

#[derive(Deserialize)]
pub(super) struct RequestReq {
    domain: String,
    credentials: DnsProviderCredentials,
    dns_propagation_secs: Option<u64>,
}

/// Issue a cert from Let's Encrypt via a DNS-01 challenge. On success the cert is
/// written with its key encrypted at rest, and the DNS-provider credentials are
/// stored encrypted alongside it so the cert can be auto-renewed unattended. The
/// watcher pushes the new cert to the proxies.
pub(super) async fn request_handler(
    State(state): State<AppState>,
    axum::Json(req): axum::Json<RequestReq>,
) -> impl IntoResponse {
    let Some(domain) = sanitize_domain(&req.domain) else {
        return bad_request("invalid domain");
    };
    // keep a copy to persist for auto-renew; `create_dns_provider` consumes it
    let credentials = req.credentials.clone();
    let provider_name = req.credentials.provider_name();
    println!("ACME request for '{domain}' via DNS provider '{provider_name}'");
    let provider = match cert::create_dns_provider(req.credentials) {
        Ok(p) => p,
        Err(e) => return bad_request(&format!("invalid DNS provider credentials: {e}")),
    };

    // staging CA in debug builds, production in release (matches ../routix)
    let ca = if cfg!(debug_assertions) {
        CertificateAuthority::staging()
    } else {
        CertificateAuthority::production()
    };
    let propagation = req
        .dns_propagation_secs
        .unwrap_or(DEFAULT_DNS_PROPAGATION_SECS);

    match ca
        .request_certificate(&domain, provider.as_ref(), propagation)
        .await
    {
        Ok((fullchain_pem, key_pem)) => {
            let resp = persist_cert(&state, &domain, &fullchain_pem, &key_pem).await;
            // store creds for auto-renew, but only if the cert actually persisted
            // (otherwise we'd orphan a creds file with no matching cert)
            if resp.status() == StatusCode::NO_CONTENT {
                match serde_json::to_string(&credentials) {
                    Ok(json) => {
                        if let Err(e) = crate::certs::store_dns_credentials(&domain, &json).await {
                            eprintln!("Failed to store DNS credentials for '{domain}': {e:?}");
                        }
                    }
                    Err(e) => {
                        eprintln!("Failed to serialize DNS credentials for '{domain}': {e:?}")
                    }
                }
            }
            resp
        }
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            axum::Json(ErrorJson {
                error: format!("ACME issuance failed: {e:#}"),
            }),
        )
            .into_response(),
    }
}

/// Remove a cert. The change is pushed to the proxies and hot-reloaded like any
/// other; removing the last cert clears the set everywhere.
pub(super) async fn delete_handler(
    State(state): State<AppState>,
    AxumPath(domain): AxumPath<String>,
) -> impl IntoResponse {
    let Some(domain) = sanitize_domain(&domain) else {
        return StatusCode::BAD_REQUEST;
    };
    let dir = PathBuf::from(CERTS_DIR).join(&domain);
    match tokio::fs::remove_dir_all(&dir).await {
        Ok(()) => {
            state.events.emit(Event::certificate_removed(domain)).await;
            StatusCode::NO_CONTENT
        }
        Err(_) => StatusCode::NOT_FOUND,
    }
}

/// Validate a domain as a safe directory name: exact (`color.com`) or single-level
/// wildcard (`*.color.com`). Rejects path separators, `..`, and stray characters,
/// so it is safe to join onto `CERTS_DIR`.
fn sanitize_domain(input: &str) -> Option<String> {
    let d = input.trim();
    if d.is_empty() || d.len() > 253 {
        return None;
    }
    let labels = d.strip_prefix("*.").unwrap_or(d);
    if labels.is_empty() {
        return None;
    }
    let ok = labels.split('.').all(|l| {
        !l.is_empty()
            && l.len() <= 63
            && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            && !l.starts_with('-')
            && !l.ends_with('-')
    });
    ok.then(|| d.to_string())
}

#[cfg(test)]
mod tests {
    use super::sanitize_domain;

    #[test]
    fn accepts_exact_and_wildcard() {
        assert_eq!(sanitize_domain("color.com").as_deref(), Some("color.com"));
        assert_eq!(
            sanitize_domain("*.color.com").as_deref(),
            Some("*.color.com")
        );
        assert_eq!(
            sanitize_domain(" a-b.example.io ").as_deref(),
            Some("a-b.example.io")
        );
    }

    #[test]
    fn rejects_traversal_and_junk() {
        for bad in [
            "",
            "..",
            "../etc",
            "a/b",
            "a/../b",
            "color..com",
            "*.*.com",
            "-bad.com",
            "bad-.com",
            "a b.com",
            "color.com/",
        ] {
            assert!(sanitize_domain(bad).is_none(), "should reject {bad:?}");
        }
    }
}
