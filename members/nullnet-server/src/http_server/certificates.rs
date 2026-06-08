use super::AppState;
use crate::certs::{CERTS_DIR, KEY_ENCRYPTED, KEY_PLAINTEXT};
use crate::crypto;
use crate::events::Event;
use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

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

#[derive(Deserialize)]
pub(super) struct UploadReq {
    domain: String,
    fullchain_pem: String,
    key_pem: String,
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

/// List installed certs (domain + best-effort expiry). Never returns keys.
pub(super) async fn list_handler() -> impl IntoResponse {
    let mut certs: Vec<CertJson> = Vec::new();
    let Ok(mut entries) = tokio::fs::read_dir(CERTS_DIR).await else {
        return axum::Json(certs);
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(domain) = path
            .file_name()
            .and_then(|s| s.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        let expires_at = read_expiry(&path.join("fullchain.pem")).await;
        certs.push(CertJson { domain, expires_at });
    }
    certs.sort_by(|a, b| a.domain.cmp(&b.domain));
    axum::Json(certs)
}

/// Ingest or replace (renew) a cert: writes the cert plaintext + the key
/// encrypted at rest. The certs watcher then pushes it to the proxies, which
/// validate it and report any problem back as an event.
pub(super) async fn upload_handler(
    State(state): State<AppState>,
    axum::Json(req): axum::Json<UploadReq>,
) -> impl IntoResponse {
    let Some(domain) = sanitize_domain(&req.domain) else {
        return bad_request("invalid domain");
    };
    if !req.fullchain_pem.contains("BEGIN CERTIFICATE") {
        return bad_request("fullchain_pem is not a PEM certificate");
    }
    if !req.key_pem.contains("PRIVATE KEY") {
        return bad_request("key_pem is not a PEM private key");
    }
    let Ok(encoded) = crypto::cipher().encrypt(&req.key_pem) else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(ErrorJson {
                error: "failed to encrypt private key".to_string(),
            }),
        )
            .into_response();
    };

    let dir = PathBuf::from(CERTS_DIR).join(&domain);
    let renewal = tokio::fs::try_exists(&dir).await.unwrap_or(false);
    if tokio::fs::create_dir_all(&dir).await.is_err()
        || tokio::fs::write(dir.join("fullchain.pem"), &req.fullchain_pem)
            .await
            .is_err()
        || tokio::fs::write(dir.join(KEY_ENCRYPTED), &encoded)
            .await
            .is_err()
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(ErrorJson {
                error: "failed to write certificate".to_string(),
            }),
        )
            .into_response();
    }
    // drop any stale plaintext key from a previous file-drop
    let _ = tokio::fs::remove_file(dir.join(KEY_PLAINTEXT)).await;
    let event = if renewal {
        Event::certificate_renewed(domain)
    } else {
        Event::certificate_installed(domain)
    };
    state.events.emit(event).await;
    StatusCode::NO_CONTENT.into_response()
}

/// Remove a cert. Note: clearing the *last* cert won't fully propagate until the
/// proxies restart (they keep the last-known-good set to avoid going dark).
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

async fn read_expiry(fullchain: &Path) -> Option<i64> {
    let bytes = tokio::fs::read(fullchain).await.ok()?;
    let (_, pem) = x509_parser::pem::parse_x509_pem(&bytes).ok()?;
    let cert = pem.parse_x509().ok()?;
    Some(cert.validity().not_after.timestamp())
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
