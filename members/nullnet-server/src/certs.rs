use crate::crypto;
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use nullnet_grpc_lib::nullnet_grpc::{CertBundle, TlsCertificate};
use nullnet_liberror::{Error, ErrorHandler, Location, location};
use std::ops::Sub;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::mpsc as tokio_mpsc;
use tokio::sync::watch;
use tokio::time::Instant;

pub(crate) const CERTS_DIR: &str = "./certs";
/// Leaf + intermediate chain, PEM, plaintext.
pub(crate) const FULLCHAIN: &str = "fullchain.pem";
/// Encrypted private key (AES-256-GCM, at rest). Preferred.
pub(crate) const KEY_ENCRYPTED: &str = "privkey.enc";
/// Legacy/plaintext private key (manual file-drop); migrated to encrypted on load.
pub(crate) const KEY_PLAINTEXT: &str = "privkey.pem";
/// Encrypted DNS-provider credentials (JSON) for ACME-issued certs. Present only
/// when the cert can be auto-renewed; absent for manual uploads.
pub(crate) const DNS_CREDS_ENCRYPTED: &str = "dns_credentials.enc";

/// Read every certificate from disk into a `CertBundle`.
///
/// Layout: `./certs/<domain>/fullchain.pem` + an encrypted `privkey.enc` (or a
/// legacy plaintext `privkey.pem`, migrated on first read). `<domain>` is the SNI
/// key (exact `color.com` or wildcard `*.color.com`). The bundle carries the
/// decrypted key; the proxy validates the PEMs.
pub(crate) async fn load_certificates() -> CertBundle {
    let _ = tokio::fs::create_dir_all(CERTS_DIR).await;

    let mut certificates = Vec::new();
    let Ok(mut entries) = tokio::fs::read_dir(CERTS_DIR).await else {
        return CertBundle { certificates };
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
        let Ok(fullchain_pem) = tokio::fs::read_to_string(path.join(FULLCHAIN)).await else {
            continue;
        };
        let Some(key_pem) = load_or_migrate_key(&path).await else {
            continue;
        };
        routine_log!("Loaded TLS certificate for '{domain}'");
        certificates.push(TlsCertificate {
            domain,
            fullchain_pem,
            key_pem,
        });
    }

    CertBundle { certificates }
}

/// Return the decrypted private key for a cert dir. Prefers the encrypted
/// `privkey.enc`; if only a legacy plaintext `privkey.pem` exists, encrypt it in
/// place (write `privkey.enc`, remove the plaintext) before returning it.
async fn load_or_migrate_key(dir: &Path) -> Option<String> {
    let enc_path = dir.join(KEY_ENCRYPTED);
    if let Ok(encoded) = tokio::fs::read_to_string(&enc_path).await {
        return crypto::cipher().decrypt(&encoded).ok();
    }

    // legacy / freshly dropped plaintext key: migrate to encrypted at rest
    let key_pem = tokio::fs::read_to_string(dir.join(KEY_PLAINTEXT))
        .await
        .ok()?;
    if let Ok(encoded) = crypto::cipher().encrypt(&key_pem)
        && tokio::fs::write(&enc_path, encoded).await.is_ok()
    {
        let _ = tokio::fs::remove_file(dir.join(KEY_PLAINTEXT)).await;
        eprintln!("Encrypted private key at rest for '{}'", dir.display());
    }
    Some(key_pem)
}

/// Encrypt the key and write `fullchain.pem` + `privkey.enc` into
/// `./certs/<domain>/`, dropping any stale plaintext key. Returns whether the
/// domain dir already existed (i.e. this was a renewal/replacement). The certs
/// watcher picks up the write and pushes the new bundle to the proxies.
pub(crate) async fn write_cert(
    domain: &str,
    fullchain_pem: &str,
    key_pem: &str,
) -> Result<bool, Error> {
    let encoded = crypto::cipher().encrypt(key_pem)?;
    let dir = PathBuf::from(CERTS_DIR).join(domain);
    let existed = tokio::fs::try_exists(&dir).await.unwrap_or(false);
    tokio::fs::create_dir_all(&dir)
        .await
        .handle_err(location!())?;
    tokio::fs::write(dir.join(FULLCHAIN), fullchain_pem)
        .await
        .handle_err(location!())?;
    tokio::fs::write(dir.join(KEY_ENCRYPTED), &encoded)
        .await
        .handle_err(location!())?;
    // drop any stale plaintext key from a previous file-drop
    let _ = tokio::fs::remove_file(dir.join(KEY_PLAINTEXT)).await;
    Ok(existed)
}

/// Store DNS-provider credentials (`creds_json`) encrypted at rest so the cert
/// can be auto-renewed without re-supplying the token. Call after `write_cert`.
pub(crate) async fn store_dns_credentials(domain: &str, creds_json: &str) -> Result<(), Error> {
    let encoded = crypto::cipher().encrypt(creds_json)?;
    let dir = PathBuf::from(CERTS_DIR).join(domain);
    tokio::fs::create_dir_all(&dir)
        .await
        .handle_err(location!())?;
    tokio::fs::write(dir.join(DNS_CREDS_ENCRYPTED), encoded)
        .await
        .handle_err(location!())
}

/// Load + decrypt the stored DNS-provider credentials JSON for `domain`, if any.
pub(crate) async fn load_dns_credentials(domain: &str) -> Option<String> {
    let path = PathBuf::from(CERTS_DIR)
        .join(domain)
        .join(DNS_CREDS_ENCRYPTED);
    let encoded = tokio::fs::read_to_string(&path).await.ok()?;
    crypto::cipher().decrypt(&encoded).ok()
}

/// Best-effort leaf `notAfter` (unix seconds) for a domain's `fullchain.pem`.
pub(crate) async fn read_expiry(domain: &str) -> Option<i64> {
    let path = PathBuf::from(CERTS_DIR).join(domain).join(FULLCHAIN);
    let bytes = tokio::fs::read(&path).await.ok()?;
    let (_, pem) = x509_parser::pem::parse_x509_pem(&bytes).ok()?;
    let cert = pem.parse_x509().ok()?;
    Some(cert.validity().not_after.timestamp())
}

/// Domain dirs currently present under `./certs` (the SNI keys).
pub(crate) async fn cert_domains() -> Vec<String> {
    let mut domains = Vec::new();
    let Ok(mut entries) = tokio::fs::read_dir(CERTS_DIR).await else {
        return domains;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.is_dir()
            && let Some(name) = path.file_name().and_then(|s| s.to_str())
        {
            domains.push(name.to_string());
        }
    }
    domains
}

/// Watch `./certs` and push a fresh `CertBundle` through `certs_tx` on every
/// change, so subscribed proxies hot-reload. Mirrors the services watcher.
pub(crate) async fn watch(certs_tx: watch::Sender<CertBundle>) -> Result<(), Error> {
    let dir = PathBuf::from(CERTS_DIR);
    let _ = tokio::fs::create_dir_all(&dir).await;

    let (tx, mut rx) = tokio_mpsc::unbounded_channel();
    let mut watcher = RecommendedWatcher::new(
        move |event| {
            let _ = tx.send(event);
        },
        Config::default(),
    )
    .handle_err(location!())?;
    watcher
        .watch(&dir, RecursiveMode::Recursive)
        .handle_err(location!())?;

    let mut last_update_time = Instant::now().sub(Duration::from_mins(1));

    loop {
        let event = rx.recv().await;
        if event.is_none() {
            eprintln!("Certs file watcher channel closed, stopping watch");
            break;
        }
        if let Some(Ok(Event { kind, .. })) = event
            && matches!(
                kind,
                EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
            )
            // debounce duplicated events
            && last_update_time.elapsed().as_millis() > 100
        {
            // ensure file changes are fully flushed before reading
            tokio::time::sleep(Duration::from_millis(100)).await;
            let _ = certs_tx.send(load_certificates().await);
            last_update_time = Instant::now();
        }
    }

    Ok(())
}
