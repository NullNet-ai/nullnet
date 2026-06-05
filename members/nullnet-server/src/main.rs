mod certs;
mod env;
mod events;
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

    // TODO(grpc-tls): the gRPC server is plaintext. Once certificate private
    // keys are served over this channel (GetCertificates/WatchCertificates),
    // enable TLS here (e.g. `Server::builder().tls_config(...)`) — and ideally
    // mTLS — so keys never travel in the clear. Until then the control plane
    // must run on a trusted network. The proxy already supports a TLS channel
    // via `NullnetGrpcInterface::new(.., tls = true)`.
    let mut server = Server::builder();

    let nullnet = init_nullnet().await?;
    let app_state = http_server::AppState {
        services: nullnet.services().clone(),
        events: nullnet.orchestrator().events.clone(),
        orchestrator: nullnet.orchestrator().clone(),
    };

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
