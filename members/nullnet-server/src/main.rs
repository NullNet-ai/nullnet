mod cert;
mod cert_renewal;
mod certs;
mod crypto;
mod db;
mod env;
mod events;
mod geo;
mod graphviz;
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
use tonic::transport::Server;

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

    // SQLite-backed storage for server data (certs/services, going forward):
    // pending schema migrations run automatically before anything else starts.
    let database_url =
        std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_string());
    db::Db::open(&database_url).await?;

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

    // TODO(grpc-tls): the gRPC server is plaintext, but WatchCertificates
    // streams private keys. Enable TLS here (Server::builder().tls_config(..)),
    // ideally mTLS, so keys never travel in clear; until then keep the control
    // plane on a trusted network. Proxy side: NullnetGrpcInterface::new(.., true).
    let mut server = Server::builder();

    let nullnet = init_nullnet().await?;
    let app_state = http_server::AppState {
        services: nullnet.services().clone(),
        events: nullnet.orchestrator().events.clone(),
        orchestrator: nullnet.orchestrator().clone(),
    };

    // auto-renew ACME certs nearing expiry (those with stored DNS credentials)
    cert_renewal::start(
        app_state.events.clone(),
        cert_renewal::RenewalConfig::from_env(),
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

async fn init_nullnet() -> Result<NullnetGrpcImpl, Error> {
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

    NullnetGrpcImpl::new().await
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
