use crate::nullnet_proxy::{ConnectionGuard, NullnetProxy};
use crate::port_mappings::MappingEntry;
use nullnet_grpc_lib::nullnet_grpc::{
    AgentEvent, AgentTcpListenerBindFailed, AgentTcpUpstreamConnectFailed,
    AgentUpstreamLookupFailed, ProxyRequest, agent_event::Event as AgentEventKind,
};
use tokio::io::copy_bidirectional;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

/// Bind a raw TCP listener on `listen_port` and forward every accepted
/// connection to the service named in `mapping`, resolving the upstream the
/// same way the HTTP path does (`Proxy` RPC → on-demand VXLAN → local
/// upstream). Exits if the bind fails — the supervisor logs nothing further,
/// the failure event is the record of what happened.
pub(crate) async fn run(
    proxy: NullnetProxy,
    listen_port: u16,
    mapping: watch::Receiver<MappingEntry>,
) {
    let listener = match TcpListener::bind(("0.0.0.0", listen_port)).await {
        Ok(l) => l,
        Err(e) => {
            let service_name = mapping.borrow().service_name.clone();
            eprintln!("[tcp/{listen_port}] bind failed: {e}");
            let _ = proxy
                .server
                .report_event(AgentEvent {
                    event: Some(AgentEventKind::TcpListenerBindFailed(
                        AgentTcpListenerBindFailed {
                            listen_port: u32::from(listen_port),
                            service_name,
                            error_message: e.to_string(),
                        },
                    )),
                })
                .await;
            return;
        }
    };
    println!("[tcp/{listen_port}] listening");

    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                let proxy = proxy.clone();
                let entry = mapping.borrow().clone();
                println!(
                    "[tcp/{listen_port}] accepted connection from {peer} → '{}'",
                    entry.service_name
                );
                tokio::spawn(async move {
                    handle_connection(proxy, stream, peer.to_string(), entry).await;
                });
            }
            Err(e) => eprintln!("[tcp/{listen_port}] accept error: {e}"),
        }
    }
}

async fn handle_connection(
    proxy: NullnetProxy,
    mut inbound: TcpStream,
    client_addr: String,
    entry: MappingEntry,
) {
    let client_ip = client_addr
        .parse::<std::net::SocketAddr>()
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|_| client_addr.clone());

    let proxy_req = ProxyRequest {
        client_ip: client_ip.clone(),
        service_name: entry.service_name.clone(),
    };
    // Ingress country policy: an explicit deny closes the connection. A check
    // error is logged and allowed through (upstream lookup fails anyway if the
    // control plane is down).
    match proxy
        .server
        .check_ingress(entry.service_name.clone(), client_ip.clone())
        .await
    {
        Ok(false) => {
            println!(
                "[tcp] ingress policy denied {client_addr} -> '{}'",
                entry.service_name
            );
            return;
        }
        Ok(true) => {}
        Err(e) => eprintln!(
            "[tcp] ingress check failed for '{}': {e}",
            entry.service_name
        ),
    }
    let upstream = match proxy.get_or_add_upstream(proxy_req).await {
        Ok(u) => {
            println!(
                "[tcp] {client_addr} → '{}' resolved upstream {u}",
                entry.service_name
            );
            u
        }
        Err(e) => {
            eprintln!(
                "[tcp] upstream lookup failed for '{}' (client {client_addr}): {e:?}",
                entry.service_name
            );
            let _ = proxy
                .server
                .report_event(AgentEvent {
                    event: Some(AgentEventKind::UpstreamLookupFailed(
                        AgentUpstreamLookupFailed {
                            service_name: entry.service_name,
                            client_ip,
                            error_message: format!("{e:?}"),
                        },
                    )),
                })
                .await;
            return;
        }
    };

    // Armed the instant the server has counted this connection. Every path out
    // from here — upstream-connect failure, relay error, normal close — drops
    // the guard and reports the close, so the count cannot leak.
    let _close_guard = ConnectionGuard::new(
        proxy.server.clone(),
        entry.service_name.clone(),
        client_ip.clone(),
    );

    let mut outbound = match TcpStream::connect(upstream).await {
        Ok(s) => {
            println!("[tcp] {client_addr} ↔ {upstream} relay started");
            s
        }
        Err(e) => {
            eprintln!("[tcp] dial upstream {upstream} failed (client {client_addr}): {e}");
            let _ = proxy
                .server
                .report_event(AgentEvent {
                    event: Some(AgentEventKind::TcpUpstreamConnectFailed(
                        AgentTcpUpstreamConnectFailed {
                            service_name: entry.service_name,
                            client_ip,
                            error_message: e.to_string(),
                        },
                    )),
                })
                .await;
            return;
        }
    };

    match copy_bidirectional(&mut inbound, &mut outbound).await {
        Ok((to_upstream, to_client)) => {
            println!(
                "[tcp] {client_addr} ↔ {upstream} relay closed (↑{to_upstream}B ↓{to_client}B)"
            );
        }
        Err(e) => {
            eprintln!("[tcp] {client_addr} ↔ {upstream} relay error: {e}");
        }
    }
}
