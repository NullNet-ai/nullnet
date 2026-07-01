use crate::nullnet_proxy::NullnetProxy;
use crate::port_mappings::MappingEntry;
use nullnet_grpc_lib::nullnet_grpc::{
    AgentEvent, AgentUdpListenerBindFailed, AgentUdpUpstreamConnectFailed,
    AgentUpstreamLookupFailed, ProxyRequest, agent_event::Event as AgentEventKind,
};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::sync::watch;
use tokio::task::JoinHandle;

const SWEEP_INTERVAL: Duration = Duration::from_secs(10);
const MAX_DATAGRAM: usize = 65536;

/// A live client↔upstream relay, keyed by the client's `(ip, port)`. UDP has
/// no connection-close signal, so unlike TCP, sessions are only ever reaped
/// by the idle-timeout sweep below.
struct Session {
    /// Connected to the upstream — `send`/`recv` address themselves.
    upstream: Arc<UdpSocket>,
    /// Pumps upstream replies back to the client via the shared listening
    /// socket. Aborted when the session is swept.
    relay_handle: JoinHandle<()>,
    last_active: Instant,
}

/// Bind a UDP listener on `listen_port` and relay datagrams to the service
/// named in `mapping`. The first datagram from a given client address
/// resolves an upstream via the same `Proxy` RPC the TCP/HTTP paths use;
/// subsequent datagrams from that address reuse the session until it goes
/// idle for `mapping.idle_timeout_secs` (0 disables the timeout, same as
/// everywhere else in the config).
pub(crate) async fn run(
    proxy: NullnetProxy,
    listen_port: u16,
    mapping: watch::Receiver<MappingEntry>,
) {
    let socket = match UdpSocket::bind(("0.0.0.0", listen_port)).await {
        Ok(s) => s,
        Err(e) => {
            let service_name = mapping.borrow().service_name.clone();
            eprintln!("[udp/{listen_port}] bind failed: {e}");
            let _ = proxy
                .server
                .report_event(AgentEvent {
                    event: Some(AgentEventKind::UdpListenerBindFailed(
                        AgentUdpListenerBindFailed {
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
    let socket = Arc::new(socket);
    println!("[udp/{listen_port}] listening");

    let mut sessions: HashMap<SocketAddr, Session> = HashMap::new();
    let mut sweep = tokio::time::interval(SWEEP_INTERVAL);
    let mut buf = vec![0u8; MAX_DATAGRAM];

    loop {
        tokio::select! {
            recv = socket.recv_from(&mut buf) => {
                match recv {
                    Ok((n, src)) => {
                        handle_datagram(&proxy, &socket, &mapping, &mut sessions, src, &buf[..n]).await;
                    }
                    Err(e) => eprintln!("[udp/{listen_port}] recv error: {e}"),
                }
            }
            _ = sweep.tick() => {
                sweep_idle(&mut sessions, mapping.borrow().idle_timeout_secs);
            }
        }
    }
}

async fn handle_datagram(
    proxy: &NullnetProxy,
    listen_socket: &Arc<UdpSocket>,
    mapping: &watch::Receiver<MappingEntry>,
    sessions: &mut HashMap<SocketAddr, Session>,
    src: SocketAddr,
    data: &[u8],
) {
    if let Some(session) = sessions.get_mut(&src) {
        session.last_active = Instant::now();
        if let Err(e) = session.upstream.send(data).await {
            eprintln!("[udp] send to upstream failed: {e}");
        }
        return;
    }

    let entry = mapping.borrow().clone();
    let proxy_req = ProxyRequest {
        client_ip: src.ip().to_string(),
        service_name: entry.service_name.clone(),
    };
    let upstream_addr = match proxy.get_or_add_upstream(proxy_req).await {
        Ok(u) => {
            println!(
                "[udp] new session from {src} → '{}' resolved upstream {u}",
                entry.service_name
            );
            u
        }
        Err(e) => {
            eprintln!(
                "[udp] upstream lookup failed for '{}' (client {src}): {e:?}",
                entry.service_name
            );
            let _ = proxy
                .server
                .report_event(AgentEvent {
                    event: Some(AgentEventKind::UpstreamLookupFailed(
                        AgentUpstreamLookupFailed {
                            service_name: entry.service_name,
                            client_ip: src.ip().to_string(),
                            error_message: format!("{e:?}"),
                        },
                    )),
                })
                .await;
            return;
        }
    };

    let upstream_socket = match UdpSocket::bind(("0.0.0.0", 0)).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[udp] failed to open upstream socket for {src}: {e}");
            return;
        }
    };
    if let Err(e) = upstream_socket.connect(upstream_addr).await {
        eprintln!("[udp] connect to upstream {upstream_addr} failed (client {src}): {e}");
        let _ = proxy
            .server
            .report_event(AgentEvent {
                event: Some(AgentEventKind::UdpUpstreamConnectFailed(
                    AgentUdpUpstreamConnectFailed {
                        service_name: entry.service_name,
                        client_ip: src.ip().to_string(),
                        error_message: e.to_string(),
                    },
                )),
            })
            .await;
        return;
    }
    let upstream_socket = Arc::new(upstream_socket);

    let relay_socket = listen_socket.clone();
    let relay_upstream = upstream_socket.clone();
    let relay_handle = tokio::spawn(async move {
        let mut buf = vec![0u8; MAX_DATAGRAM];
        while let Ok(n) = relay_upstream.recv(&mut buf).await {
            if relay_socket.send_to(&buf[..n], src).await.is_err() {
                break;
            }
        }
    });

    if let Err(e) = upstream_socket.send(data).await {
        eprintln!("[udp] initial send to upstream {upstream_addr} failed (client {src}): {e}");
    }

    sessions.insert(
        src,
        Session {
            upstream: upstream_socket,
            relay_handle,
            last_active: Instant::now(),
        },
    );
}

/// `idle_timeout_secs == 0` disables the timeout, same convention as the
/// server's per-service `timeout` everywhere else.
fn sweep_idle(sessions: &mut HashMap<SocketAddr, Session>, idle_timeout_secs: u64) {
    if idle_timeout_secs == 0 {
        return;
    }
    let timeout = Duration::from_secs(idle_timeout_secs);
    let now = Instant::now();
    let expired: Vec<SocketAddr> = sessions
        .iter()
        .filter(|(_, s)| now.duration_since(s.last_active) >= timeout)
        .map(|(addr, _)| *addr)
        .collect();
    for addr in expired {
        if let Some(session) = sessions.remove(&addr) {
            session.relay_handle.abort();
            println!("[udp] session from {addr} swept (idle > {idle_timeout_secs}s)");
        }
    }
}
