#![allow(clippy::used_underscore_binding)]

use crate::cli::Args;
use crate::commands::{
    RtNetLinkHandle, cleanup_network, find_ethernet_interface, find_ethernet_ip, setup_br0,
};
use crate::control_channel::control_channel;
use crate::env::{CONTROL_SERVICE_ADDR, CONTROL_SERVICE_CA_CERT, CONTROL_SERVICE_PORT};
use crate::forward::receive::receive;
use crate::forward::send::send;
use crate::host_mappings::HostMappingsState;
use crate::local_endpoints::LocalEndpoints;
use crate::nfqueue::{TriggerMap, TriggerOwner};
use crate::peers::peer::Peers;
use crate::triggers::TriggersState;
use clap::Parser;
use nullnet_grpc_lib::NullnetGrpcInterface;
use nullnet_grpc_lib::nullnet_grpc::{
    AgentEvent, AgentFirewallRulesLoadFailed, AgentMssClampInstallFailed,
    AgentServicesListUpdateFailed, AgentServicesListUpdated, Container, Listener, Net, NetType,
    ServiceReport, agent_event::Event as AgentEventKind,
};
use nullnet_liberror::{Error, ErrorHandler, Location, location};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use std::{panic, process};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::{Notify, RwLock};
use tun_rs::{DeviceBuilder, Layer};

mod cli;
mod commands;
mod control_channel;
mod crypto;
mod ebpf;
mod egress_policy;
mod egress_state;
mod env;
mod forward;
mod host_mappings;
mod local_endpoints;
mod nfqueue;
mod peers;
mod triggers;

pub const FORWARD_PORT: u16 = 9999;
pub const TAP_NAME: &str = "nullnet0";
/// Shared VXLAN dstport for a tunnel that doesn't need a dedicated one — must
/// match nullnet-server's `DEFAULT_VXLAN_DSTPORT` (net_id_pool.rs) and the
/// eBPF firewall's `VXLAN_PORT` constant (ebpf/src/main.rs).
pub const DEFAULT_VXLAN_DSTPORT: u16 = 4789;

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

    // kill the main thread as soon as a secondary thread panics
    let orig_hook = panic::take_hook();
    panic::set_hook(Box::new(move |panic_info| {
        // invoke the default handler and exit the process
        orig_hook(panic_info);
        process::exit(1);
    }));

    // read CLI arguments
    let Args { num_tasks, .. } = Args::parse();

    // create a handle to execute netlink commands
    let rtnetlink_handle = RtNetLinkHandle::new()?;

    // cleanup existing VLANs and VXLANs material
    let mss_error = cleanup_network(&rtnetlink_handle).await;

    // maps of all the peers
    let peers = Arc::new(RwLock::new(Peers::default()));
    let peers_2 = peers.clone();

    // initialize gRPC connection
    let grpc_server = grpc_init().await?;
    let grpc_server2 = grpc_server.clone();
    let grpc_server3 = grpc_server.clone();

    // Deferred from `cleanup_network`, which runs before this connection exists.
    // Without the clamp, oversized segments are silently black-holed once they
    // enter a tunnel, which is near-impossible to trace from the symptom.
    if let Some(error_message) = mss_error {
        let grpc = grpc_server.clone();
        tokio::spawn(async move {
            let _ = grpc
                .report_event(AgentEvent {
                    event: Some(AgentEventKind::MssClampInstallFailed(
                        AgentMssClampInstallFailed { error_message },
                    )),
                })
                .await;
        });
    }

    let net_type = grpc_server.network_type().await.handle_err(location!())?;

    if net_type.net() == Net::Vlan {
        setup_tap(num_tasks, peers, &rtnetlink_handle).await?;
        setup_br0(&rtnetlink_handle).await;
    }

    print_info(net_type.net());

    // bring up the host-NIC eBPF default-deny firewall. Must happen before the
    // control channel learns peers so its add()/remove() land in the PEERS map.
    // Held alive for the whole run (drop = detach). Fails closed: any error
    // aborts startup rather than running unprotected.
    let ebpf_firewall = match setup_ebpf_firewall(&rtnetlink_handle, &net_type).await {
        Ok(fw) => {
            println!("eBPF host firewall enabled (strict nullnet-only)");
            fw
        }
        Err(e) => {
            eprintln!("Failed to enable eBPF firewall: {e:?}");
            // Awaited, not fire-and-forget: the exit below would kill a spawned
            // task before it ever reached the server. Bounded, because
            // `report_event` has no request timeout of its own and a hung one
            // would keep us from exiting at all.
            let _ = tokio::time::timeout(
                Duration::from_secs(5),
                grpc_server.report_event(AgentEvent {
                    event: Some(AgentEventKind::FirewallRulesLoadFailed(
                        AgentFirewallRulesLoadFailed {
                            path: "ebpf host firewall".to_string(),
                            error_message: format!("{e:?}"),
                        },
                    )),
                }),
            )
            .await;
            process::exit(1);
        }
    };
    let firewall_peers = ebpf_firewall.peers.clone();
    let firewall_vxlan_ports = ebpf_firewall.vxlan_ports.clone();

    // shared dedup + waiter state, keyed by (initiator_container, port).
    // The NFQUEUE listener marks Pending and awaits the Notify; the control
    // channel marks Active when the matching VxlanSetup lands.
    let triggers_state = Arc::new(TriggersState::default());
    let triggers_state_cc = triggers_state.clone();

    // remember /etc/hosts entries installed at setup so teardown can undo them
    let host_mappings_state = Arc::new(HostMappingsState::default());

    // remember egress plumbing installed at setup so teardown can reverse it
    let egress_state = Arc::new(egress_state::EgressState::default());

    // bridge-IP → container cache + egress policy-verdict cache, shared by the
    // NFQUEUE listeners and the control channel (which flushes them when the
    // server pushes an egress policy change).
    let bridge_cache = nfqueue::BridgeIpCache::new();
    let policy_verdicts = Arc::new(egress_policy::PolicyVerdicts::default());
    let bridge_cache_cc = bridge_cache.clone();
    let policy_verdicts_cc = policy_verdicts.clone();

    // listen on the gRPC control channel
    tokio::spawn(async move {
        if let Err(e) = control_channel(
            grpc_server2,
            peers_2,
            rtnetlink_handle,
            triggers_state_cc,
            host_mappings_state,
            firewall_peers,
            firewall_vxlan_ports,
            egress_state,
            bridge_cache_cc,
            policy_verdicts_cc,
        )
        .await
        {
            eprintln!("Control channel failed: {e:?}");
        }
        // control_channel only returns when the server stream drops (server
        // down). Exit so the supervisor restarts us with a clean env.
        eprintln!("Control channel to server closed; exiting for restart");
        process::exit(1);
    });

    // NFQUEUE listener owns trigger detection: kernel queues the first
    // packet of each new watched-port flow, listener fires backend_trigger
    // with the resolved initiator container, waits for VxlanSetup to install
    // DNAT, then verdicts ACCEPT so the original packet hits the new chain.
    let (config_tx, config_rx) = tokio::sync::mpsc::unbounded_channel::<TriggerMap>();

    // Poked by the cache's docker-events watcher after every container
    // start/die so `declare_services` re-runs immediately instead of
    // waiting up to its 10 s polling interval. Without this, a freshly
    // started task can fire its first SYN to a trigger port before that
    // port has been added back to the ipset, so NFQUEUE misses the SYN,
    // the kernel routes it to the public-IP DNS resolution, and the app
    // gets ECHO/EHOSTUNREACH before we ever get a chance to DNAT.
    let docker_changed = Arc::new(Notify::new());

    nfqueue::spawn_listener(
        grpc_server3,
        triggers_state,
        config_rx,
        docker_changed.clone(),
        bridge_cache,
        policy_verdicts,
    );

    // declare services + push the port→trigger-owners map to the NFQUEUE
    // listener on each refresh.
    tokio::spawn(async move {
        declare_services(grpc_server, config_tx, docker_changed)
            .await
            .expect("Failed to declare services");
    });

    // all work runs in the spawned tasks above; keep the process alive.
    std::future::pending::<()>().await;

    Ok(())
}

/// Prints useful info about the local environment and the created interface.
fn print_info(net: Net) {
    println!("\n{}", "=".repeat(40));
    println!("Nullnet is up and running!");
    println!("Network type: {net:?}");
    println!("{}\n", "=".repeat(40));
}

/// Resolve, attach, and return the host-NIC eBPF firewall. Fails closed: any
/// problem (unresolvable server, missing NIC, load error) aborts startup rather
/// than running unprotected.
async fn setup_ebpf_firewall(
    rtnetlink_handle: &RtNetLinkHandle,
    net_type: &NetType,
) -> Result<ebpf::Firewall, Error> {
    let server_ip = resolve_server_ip()
        .ok_or("could not resolve CONTROL_SERVICE_ADDR to an IPv4 address")
        .handle_err(location!())?;
    if server_ip.is_unspecified() {
        return Err(
            "CONTROL_SERVICE_ADDR is unspecified (0.0.0.0); refusing to enable the \
                    firewall as it would block the control plane",
        )
        .handle_err(location!());
    }

    let eth_ip = find_ethernet_ip(rtnetlink_handle)
        .await
        .ok_or("could not find the local ethernet IP")
        .handle_err(location!())?;
    let iface = find_ethernet_interface(eth_ip)
        .ok_or("could not find the ethernet interface name")
        .handle_err(location!())?;

    // Firewall policy is decided globally by the server and delivered in the
    // NetworkType response (fetched before this runs). Ports arrive as u16-in-u32;
    // skip any out-of-range value rather than silently truncating it to a port.
    let to_u16 = |ports: &[u32]| {
        ports
            .iter()
            .filter_map(|&p| u16::try_from(p).ok())
            .collect::<Vec<u16>>()
    };
    let cfg = ebpf::FirewallConfig {
        server_ip,
        control_port: *CONTROL_SERVICE_PORT,
        egress_gateway: net_type.egress_gateway,
        ingress_tcp: to_u16(&net_type.ingress_allow_tcp_ports),
        ingress_udp: to_u16(&net_type.ingress_allow_udp_ports),
        egress_tcp: to_u16(&net_type.egress_allow_tcp_ports),
        egress_udp: to_u16(&net_type.egress_allow_udp_ports),
    };
    if cfg.egress_gateway {
        println!(
            "Attaching eBPF firewall to {iface} in EGRESS-GATEWAY mode (stateful boundary: \
             outbound allowed + tracked; inbound = established + allowlist \
             tcp{:?} udp{:?}; ICMP always allowed)",
            cfg.ingress_tcp, cfg.ingress_udp
        );
    } else {
        println!(
            "Attaching eBPF firewall to {iface} (stateful strict; control plane \
             {server_ip}:{}; allow in tcp{:?} udp{:?}, out tcp{:?} udp{:?}; ICMP always allowed)",
            cfg.control_port, cfg.ingress_tcp, cfg.ingress_udp, cfg.egress_tcp, cfg.egress_udp
        );
    }
    ebpf::enable(&iface, &cfg)
}

/// Resolve `CONTROL_SERVICE_ADDR:CONTROL_SERVICE_PORT` to its first IPv4.
fn resolve_server_ip() -> Option<std::net::Ipv4Addr> {
    use std::net::{IpAddr, ToSocketAddrs};
    let host = CONTROL_SERVICE_ADDR.as_str();
    let port = *CONTROL_SERVICE_PORT;
    (host, port)
        .to_socket_addrs()
        .ok()?
        .find_map(|sa| match sa.ip() {
            IpAddr::V4(v4) => Some(v4),
            IpAddr::V6(_) => None,
        })
}

async fn grpc_init() -> Result<NullnetGrpcInterface, Error> {
    let host = CONTROL_SERVICE_ADDR.to_string();
    let port = *CONTROL_SERVICE_PORT;
    let server =
        NullnetGrpcInterface::new(&host, port, Path::new(CONTROL_SERVICE_CA_CERT.as_str()))
            .await
            .handle_err(location!())?;

    Ok(server)
}

async fn declare_services(
    grpc_server: NullnetGrpcInterface,
    config_tx: UnboundedSender<TriggerMap>,
    docker_changed: Arc<Notify>,
) -> Result<(), Error> {
    let mut last_snapshot: Vec<String> = Vec::new();
    loop {
        // Report raw local observations; the server joins them against its
        // per-stack config to decide what this node hosts. Running containers:
        // logical (Swarm label / name) -> real container name(s).
        let containers: Vec<Container> = get_running_docker_containers()
            .await
            .into_iter()
            .flat_map(|(match_key, real_names)| {
                real_names.into_iter().map(move |real_name| Container {
                    match_key: match_key.clone(),
                    real_name,
                })
            })
            .collect();

        // One Listener per distinct listening process, keyed by exe path.
        let mut paths: Vec<String> = listeners::get_all()
            .handle_err(location!())?
            .into_iter()
            .map(|l| l.process.path)
            .collect();
        paths.sort_unstable();
        paths.dedup();
        let report = ServiceReport {
            containers,
            listeners: paths.into_iter().map(|path| Listener { path }).collect(),
        };

        println!("Reporting to gRPC server: {report:?}");
        let num_services = (report.containers.len() + report.listeners.len()) as u32;

        // canonical snapshot for change detection (order-independent)
        let mut snapshot: Vec<String> = report
            .containers
            .iter()
            .map(|c| format!("c:{}|{}", c.match_key, c.real_name))
            .chain(report.listeners.iter().map(|l| format!("l:{}", l.path)))
            .collect();
        snapshot.sort_unstable();

        // send observations to gRPC server; response carries the trigger ports
        // attached to the services the server matched us to.
        match grpc_server.services_list(report).await {
            Err(e) => {
                eprintln!("services_list failed: {e}");
                let grpc = grpc_server.clone();
                let error_message = e.clone();
                tokio::spawn(async move {
                    let _ = grpc
                        .report_event(AgentEvent {
                            event: Some(AgentEventKind::ServicesListUpdateFailed(
                                AgentServicesListUpdateFailed {
                                    error_message,
                                    num_services,
                                },
                            )),
                        })
                        .await;
                });
            }
            Ok(response) => {
                if snapshot != last_snapshot {
                    last_snapshot = snapshot;
                    let grpc = grpc_server.clone();
                    tokio::spawn(async move {
                        let _ = grpc
                            .report_event(AgentEvent {
                                event: Some(AgentEventKind::ServicesListUpdated(
                                    AgentServicesListUpdated { num_services },
                                )),
                            })
                            .await;
                    });
                }

                // One port may be claimed by several services on this node, so
                // each maps to a list of owners rather than a single service.
                let mut trigger_owners: TriggerMap = HashMap::new();
                for st in response.service_triggers {
                    for port in st.ports {
                        let Ok(port) = u16::try_from(port) else {
                            eprintln!("server returned invalid trigger port {port}; skipping");
                            continue;
                        };
                        trigger_owners.entry(port).or_default().push(TriggerOwner {
                            service: st.service_name.clone(),
                            containers: st.containers.clone(),
                        });
                    }
                }
                if config_tx.send(trigger_owners).is_err() {
                    // observer task gone; nothing more to do here
                    return Ok(());
                }
            }
        }

        // Wait up to 10 s before re-declaring, but cut the wait short on a
        // docker container start/die — that's what populates the ipset for
        // a freshly-started task before its app dials a trigger port.
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(10)) => {}
            _ = docker_changed.notified() => {}
        }
    }
}

/// Returns a map of logical name -> real container names for all running Docker containers.
///
/// Supports both standalone Docker (name -> [name]) and Swarm mode (swarm service label -> [replicas]).
async fn get_running_docker_containers() -> HashMap<String, Vec<String>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();

    // Query container name and Swarm service label together
    let output = tokio::process::Command::new("docker")
        .args([
            "ps",
            "--format",
            "{{.Names}}\t{{.Label \"com.docker.swarm.service.name\"}}",
        ])
        .output()
        .await;

    if let Ok(out) = output {
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            if line.is_empty() {
                continue;
            }
            let parts: Vec<&str> = line.split('\t').collect();
            let real_name = parts[0].to_string();
            let swarm_label = parts.get(1).unwrap_or(&"").trim();
            if swarm_label.is_empty() {
                // standalone: logical name = container name
                map.entry(real_name.clone()).or_default().push(real_name);
            } else {
                // Swarm: logical name = swarm service label, may have multiple replicas
                map.entry(swarm_label.to_string())
                    .or_default()
                    .push(real_name);
            }
        }
    }

    map
}

async fn setup_tap(
    num_tasks: u8,
    peers: Arc<RwLock<Peers>>,
    rtnetlink_handle: &RtNetLinkHandle,
) -> Result<(), Error> {
    // set up the local environment
    let endpoints = LocalEndpoints::setup(rtnetlink_handle).await?;
    let forward_socket = endpoints.forward_socket.clone();

    // create the asynchronous TAP device, and split it into reader & writer halves
    let device = DeviceBuilder::new()
        .name(TAP_NAME)
        .layer(Layer::L2)
        // TODO: MTU? GSO?
        // .mtu(mtu)
        .build_async()
        .handle_err(location!())?;

    let reader_shared = Arc::new(device);
    let writer_shared = reader_shared.clone();

    // spawn a number of asynchronous tasks to handle incoming and outgoing network traffic
    for _ in 0..num_tasks / 2 {
        let writer = writer_shared.clone();
        let reader = reader_shared.clone();
        let socket_1 = forward_socket.clone();
        let socket_2 = socket_1.clone();
        let peers_1 = peers.clone();
        let peers_2 = peers.clone();

        // handle incoming traffic
        tokio::spawn(async move {
            Box::pin(receive(&writer, &socket_1, &peers_1)).await;
        });

        // handle outgoing traffic
        tokio::spawn(async move {
            Box::pin(send(&reader, &socket_2, peers_2)).await;
        });
    }

    Ok(())
}

// fn redirect_stdout_stderr_to_file()
// -> Option<(gag::Redirect<std::fs::File>, gag::Redirect<std::fs::File>)> {
//     let dir = "/var/log/nullnet";
//     std::fs::create_dir_all(dir).handle_err(location!()).ok()?;
//     let timestamp = chrono::Utc::now().format("%Y-%m-%d_%H-%M-%S");
//     let file_path = format!("{dir}/tun_{timestamp}.txt");
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
