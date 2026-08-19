mod auth;
mod cert;
mod cert_renewal;
mod certs;
mod crypto;
mod db;
mod env;
mod events;
mod events_retention;
mod geo;
mod graphviz;
mod grpc_tls;
mod http_server;
mod net;
mod net_id_pool;
mod nullnet_grpc_impl;
mod orchestrator;
mod services;
#[cfg(test)]
mod tests;
mod timeout;

use crate::nullnet_grpc_impl::NullnetGrpcImpl;
use nullnet_grpc_lib::nullnet_grpc::nullnet_grpc_server::NullnetGrpcServer;
use nullnet_liberror::{Error, ErrorHandler, Location, location};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::{panic, process};
use tonic::transport::{Identity, Server, ServerTlsConfig};

const PORT: u16 = 50051;
/// Default path for the SQLite database; override with `DATABASE_URL`.
const DEFAULT_DATABASE_URL: &str = "/var/nullnet/data/nullnet.db";

#[tokio::main]
async fn main() -> Result<(), Error> {
    // let _gag1: gag::Redirect<std::fs::File>;
    // let _gag2: gag::Redirect<std::fs::File>;
    // if let Some((gag1, gag2)) = redirect_stdout_stderr_to_file() {
    //     _gag1 = gag1;
    //     _gag2 = gag2;
    // } else {
    //     println!("Failed to redirect stdout and stderr to file, logs will be printed to console");
    // }

    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), PORT);

    // cert private keys are encrypted at rest with this key; fail fast if absent
    crypto::init_from_env()?;
    // JWT signing key + MFA-secret encryption key: same fail-fast pattern,
    // distinct keys/env vars so neither shares blast radius with the other
    // or with CERT_ENCRYPTION_KEY.
    auth::jwt::init_from_env()?;
    auth::mfa_crypto::init_from_env()?;

    // SQLite-backed storage for server data (certs/services, going forward):
    // pending schema migrations run automatically before anything else starts.
    let database_url =
        std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_string());
    let db = db::Db::open(&database_url).await?;
    // Create the first admin account if none exists yet. Defaults to
    // 'admin'/'admin' when the bootstrap env vars aren't set, so a fresh
    // deployment is never locked out of its own admin UI — but that's a
    // well-known credential pair, so warn loudly every time it's used.
    let bootstrap_username_env = std::env::var("ADMIN_BOOTSTRAP_USERNAME")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let bootstrap_password_env = std::env::var("ADMIN_BOOTSTRAP_PASSWORD")
        .ok()
        .filter(|s| !s.is_empty());
    if bootstrap_username_env.is_none() || bootstrap_password_env.is_none() {
        println!(
            "WARNING: ADMIN_BOOTSTRAP_USERNAME/ADMIN_BOOTSTRAP_PASSWORD not set — defaulting the \
             initial admin account to 'admin'/'admin'. Change this password immediately after \
             first login (this only affects a brand-new deployment with no existing users)."
        );
    }
    let bootstrap_username = bootstrap_username_env.unwrap_or_else(|| "admin".to_string());
    let bootstrap_password = bootstrap_password_env.unwrap_or_else(|| "admin".to_string());
    auth::bootstrap::ensure_admin_exists(&db, Some(&bootstrap_username), Some(&bootstrap_password))
        .await?;

    // The firewall allowlist is now global (single point of decision). An empty
    // ingress-TCP list means every client's host firewall drops ALL inbound TCP —
    // including SSH — on startup, so warn loudly rather than silently lock out the
    // fleet on the next client restart.
    if env::INGRESS_ALLOW_TCP_PORTS.is_empty() {
        println!(
            "WARNING: INGRESS_ALLOW_TCP_PORTS is empty — every client firewall will drop all \
             inbound TCP (including SSH/22). Set it in the server .env before starting clients."
        );
    }

    // Server-only TLS: encrypts the channel (WatchCertificates streams
    // customer private keys) but doesn't yet authenticate clients — mTLS is
    // a separate, still-open follow-up for that.
    let (cert_pem, key_pem) = grpc_tls::load_or_generate().await?;
    let tls_config = ServerTlsConfig::new().identity(Identity::from_pem(cert_pem, key_pem));
    let mut server = Server::builder()
        .tls_config(tls_config)
        .handle_err(location!())?;

    let nullnet = init_nullnet(db.clone()).await?;
    let app_state = http_server::AppState {
        services: nullnet.services().clone(),
        routes: nullnet.routes().clone(),
        events: nullnet.orchestrator().events.clone(),
        orchestrator: nullnet.orchestrator().clone(),
        db,
    };

    // auto-renew ACME certs nearing expiry (those with stored DNS credentials)
    cert_renewal::start(
        app_state.events.clone(),
        cert_renewal::RenewalConfig::from_env(),
    );
    // prune persisted events past the retention window (issue #151)
    events_retention::start(
        app_state.db.clone(),
        events_retention::RetentionConfig::from_env(),
    );

    tokio::select! {
        result = server
            .add_service(
                NullnetGrpcServer::new(nullnet)
                    .max_decoding_message_size(50 * 1024 * 1024),
            )
            .serve(addr) => {
            result.handle_err(location!())?;
        }
        () = http_server::serve(app_state) => {}
    }

    Ok(())
}

async fn init_nullnet(db: db::Db) -> Result<NullnetGrpcImpl, Error> {
    if cfg!(not(debug_assertions)) {
        // custom panic hook to correctly clean up the server, even in case a secondary thread fails
        let orig_hook = panic::take_hook();
        panic::set_hook(Box::new(move |panic_info| {
            // invoke the default handler and exit the process
            orig_hook(panic_info);
            process::exit(1);
        }));
    }

    // handle termination signals: SIGINT, SIGTERM, SIGHUP
    ctrlc::set_handler(move || {
        process::exit(1);
    })
    .handle_err(location!())?;

    NullnetGrpcImpl::new(db).await
}

// fn redirect_stdout_stderr_to_file()
// -> Option<(gag::Redirect<std::fs::File>, gag::Redirect<std::fs::File>)> {
//     let dir = "/var/log/nullnet";
//     std::fs::create_dir_all(dir).handle_err(location!()).ok()?;
//     let timestamp = chrono::Utc::now().format("%Y-%m-%d_%H-%M-%S");
//     let file_path = format!("{dir}/grpc_{timestamp}.txt");
//     if let Ok(logs_file) = std::fs::OpenOptions::new()
//         .create(true)
//         .append(true)
//         .open(&file_path)
//     {
//         println!("Writing logs to '{file_path}'");
//         return Some((
//             gag::Redirect::stdout(logs_file.try_clone().ok()?).ok()?,
//             gag::Redirect::stderr(logs_file).ok()?,
//         ));
//     }
//     None
// }
