use crate::commands::{RtNetLinkHandle, configure_access_port, dnat, remove_vlan};
use crate::host_mappings::HostMappingsState;
use crate::peers::peer::{Peers, VethKey};
use crate::triggers::TriggersState;
use ipnetwork::Ipv4Network;
use nullnet_grpc_lib::NullnetGrpcInterface;
use nullnet_grpc_lib::nullnet_grpc::{
    AgentControlChannelAckFailed, AgentControlChannelClosed, AgentControlChannelEstablished,
    AgentDnatInstallFailed, AgentHostMappingFailed, AgentVlanSetupCompleted, AgentVlanSetupFailed,
    AgentVlanTeardownFailed, AgentVxlanSetupCompleted, AgentVxlanSetupFailed,
    AgentVxlanTeardownFailed,
};
use nullnet_grpc_lib::nullnet_grpc::{
    AgentEvent, HostMapping, MsgId, VlanSetup, VlanTeardown, VxlanSetup, VxlanTeardown,
    agent_event::Event as AgentEventKind, net_message,
};
use nullnet_liberror::{Error, ErrorHandler, Location, location};
use std::net::Ipv4Addr;
use std::sync::Arc;
use tokio::sync::mpsc::Sender;
use tokio::sync::{RwLock, mpsc};

/// Fire-and-forget: send an agent event to the server without blocking the caller.
fn fire_event(grpc: &NullnetGrpcInterface, kind: AgentEventKind) {
    let grpc = grpc.clone();
    let event = AgentEvent { event: Some(kind) };
    tokio::spawn(async move {
        let _ = grpc.report_event(event).await;
    });
}

pub(crate) async fn control_channel(
    server: NullnetGrpcInterface,
    peers: Arc<RwLock<Peers>>,
    rtnetlink_handle: RtNetLinkHandle,
    triggers_state: Arc<TriggersState>,
    host_mappings_state: Arc<HostMappingsState>,
) -> Result<(), Error> {
    let (outbound, grpc_rx) = mpsc::channel(64);
    let mut inbound = server
        .control_channel(grpc_rx)
        .await
        .handle_err(location!())?;

    fire_event(
        &server,
        AgentEventKind::ControlChannelEstablished(AgentControlChannelEstablished {}),
    );

    while let Ok(Some(message)) = inbound.message().await {
        let rtnetlink_handle = rtnetlink_handle.clone();
        let peers = peers.clone();
        let outbound = outbound.clone();
        let host_mappings_state = host_mappings_state.clone();
        let server = server.clone();
        match message.message {
            Some(net_message::Message::VlanSetup(vlan_setup)) => {
                tokio::spawn(async move {
                    let _ = handle_vlan_setup(
                        vlan_setup,
                        rtnetlink_handle,
                        peers,
                        outbound,
                        host_mappings_state,
                        server,
                    )
                    .await;
                });
            }
            Some(net_message::Message::VlanTeardown(vlan_teardown)) => {
                tokio::spawn(async move {
                    let _ = handle_vlan_teardown(
                        vlan_teardown,
                        rtnetlink_handle,
                        peers,
                        host_mappings_state,
                        server,
                    )
                    .await;
                });
            }
            Some(net_message::Message::VxlanSetup(vxlan_setup)) => {
                let triggers_state = triggers_state.clone();
                tokio::spawn(async move {
                    let _ = handle_vxlan_setup(
                        vxlan_setup,
                        outbound,
                        triggers_state,
                        host_mappings_state,
                        server,
                    )
                    .await;
                });
            }
            Some(net_message::Message::VxlanTeardown(vxlan_teardown)) => {
                let triggers_state = triggers_state.clone();
                tokio::spawn(async move {
                    handle_vxlan_teardown(
                        vxlan_teardown,
                        triggers_state,
                        host_mappings_state,
                        server,
                    );
                });
            }
            None => {}
        }
    }

    fire_event(
        &server,
        AgentEventKind::ControlChannelClosed(AgentControlChannelClosed {}),
    );

    Ok(())
}

async fn handle_vlan_setup(
    message: VlanSetup,
    rtnetlink_handle: RtNetLinkHandle,
    peers: Arc<RwLock<Peers>>,
    outbound: Sender<MsgId>,
    host_mappings_state: Arc<HostMappingsState>,
    grpc: NullnetGrpcInterface,
) -> Result<(), Error> {
    let msg_id = &message
        .msg_id
        .ok_or("Missing message ID in VLAN setup message")
        .handle_err(location!())?;
    let local_veth = message
        .local_veth
        .parse::<Ipv4Addr>()
        .handle_err(location!())?;
    let remote_ip = message
        .remote_ip
        .parse::<Ipv4Addr>()
        .handle_err(location!())?;
    let remote_veth = message
        .remote_veth
        .parse::<Ipv4Addr>()
        .handle_err(location!())?;
    let vlan_id = u16::try_from(message.vlan_id)
        .handle_err(location!())
        .inspect_err(|e| {
            fire_event(
                &grpc,
                AgentEventKind::VlanSetupFailed(AgentVlanSetupFailed {
                    vlan_id: message.vlan_id,
                    local_veth: local_veth.to_string(),
                    error_reason: e.to_str().to_string(),
                }),
            );
        })?;

    // setup VLAN on this machine
    let init_t = std::time::Instant::now();
    configure_access_port(
        &rtnetlink_handle,
        vlan_id,
        Ipv4Network::new(local_veth, 30).unwrap(),
    )
    .await;
    println!(
        "veth {local_veth} setup completed in {} ms",
        init_t.elapsed().as_millis()
    );

    // register peer
    peers
        .write()
        .await
        .insert(VethKey::new(remote_veth, vlan_id), remote_ip);

    // add host mapping if needed
    if let Some(host_mapping) = &message.host_mapping {
        if add_host_mapping(host_mapping, None).is_err() {
            fire_event(
                &grpc,
                AgentEventKind::HostMappingFailed(AgentHostMappingFailed {
                    hostname: host_mapping.name.clone(),
                    ip: host_mapping.ip.clone(),
                    docker_container: None,
                }),
            );
        }
        host_mappings_state.record_vlan(vlan_id, host_mapping.clone());
    }

    // acknowledge message
    if outbound.send(msg_id.clone()).await.is_err() {
        fire_event(
            &grpc,
            AgentEventKind::ControlChannelAckFailed(AgentControlChannelAckFailed {
                msg_id: msg_id.id.clone(),
                message_type: "vlan_setup".to_string(),
            }),
        );
    }

    fire_event(
        &grpc,
        AgentEventKind::VlanSetupCompleted(AgentVlanSetupCompleted {
            vlan_id: u32::from(vlan_id),
        }),
    );

    Ok(())
}

async fn handle_vlan_teardown(
    message: VlanTeardown,
    rtnetlink_handle: RtNetLinkHandle,
    peers: Arc<RwLock<Peers>>,
    host_mappings_state: Arc<HostMappingsState>,
    grpc: NullnetGrpcInterface,
) -> Result<(), Error> {
    let vlan_id = u16::try_from(message.vlan_id)
        .handle_err(location!())
        .inspect_err(|e| {
            fire_event(
                &grpc,
                AgentEventKind::VlanTeardownFailed(AgentVlanTeardownFailed {
                    vlan_id: message.vlan_id,
                    error_reason: e.to_str().to_string(),
                }),
            );
        })?;

    // teardown VLAN on this machine
    let init_t = std::time::Instant::now();

    remove_vlan(&rtnetlink_handle, vlan_id).await;

    println!(
        "VLAN teardown completed in {} ms",
        init_t.elapsed().as_millis()
    );

    // remove peer
    peers.write().await.remove(vlan_id);

    // remove host mapping if one was installed at setup
    if let Some(host_mapping) = host_mappings_state.take_vlan(vlan_id) {
        let _ = remove_host_mapping(&host_mapping, None);
    }

    Ok(())
}

async fn handle_vxlan_setup(
    message: VxlanSetup,
    outbound: Sender<MsgId>,
    triggers_state: Arc<TriggersState>,
    host_mappings_state: Arc<HostMappingsState>,
    grpc: NullnetGrpcInterface,
) -> Result<(), Error> {
    let msg_id = &message
        .msg_id
        .ok_or("Missing message ID in VXLAN setup message")
        .handle_err(location!())?;
    let vxlan_id = message.vxlan_id;
    let ns_name = message.ns_name;
    let ns_net = message
        .ns_net
        .parse::<Ipv4Network>()
        .handle_err(location!())?;
    let br_name = message.br_name;
    let br_net = message
        .br_net
        .parse::<Ipv4Network>()
        .handle_err(location!())?;
    let local_ip = message
        .local_ip
        .parse::<Ipv4Addr>()
        .handle_err(location!())?;
    let remote_ip = message
        .remote_ip
        .parse::<Ipv4Addr>()
        .handle_err(location!())?;

    // setup VXLAN on this machine (optionally attaching a Docker container)
    let init_t = std::time::Instant::now();
    let mut cmd = std::process::Command::new("./vxlan_scripts/vxlan-setup.sh");
    cmd.arg(vxlan_id.to_string())
        .arg(&ns_name)
        .arg(ns_net.to_string())
        .arg(br_name)
        .arg(br_net.to_string())
        .arg(local_ip.to_string())
        .arg(remote_ip.to_string());
    if let Some(container) = &message.docker_container {
        cmd.arg(container);
    }
    let script_result = cmd.spawn().and_then(|mut c| c.wait());
    let error_code = match &script_result {
        Ok(status) if !status.success() => status.code().unwrap_or(-1),
        Err(_) => -1,
        _ => 0,
    };
    if error_code != 0 {
        fire_event(
            &grpc,
            AgentEventKind::VxlanSetupFailed(AgentVxlanSetupFailed {
                vxlan_id,
                ns_name: ns_name.clone(),
                error_code,
            }),
        );
    }
    let _ = script_result.handle_err(location!());
    println!(
        "VXLAN {vxlan_id} setup completed in {} ms (docker: {})",
        init_t.elapsed().as_millis(),
        message.docker_container.as_deref().unwrap_or("none"),
    );

    // add host mapping if needed
    if let Some(host_mapping) = &message.host_mapping {
        if add_host_mapping(host_mapping, message.docker_container.as_deref()).is_err() {
            fire_event(
                &grpc,
                AgentEventKind::HostMappingFailed(AgentHostMappingFailed {
                    hostname: host_mapping.name.clone(),
                    ip: host_mapping.ip.clone(),
                    docker_container: message.docker_container.clone(),
                }),
            );
        }
        host_mappings_state.record_vxlan(
            vxlan_id,
            host_mapping.clone(),
            message.docker_container.clone(),
        );

        // backend-entry edge: install DNAT(dnat_port -> overlay_ip) so the
        // initiator's traffic on that local port is steered into the new
        // VXLAN.
        //
        // Order matters. The NFQUEUE listener is parked on a `Notify` that
        // `mark_active` fires; once woken it verdicts ACCEPT and the held
        // packet traverses `nat PREROUTING`. The DNAT rule MUST already be
        // installed by then, so we:
        //   1. peek the initiator's bridge IP (stashed at `mark_pending`)
        //   2. install DNAT with `-s <container_ip>`
        //   3. mark_active → wakes the waiter, packet released into the new
        //      rule
        if let Some(dnat_port) = message.dnat_port
            && let Ok(dnat_port) = u16::try_from(dnat_port)
            && let Ok(overlay_ip) = host_mapping.ip.parse::<Ipv4Addr>()
        {
            let container_key = message.docker_container.as_deref().unwrap_or("");
            let container_ip = triggers_state.peek_container_ip(container_key, dnat_port);
            dnat::install(dnat_port, overlay_ip, container_ip);
            triggers_state.mark_active(
                container_key,
                dnat_port,
                vxlan_id,
                overlay_ip,
                container_ip,
            );
        } else if message.dnat_port.is_some() {
            // Backend-entry edge with a malformed port or host IP — DNAT
            // can't be installed and the trigger waiter on this host will
            // block until `ACTIVE_TIMEOUT` then drop the held packet. Log
            // loudly and surface a structured event instead of silently
            // no-op'ing.
            eprintln!(
                "[vxlan_setup] backend entry malformed: dnat_port={:?}, host_mapping.ip={:?}; \
                 DNAT not installed, trigger waiter will time out",
                message.dnat_port, host_mapping.ip
            );
            fire_event(
                &grpc,
                AgentEventKind::DnatInstallFailed(AgentDnatInstallFailed {
                    port: message.dnat_port.unwrap_or(0),
                    overlay_ip: host_mapping.ip.clone(),
                }),
            );
        }
    }

    // acknowledge message
    if outbound.send(msg_id.clone()).await.is_err() {
        fire_event(
            &grpc,
            AgentEventKind::ControlChannelAckFailed(AgentControlChannelAckFailed {
                msg_id: msg_id.id.clone(),
                message_type: "vxlan_setup".to_string(),
            }),
        );
    }

    if error_code == 0 {
        fire_event(
            &grpc,
            AgentEventKind::VxlanSetupCompleted(AgentVxlanSetupCompleted { vxlan_id, ns_name }),
        );
    }

    Ok(())
}

fn handle_vxlan_teardown(
    message: VxlanTeardown,
    triggers_state: Arc<TriggersState>,
    host_mappings_state: Arc<HostMappingsState>,
    grpc: NullnetGrpcInterface,
) {
    // remove DNAT before tearing the tunnel down so existing flows reset
    // cleanly. The `container_ip` matches the `-s` we used at install time.
    if let Some((_container, port, overlay_ip, container_ip)) =
        triggers_state.remove_by_vxlan(message.vxlan_id)
    {
        dnat::remove(port, overlay_ip, container_ip);
    }

    // remove host mapping if one was installed at setup
    if let Some((host_mapping, docker_container)) = host_mappings_state.take_vxlan(message.vxlan_id)
    {
        let _ = remove_host_mapping(&host_mapping, docker_container.as_deref());
    }

    // teardown VXLAN on this machine
    let init_t = std::time::Instant::now();

    let vxlan_id = message.vxlan_id;
    let ns_name = message.ns_name.clone();
    let br_name = message.br_name;

    let mut cmd = std::process::Command::new("./vxlan_scripts/vxlan-teardown.sh");
    cmd.arg(vxlan_id.to_string()).arg(&ns_name).arg(&br_name);
    if let Some(container) = &message.docker_container {
        cmd.arg(container);
    }
    let script_result = cmd.spawn().and_then(|mut c| c.wait());
    let error_code = match &script_result {
        Ok(status) if !status.success() => status.code().unwrap_or(-1),
        Err(_) => -1,
        _ => 0,
    };
    if error_code != 0 {
        fire_event(
            &grpc,
            AgentEventKind::VxlanTeardownFailed(AgentVxlanTeardownFailed {
                vxlan_id,
                ns_name,
                error_code,
            }),
        );
    }
    let _ = script_result.handle_err(location!());

    println!(
        "VXLAN teardown completed in {} ms",
        init_t.elapsed().as_millis()
    );
}

fn add_host_mapping(hm: &HostMapping, docker_container: Option<&str>) -> Result<(), Error> {
    let path = "/etc/hosts";
    let entry = format!("{} {}", hm.ip, hm.name);

    if let Some(container) = docker_container {
        // container-targeted: the resolver that needs this name lives inside
        // the container, so write only there and leave the host's file alone.
        let cat = std::process::Command::new("docker")
            .args(["exec", container, "cat", path])
            .output()
            .handle_err(location!())?;
        let content = upsert_hosts_entry(&String::from_utf8_lossy(&cat.stdout), &hm.name, &entry);
        let mut child = std::process::Command::new("docker")
            .args([
                "exec",
                "-i",
                container,
                "sh",
                "-c",
                &format!("cat > {path}"),
            ])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .handle_err(location!())?;
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            stdin
                .write_all(content.as_bytes())
                .handle_err(location!())?;
        }
        let _ = child.wait();
    } else {
        // host-targeted: upsert into the host's /etc/hosts
        let content = std::fs::read_to_string(path).handle_err(location!())?;
        std::fs::write(path, upsert_hosts_entry(&content, &hm.name, &entry))
            .handle_err(location!())?;
    }

    Ok(())
}

fn upsert_hosts_entry(content: &str, name: &str, entry: &str) -> String {
    let mut lines: Vec<String> = content.lines().map(ToString::to_string).collect();
    let mut found = false;
    for line in &mut lines {
        if line.split_whitespace().skip(1).any(|tok| tok == name) {
            *line = entry.to_string();
            found = true;
        }
    }
    if !found {
        lines.push(entry.to_string());
    }
    lines.join("\n") + "\n"
}

fn remove_host_mapping(hm: &HostMapping, docker_container: Option<&str>) -> Result<(), Error> {
    let path = "/etc/hosts";

    if let Some(container) = docker_container {
        // container-targeted: setup only wrote inside the container, so the
        // matching removal is container-only too.
        let cat = std::process::Command::new("docker")
            .args(["exec", container, "cat", path])
            .output()
            .handle_err(location!())?;
        let content = remove_hosts_entry(&String::from_utf8_lossy(&cat.stdout), &hm.name);
        let mut child = std::process::Command::new("docker")
            .args([
                "exec",
                "-i",
                container,
                "sh",
                "-c",
                &format!("cat > {path}"),
            ])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .handle_err(location!())?;
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            stdin
                .write_all(content.as_bytes())
                .handle_err(location!())?;
        }
        let _ = child.wait();
    } else {
        // host-targeted: drop any line in the host file referencing this name
        let content = std::fs::read_to_string(path).handle_err(location!())?;
        std::fs::write(path, remove_hosts_entry(&content, &hm.name)).handle_err(location!())?;
    }

    Ok(())
}

fn remove_hosts_entry(content: &str, name: &str) -> String {
    let lines: Vec<String> = content
        .lines()
        .filter(|line| !line.split_whitespace().skip(1).any(|tok| tok == name))
        .map(ToString::to_string)
        .collect();
    lines.join("\n") + "\n"
}
