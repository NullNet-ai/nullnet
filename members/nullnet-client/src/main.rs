#![allow(clippy::used_underscore_binding)]

use crate::cli::Args;
use crate::commands::{RtNetLinkHandle, cleanup_network, setup_br0};
use crate::control_channel::control_channel;
use crate::ebpf::triggers::TriggersState;
use crate::env::{CONTROL_SERVICE_ADDR, CONTROL_SERVICE_PORT, ETH_NAME};
use crate::forward::receive::receive;
use crate::forward::send::send;
use crate::host_mappings::HostMappingsState;
use crate::local_endpoints::LocalEndpoints;
use crate::peers::peer::Peers;
use clap::Parser;
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use nullnet_firewall::{DataLink, Firewall, FirewallError, LogLevel};
use nullnet_grpc_lib::NullnetGrpcInterface;
use nullnet_grpc_lib::nullnet_grpc::{
    AgentBackendTriggerSendFailed, AgentEvent, AgentFirewallRulesLoadFailed,
    AgentServicesListUpdateFailed, AgentServicesListUpdated, Net, Services,
    agent_event::Event as AgentEventKind,
};
use nullnet_liberror::{Error, ErrorHandler, Location, location};
use std::collections::HashMap;
use std::ops::Sub;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::{panic, process};
use tokio::sync::RwLock;
use tokio::sync::mpsc::UnboundedSender;
use tun_rs::{DeviceBuilder, Layer};

mod cli;
mod commands;
mod control_channel;
mod craft;
mod ebpf;
mod env;
mod forward;
mod host_mappings;
mod local_endpoints;
mod peers;

pub const FORWARD_PORT: u16 = 9999;
pub const TAP_NAME: &str = "nullnet0";

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
    let Args {
        firewall_path,
        num_tasks,
        ..
    } = Args::parse();

    // create a handle to execute netlink commands
    let rtnetlink_handle = RtNetLinkHandle::new()?;

    // cleanup existing VLANs and VXLANs material
    cleanup_network(&rtnetlink_handle).await;

    // maps of all the peers
    let peers = Arc::new(RwLock::new(Peers::default()));
    let peers_2 = peers.clone();

    // create firewall based on the defined rules
    let mut firewall = Firewall::new();
    firewall.log_level(LogLevel::Db);
    firewall.data_link(DataLink::Ethernet);
    let firewall_shared = Arc::new(RwLock::new(firewall));
    set_firewall_rules(&firewall_shared, &firewall_path, true, None).await?;

    // initialize gRPC connection
    let grpc_server = grpc_init().await?;
    let grpc_server2 = grpc_server.clone();
    let grpc_server3 = grpc_server.clone();

    let net_type = grpc_server.network_type().await.handle_err(location!())?;

    if net_type.net() == Net::Vlan {
        setup_tap(num_tasks, peers, &firewall_shared, &rtnetlink_handle).await?;
        setup_br0(&rtnetlink_handle).await;
    }

    print_info(net_type.net());

    // shared dedup state (one trigger pending/active per port until teardown)
    let triggers_state = Arc::new(TriggersState::default());
    let triggers_state_cc = triggers_state.clone();
    let triggers_state_tr = triggers_state.clone();

    // remember /etc/hosts entries installed at setup so teardown can undo them
    let host_mappings_state = Arc::new(HostMappingsState::default());

    // listen on the gRPC control channel
    tokio::spawn(async move {
        control_channel(
            grpc_server2,
            peers_2,
            rtnetlink_handle,
            triggers_state_cc,
            host_mappings_state,
        )
        .await
        .expect("Control channel failed");
    });

    // observe outgoing dependency-port traffic via eBPF; the observer's
    // watch-port set is driven by the services-list response from the server.
    let (config_tx, config_rx) = tokio::sync::mpsc::unbounded_channel::<HashMap<u16, String>>();
    let (trigger_tx, mut trigger_rx) = tokio::sync::mpsc::unbounded_channel::<(String, u16)>();
    ebpf::load::load_ebpf(&ETH_NAME, config_rx, trigger_tx);

    // declare services + push trigger config to the eBPF observer on each refresh
    let grpc_server_ds = grpc_server.clone();
    tokio::spawn(async move {
        declare_services(grpc_server_ds, config_tx)
            .await
            .expect("Failed to declare services");
    });

    // forward observed triggers to the gRPC server
    tokio::spawn(async move {
        while let Some((service_name, port)) = trigger_rx.recv().await {
            if !triggers_state_tr.try_mark_pending(port) {
                continue;
            }
            if let Err(e) = grpc_server3
                .backend_trigger(service_name.clone(), u32::from(port))
                .await
            {
                eprintln!("backend_trigger for '{service_name}' port {port} failed: {e}");
                triggers_state_tr.forget(port);
                let grpc = grpc_server3.clone();
                let svc = service_name.clone();
                let err_msg = e.clone();
                tokio::spawn(async move {
                    let _ = grpc
                        .report_event(AgentEvent {
                            event: Some(AgentEventKind::BackendTriggerSendFailed(
                                AgentBackendTriggerSendFailed {
                                    service_name: svc,
                                    port: u32::from(port),
                                    error_message: err_msg,
                                },
                            )),
                        })
                        .await;
                });
            }
        }
    });

    // watch the file defining rules and update the firewall accordingly
    set_firewall_rules(
        &firewall_shared,
        &firewall_path,
        false,
        Some(grpc_server.clone()),
    )
    .await?;

    Ok(())
}

/// Prints useful info about the local environment and the created interface.
fn print_info(net: Net) {
    println!("\n{}", "=".repeat(40));
    println!("Nullnet is up and running!");
    println!("Network type: {net:?}");
    println!("{}\n", "=".repeat(40));
}

/// Loads and refreshes firewall rules whenever the corresponding file is updated.
/// `grpc` is only used in the reload path (watch loop); initial load happens before gRPC is up.
async fn set_firewall_rules(
    firewall: &Arc<RwLock<Firewall>>,
    firewall_path: &str,
    is_init: bool,
    grpc: Option<NullnetGrpcInterface>,
) -> Result<(), Error> {
    let print_info = |result: &Result<(), FirewallError>, is_init: bool| match result {
        Err(err) => {
            println!("{err}");
            if is_init {
                println!("Waiting for a valid firewall file...");
            } else {
                println!("Firewall was not updated!");
            }
        }
        Ok(()) => {
            if is_init {
                println!("A valid firewall has been instantiated!");
            } else {
                println!("Firewall has been updated!");
            }
        }
    };

    if is_init {
        let result = firewall.write().await.set_rules(firewall_path);
        print_info(&result, is_init);
        if result.is_ok() {
            return Ok(());
        }
    }

    let firewall_path_owned = firewall_path.to_string();
    let mut firewall_directory = PathBuf::from(firewall_path);
    firewall_directory.pop();

    let (tx, rx) = std::sync::mpsc::channel();
    let mut watcher = RecommendedWatcher::new(tx, Config::default()).handle_err(location!())?;
    watcher
        .watch(&firewall_directory, RecursiveMode::Recursive)
        .handle_err(location!())?;

    let mut last_update_time = Instant::now().sub(Duration::from_secs(60));

    loop {
        // only update rules if the event is related to a file change
        if let Ok(Ok(Event {
            kind: EventKind::Modify(_),
            ..
        })) = rx.recv()
        {
            // debounce duplicated events
            if last_update_time.elapsed().as_millis() > 100 {
                // ensure file changes are propagated
                tokio::time::sleep(Duration::from_millis(100)).await;
                let result = firewall.write().await.set_rules(&firewall_path_owned);
                print_info(&result, is_init);
                if let Err(ref err) = result {
                    if let Some(ref g) = grpc {
                        let g = g.clone();
                        let path = firewall_path_owned.clone();
                        let error_message = err.to_string();
                        tokio::spawn(async move {
                            let _ = g
                                .report_event(AgentEvent {
                                    event: Some(AgentEventKind::FirewallRulesLoadFailed(
                                        AgentFirewallRulesLoadFailed {
                                            path,
                                            error_message,
                                        },
                                    )),
                                })
                                .await;
                        });
                    }
                }
                if result.is_ok() && is_init {
                    return Ok(());
                }
                last_update_time = Instant::now();
            }
        }
    }
}

async fn grpc_init() -> Result<NullnetGrpcInterface, Error> {
    let host = CONTROL_SERVICE_ADDR.to_string();
    let port = *CONTROL_SERVICE_PORT;

    let server = NullnetGrpcInterface::new(&host, port, false)
        .await
        .handle_err(location!())?;

    Ok(server)
}

async fn declare_services(
    grpc_server: NullnetGrpcInterface,
    config_tx: UnboundedSender<HashMap<u16, String>>,
) -> Result<(), Error> {
    let mut last_declared: Vec<nullnet_grpc_lib::nullnet_grpc::Service> = Vec::new();
    loop {
        // read services from file
        let services_toml = tokio::fs::read_to_string("services.toml")
            .await
            .handle_err(location!())?;
        let mut services: Services = toml::from_str(&services_toml).handle_err(location!())?;

        // get the map of logical name -> real container name (supports both standalone and Swarm)
        let running_containers = get_running_docker_containers().await;
        // get the list of actively listening ports on the host
        let listeners = listeners::get_all().handle_err(location!())?;

        // only declare services that are actually running
        // For Swarm, a single service name may map to multiple containers (replicas),
        // so we expand each service entry into one entry per running container.
        let file_services = services.services;
        services.services = Vec::new();
        for service in file_services {
            if let Some(container) = &service.docker_container {
                if let Some(real_names) = running_containers.get(container.as_str()) {
                    for real_name in real_names {
                        let mut s = service.clone();
                        s.docker_container = Some(real_name.clone());
                        services.services.push(s);
                    }
                }
            } else {
                // Host services: only declare if the port is actively listening
                if listeners
                    .iter()
                    .any(|listener| u32::from(listener.socket.port()) == service.port)
                {
                    services.services.push(service);
                }
            }
        }

        println!("Declaring services to gRPC server: {services:?}");
        let num_services = services.services.len() as u32;

        // canonical snapshot for change detection (order-independent)
        let mut current = services.services.clone();
        current.sort_by(|a, b| {
            a.name
                .cmp(&b.name)
                .then(a.stack.cmp(&b.stack))
                .then(a.port.cmp(&b.port))
                .then(a.docker_container.cmp(&b.docker_container))
        });

        // send services to gRPC server; response carries the trigger ports
        // attached to the services we just declared as hosting.
        match grpc_server.services_list(services).await {
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
                if current != last_declared {
                    last_declared = current;
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

                let mut port_to_service: HashMap<u16, String> = HashMap::new();
                for st in response.service_triggers {
                    for port in st.ports {
                        let Ok(port) = u16::try_from(port) else {
                            eprintln!("server returned invalid trigger port {port}; skipping");
                            continue;
                        };
                        port_to_service.insert(port, st.service_name.clone());
                    }
                }
                if config_tx.send(port_to_service).is_err() {
                    // observer task gone; nothing more to do here
                    return Ok(());
                }
            }
        }

        // wait before re-declaring services
        tokio::time::sleep(Duration::from_secs(10)).await;
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
    firewall_shared: &Arc<RwLock<Firewall>>,
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
        let firewall_1 = firewall_shared.clone();
        let firewall_2 = firewall_shared.clone();
        let peers_2 = peers.clone();

        // handle incoming traffic
        tokio::spawn(async move {
            Box::pin(receive(&writer, &socket_1, &firewall_1)).await;
        });

        // handle outgoing traffic
        tokio::spawn(async move {
            Box::pin(send(&reader, &socket_2, &firewall_2, peers_2)).await;
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
