use crate::nullnet_proxy::NullnetProxy;
use crate::{tcp_relay, udp_relay};
use nullnet_grpc_lib::nullnet_grpc::{PortMappingBundle, ServiceProtocol};
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::watch;
use tokio::task::JoinHandle;

#[derive(Clone, Copy, Eq, PartialEq, Hash, Debug)]
pub(crate) enum Protocol {
    Tcp,
    Udp,
}

/// The service a `(protocol, listen_port)` pair routes to, plus the idle
/// timeout to apply to local sessions (UDP only — TCP relies on the OS to
/// signal connection close). Mirrors the server's per-service `timeout`;
/// `0` disables it, same as everywhere else in the config.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MappingEntry {
    pub(crate) service_name: String,
    pub(crate) idle_timeout_secs: u64,
}

struct RunningListener {
    handle: JoinHandle<()>,
    mapping_tx: watch::Sender<MappingEntry>,
}

/// Subscribe to the server's port-mapping stream and keep TCP/UDP listeners in
/// sync with it for as long as the proxy runs. Reconnects on stream drop —
/// the certs watcher already exits the process if the control service itself
/// goes down, so this loop only needs to ride out transient hiccups.
pub(crate) async fn watch_and_serve(proxy: NullnetProxy) {
    loop {
        match proxy.server.watch_port_mappings().await {
            Ok(mut stream) => {
                println!("[port-mappings] watch stream opened");
                let mut running: HashMap<(Protocol, u16), RunningListener> = HashMap::new();
                loop {
                    match stream.message().await {
                        Ok(Some(bundle)) => {
                            println!(
                                "[port-mappings] received bundle: {} mapping(s): [{}]",
                                bundle.mappings.len(),
                                bundle
                                    .mappings
                                    .iter()
                                    .map(|m| format!(
                                        "{:?}/{}→'{}'",
                                        ServiceProtocol::try_from(m.protocol)
                                            .unwrap_or(ServiceProtocol::Http),
                                        m.listen_port,
                                        m.service_name
                                    ))
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            );
                            reconcile(&proxy, &mut running, bundle);
                        }
                        Ok(None) => {
                            println!("[port-mappings] watch stream closed by server");
                            break;
                        }
                        Err(e) => {
                            eprintln!("[port-mappings] watch stream error: {e}");
                            break;
                        }
                    }
                }
                for (key, listener) in running {
                    listener.handle.abort();
                    println!("[port-mappings] stopped {key:?} (watch stream closed)");
                }
            }
            Err(e) => eprintln!("[port-mappings] failed to open watch stream: {e}"),
        }
        tokio::time::sleep(Duration::from_secs(10)).await;
    }
}

/// Diff the freshly-pushed table against the listeners we currently run:
/// bind a new socket only for ports that just appeared, push updated
/// service_name/timeout to existing listeners without touching their socket,
/// and abort listeners for ports that disappeared.
fn reconcile(
    proxy: &NullnetProxy,
    running: &mut HashMap<(Protocol, u16), RunningListener>,
    bundle: PortMappingBundle,
) {
    let mut desired: HashMap<(Protocol, u16), MappingEntry> = HashMap::new();
    for m in bundle.mappings {
        let Some(protocol) = local_protocol(m.protocol) else {
            continue;
        };
        let Ok(listen_port) = u16::try_from(m.listen_port) else {
            eprintln!(
                "[port-mappings] ignoring '{}': listen_port {} out of range",
                m.service_name, m.listen_port
            );
            continue;
        };
        desired.insert(
            (protocol, listen_port),
            MappingEntry {
                service_name: m.service_name,
                idle_timeout_secs: m.idle_timeout_secs,
            },
        );
    }

    let removed: Vec<(Protocol, u16)> = running
        .keys()
        .filter(|key| !desired.contains_key(key))
        .copied()
        .collect();
    for key in removed {
        if let Some(listener) = running.remove(&key) {
            listener.handle.abort();
            println!("[port-mappings] stopped {key:?}");
        }
    }

    for (key, entry) in desired {
        if let Some(listener) = running.get(&key) {
            let _ = listener.mapping_tx.send(entry);
        } else {
            println!(
                "[port-mappings] starting {key:?} -> '{}'",
                entry.service_name
            );
            let (mapping_tx, mapping_rx) = watch::channel(entry);
            let proxy = proxy.clone();
            let handle = match key.0 {
                Protocol::Tcp => tokio::spawn(tcp_relay::run(proxy, key.1, mapping_rx)),
                Protocol::Udp => tokio::spawn(udp_relay::run(proxy, key.1, mapping_rx)),
            };
            running.insert(key, RunningListener { handle, mapping_tx });
        }
    }
}

fn local_protocol(raw: i32) -> Option<Protocol> {
    match ServiceProtocol::try_from(raw).ok()? {
        ServiceProtocol::Tcp => Some(Protocol::Tcp),
        ServiceProtocol::Udp => Some(Protocol::Udp),
        ServiceProtocol::Http => None,
    }
}
