//! TLS identity for the gRPC control channel itself (distinct from the
//! customer-facing certs in `certs.rs`, which get streamed to proxies via
//! `WatchCertificates`). The server generates its own private CA on first
//! boot and signs its leaf with it; clients pin the stable CA root
//! (`ca-cert.pem`) via `CONTROL_SERVICE_CA_CERT` for full standard chain
//! validation, including hostname matching — only a leaf actually signed by
//! that CA, for the right host, is accepted. The CA is never regenerated
//! once created (pinned clients trust it, not the leaf), so future leaf
//! rotation needs no client-side changes. Because hostname matching
//! applies, the leaf's SAN must cover whatever host/IP clients use as
//! `CONTROL_SERVICE_ADDR` — set via `CONTROL_SERVICE_TLS_SAN` (see
//! `leaf_sans_from_env`).
use nullnet_liberror::{Error, ErrorHandler, Location, location};
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, Issuer, KeyPair,
};
use std::path::{Path, PathBuf};

pub(crate) const GRPC_TLS_DIR: &str = "./grpc-tls";
const CERT_FILE: &str = "cert.pem";
const KEY_FILE: &str = "key.pem";
const CA_CERT_FILE: &str = "ca-cert.pem";
const CA_KEY_FILE: &str = "ca-key.pem";

/// Load the persisted cert/key, generating and persisting a CA-signed one
/// on first boot (see module docs).
pub(crate) async fn load_or_generate() -> Result<(String, String), Error> {
    let dir = PathBuf::from(GRPC_TLS_DIR);
    let cert_path = dir.join(CERT_FILE);
    let key_path = dir.join(KEY_FILE);

    if let (Ok(cert_pem), Ok(key_pem)) = (
        tokio::fs::read_to_string(&cert_path).await,
        tokio::fs::read_to_string(&key_path).await,
    ) {
        println!("Loaded gRPC control channel TLS certificate from '{GRPC_TLS_DIR}'");
        return Ok((cert_pem, key_pem));
    }

    tokio::fs::create_dir_all(&dir)
        .await
        .handle_err(location!())?;

    let (cert_pem, key_pem) = generate_ca_signed_leaf(&dir).await?;

    tokio::fs::write(&cert_path, &cert_pem)
        .await
        .handle_err(location!())?;
    tokio::fs::write(&key_path, &key_pem)
        .await
        .handle_err(location!())?;
    restrict_key_permissions(&key_path).await?;

    Ok((cert_pem, key_pem))
}

/// The CA's distinguished name must differ from the leaf's, or a validator
/// can't tell "signed by the CA" apart from "signed by itself" when the
/// issuer and subject fields are compared by name (both `openssl verify`
/// and rustls's WebPKI path-building do this) — an identical default name
/// on both was an earlier bug here, misreported as "self-signed certificate".
fn ca_distinguished_name() -> DistinguishedName {
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, "nullnet control-plane CA");
    dn
}

fn leaf_distinguished_name() -> DistinguishedName {
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, "nullnet control channel");
    dn
}

/// Comma-separated hostnames/IPs from `CONTROL_SERVICE_TLS_SAN`. Clients'
/// `PinnedCa` verifier does full standard validation, including hostname
/// matching — this must include whatever host/IP clients use as
/// `CONTROL_SERVICE_ADDR`, or their handshake fails with a hostname
/// mismatch, not a trust error. SANs are baked in when the leaf is signed;
/// changing this env var takes effect on the next leaf regeneration (i.e.
/// after deleting `cert.pem`/`key.pem` — the CA itself is untouched).
///
/// Falls back to `CONTROL_SERVICE_ADDR` if unset — this server's own
/// `.env` often already sets that to the address clients are told to
/// connect to, which is exactly the SAN a self-consistent deployment needs
/// — and only then to `localhost`.
fn leaf_sans_from_env() -> Vec<String> {
    if let Ok(val) = std::env::var("CONTROL_SERVICE_TLS_SAN") {
        let sans: Vec<String> = val
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        if !sans.is_empty() {
            return sans;
        }
    }

    if let Ok(addr) = std::env::var("CONTROL_SERVICE_ADDR") {
        let addr = addr.trim();
        if !addr.is_empty() {
            println!(
                "'CONTROL_SERVICE_TLS_SAN' environment variable not set, falling back to \
                 'CONTROL_SERVICE_ADDR' ('{addr}') for the leaf cert's SAN"
            );
            return vec![addr.to_string()];
        }
    }

    println!(
        "Neither 'CONTROL_SERVICE_TLS_SAN' nor 'CONTROL_SERVICE_ADDR' environment variables are \
         set, defaulting the leaf cert's SAN to 'localhost' — set one of them to the host/IP \
         clients use to reach this server, or their handshake will fail hostname verification"
    );
    vec!["localhost".to_string()]
}

/// Load the persisted CA (generating it on first use), then sign a fresh
/// leaf cert with it. The CA cert itself is never regenerated once it
/// exists on disk — only its private key is reloaded, to sign a new leaf.
///
/// Reconstructing `CertificateParams` here (rather than parsing the
/// existing `ca-cert.pem`, which rcgen has no public API for) is safe
/// because every field `Issuer::from_params` reads off it — distinguished
/// name, key identifier method, key usages — is set identically both here
/// and when the CA was first created, so they always match what's actually
/// embedded in the persisted CA cert.
async fn generate_ca_signed_leaf(dir: &Path) -> Result<(String, String), Error> {
    let ca_cert_path = dir.join(CA_CERT_FILE);
    let ca_key_path = dir.join(CA_KEY_FILE);

    let ca_key = if let Ok(ca_key_pem) = tokio::fs::read_to_string(&ca_key_path).await {
        println!("Loaded gRPC control channel CA from '{GRPC_TLS_DIR}'");
        KeyPair::from_pem(&ca_key_pem).handle_err(location!())?
    } else {
        println!("Generating gRPC control channel private CA...");
        let mut ca_params = CertificateParams::new(vec![]).handle_err(location!())?;
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.distinguished_name = ca_distinguished_name();
        let ca_key = KeyPair::generate().handle_err(location!())?;
        let ca_cert = ca_params.self_signed(&ca_key).handle_err(location!())?;

        tokio::fs::write(&ca_cert_path, ca_cert.pem())
            .await
            .handle_err(location!())?;
        tokio::fs::write(&ca_key_path, ca_key.serialize_pem())
            .await
            .handle_err(location!())?;
        restrict_key_permissions(&ca_key_path).await?;
        println!(
            "Generated gRPC control channel CA at '{}' — copy it to every client/proxy \
             host and point CONTROL_SERVICE_CA_CERT at it",
            ca_cert_path.display()
        );
        ca_key
    };

    println!("Signing gRPC control channel leaf certificate...");
    let mut ca_params = CertificateParams::new(vec![]).handle_err(location!())?;
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.distinguished_name = ca_distinguished_name();
    let issuer = Issuer::from_params(&ca_params, &ca_key);

    let mut leaf_params = CertificateParams::new(leaf_sans_from_env()).handle_err(location!())?;
    leaf_params.distinguished_name = leaf_distinguished_name();
    let leaf_key = KeyPair::generate().handle_err(location!())?;
    let leaf_cert = leaf_params
        .signed_by(&leaf_key, &issuer)
        .handle_err(location!())?;

    Ok((leaf_cert.pem(), leaf_key.serialize_pem()))
}

#[cfg(unix)]
async fn restrict_key_permissions(key_path: &Path) -> Result<(), Error> {
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::set_permissions(key_path, std::fs::Permissions::from_mode(0o600))
        .await
        .handle_err(location!())
}

#[cfg(not(unix))]
async fn restrict_key_permissions(_key_path: &Path) -> Result<(), Error> {
    Ok(())
}
