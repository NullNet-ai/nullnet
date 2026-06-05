use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use nullnet_grpc_lib::nullnet_grpc::{CertBundle, TlsCertificate};
use nullnet_liberror::{Error, ErrorHandler, Location, location};
use std::ops::Sub;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::mpsc as tokio_mpsc;
use tokio::sync::watch;
use tokio::time::Instant;

const CERTS_DIR: &str = "./certs";

/// Read every certificate from disk into a `CertBundle`.
///
/// Layout: `./certs/<domain>/fullchain.pem` + `./certs/<domain>/privkey.pem`,
/// where `<domain>` is the SNI key (exact `color.com` or wildcard `*.color.com`).
/// Unreadable/incomplete entries are skipped; the proxy validates the PEMs.
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
        // TODO(encryption-at-rest): privkey.pem is read in plaintext here. Once
        // encryption-at-rest lands, decrypt the key before adding it to the
        // bundle, and ensure the gRPC channel is TLS-protected (see gRPC TLS
        // TODO in main.rs) since the bundle carries private keys.
        let (Ok(fullchain_pem), Ok(key_pem)) = (
            tokio::fs::read_to_string(path.join("fullchain.pem")).await,
            tokio::fs::read_to_string(path.join("privkey.pem")).await,
        ) else {
            continue;
        };
        println!("Loaded TLS certificate for '{domain}'");
        certificates.push(TlsCertificate {
            domain,
            fullchain_pem,
            key_pem,
        });
    }

    CertBundle { certificates }
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
            println!("Certs file watcher channel closed, stopping watch");
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
