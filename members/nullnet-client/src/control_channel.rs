use crate::commands::{RtNetLinkHandle, configure_access_port, dnat, egress, remove_vlan};
use crate::ebpf::{FirewallPeers, FirewallVxlanPorts, NetId};
use crate::egress_policy::{PolicyVerdicts, flush_container_conntrack};
use crate::egress_state::{EgressRecord, EgressState};
use crate::host_mappings::{self, HOSTS_MARKER, HostMappingsState, hosts_file_lock};
use crate::nfqueue::BridgeIpCache;
use crate::peers::peer::{Peers, VethKey};
use crate::triggers::TriggersState;
use ipnetwork::Ipv4Network;
use nullnet_grpc_lib::NullnetGrpcInterface;
use nullnet_grpc_lib::nullnet_grpc::{
    AgentContainerResumeFailed, AgentContainerSuspendFailed, AgentControlChannelAckFailed,
    AgentControlChannelClosed, AgentControlChannelEstablished, AgentDnatInstallFailed,
    AgentDnatRemovalFailed, AgentGatewayForwardInstallFailed, AgentHostMappingFailed,
    AgentVlanSetupCompleted, AgentVlanSetupFailed, AgentVlanTeardownFailed,
    AgentVxlanSetupCompleted, AgentVxlanSetupFailed, AgentVxlanTeardownFailed,
};
use nullnet_grpc_lib::nullnet_grpc::{
    AgentEvent, ContainerResume, ContainerSuspend, HostMapping, MsgId, VlanSetup, VlanTeardown,
    VxlanSetup, VxlanTeardown, agent_event::Event as AgentEventKind, net_message,
};
use nullnet_liberror::{Error, ErrorHandler, Location, location};
use std::net::Ipv4Addr;
use std::sync::{Arc, PoisonError};
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

/// Confirm a completed teardown so the server can return the net id to its
/// pool. Must be called only once the teardown has actually run — the whole
/// point of the ack is that the id stays out of circulation until this edge's
/// kernel state is gone.
///
/// `msg_id` is absent when the server predates the ack field; there is then
/// nothing to confirm and the server falls back to its grace timer.
async fn ack_teardown(
    outbound: &Sender<MsgId>,
    msg_id: Option<MsgId>,
    grpc: &NullnetGrpcInterface,
    message_type: &str,
) {
    let Some(msg_id) = msg_id else {
        return;
    };
    if outbound.send(msg_id.clone()).await.is_err() {
        fire_event(
            grpc,
            AgentEventKind::ControlChannelAckFailed(AgentControlChannelAckFailed {
                msg_id: msg_id.id,
                message_type: message_type.to_string(),
            }),
        );
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn control_channel(
    server: NullnetGrpcInterface,
    peers: Arc<RwLock<Peers>>,
    rtnetlink_handle: RtNetLinkHandle,
    triggers_state: Arc<TriggersState>,
    host_mappings_state: Arc<HostMappingsState>,
    firewall_peers: Arc<FirewallPeers>,
    firewall_vxlan_ports: Arc<FirewallVxlanPorts>,
    egress_state: Arc<EgressState>,
    bridge_cache: BridgeIpCache,
    policy_verdicts: Arc<PolicyVerdicts>,
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
        let firewall_peers = firewall_peers.clone();
        let firewall_vxlan_ports = firewall_vxlan_ports.clone();
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
                        firewall_peers,
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
                        outbound,
                        host_mappings_state,
                        server,
                        firewall_peers,
                    )
                    .await;
                });
            }
            Some(net_message::Message::VxlanSetup(vxlan_setup)) => {
                let triggers_state = triggers_state.clone();
                let egress_state = egress_state.clone();
                tokio::spawn(async move {
                    let _ = handle_vxlan_setup(
                        vxlan_setup,
                        outbound,
                        triggers_state,
                        host_mappings_state,
                        server,
                        firewall_peers,
                        firewall_vxlan_ports,
                        egress_state,
                    )
                    .await;
                });
            }
            Some(net_message::Message::VxlanTeardown(vxlan_teardown)) => {
                let triggers_state = triggers_state.clone();
                let egress_state = egress_state.clone();
                tokio::spawn(async move {
                    handle_vxlan_teardown(
                        vxlan_teardown,
                        triggers_state,
                        outbound,
                        host_mappings_state,
                        server,
                        firewall_peers,
                        firewall_vxlan_ports,
                        egress_state,
                    )
                    .await;
                });
            }
            Some(net_message::Message::ContainerSuspend(container_suspend)) => {
                tokio::spawn(async move {
                    handle_container_suspend(container_suspend, server);
                });
            }
            Some(net_message::Message::ContainerResume(container_resume)) => {
                tokio::spawn(async move {
                    let _ = handle_container_resume(container_resume, outbound, server).await;
                });
            }
            Some(net_message::Message::EgressPolicyChanged(_)) => {
                // Re-verdict everything: drop cached verdicts, then flush
                // conntrack for the containers' flows so live connections
                // re-enter the NFQUEUE as NEW — newly-denied ones die there.
                let verdicts = policy_verdicts.clone();
                let cache = bridge_cache.clone();
                tokio::spawn(async move {
                    println!("[egress-policy] policy changed on server; re-verdicting flows");
                    verdicts.clear();
                    flush_container_conntrack(cache.ips()).await;
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
    firewall_peers: Arc<FirewallPeers>,
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
    // Fail before touching the network if the key is malformed — running
    // this tunnel without a valid key would mean forwarding traffic in the
    // clear instead of encrypted. Skipped entirely when the server had
    // encryption disabled (ENCRYPTION_ENABLED=false) — `encryption_key` is
    // just placeholder zero bytes in that case, not a real key to validate.
    let encryption_key: Option<[u8; 32]> = if message.encrypted {
        Some(
            message
                .encryption_key
                .try_into()
                .map_err(|_| "VLAN setup message carried a malformed encryption key")
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
                })?,
        )
    } else {
        None
    };

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

    // register peer + this tunnel's encryption state
    {
        let mut peers = peers.write().await;
        peers.insert(VethKey::new(remote_veth, vlan_id), remote_ip);
        match encryption_key {
            Some(key) => peers.insert_key(vlan_id, &key),
            None => peers.insert_plaintext(vlan_id),
        }
    }

    // allow this peer's data-plane traffic through the host firewall
    firewall_peers.add(NetId::Vlan(vlan_id), remote_ip);

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
    outbound: Sender<MsgId>,
    host_mappings_state: Arc<HostMappingsState>,
    grpc: NullnetGrpcInterface,
    firewall_peers: Arc<FirewallPeers>,
) -> Result<(), Error> {
    let ack_id = message.msg_id.clone();
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

    // drop this peer's firewall allowance (refcounted; only removed if unused)
    firewall_peers.remove(NetId::Vlan(vlan_id));

    // remove host mapping if one was installed at setup
    if let Some(host_mapping) = host_mappings_state.take_vlan(vlan_id) {
        let _ = remove_host_mapping(&host_mapping, None);
    }

    // Acked last: the server frees the net id on this, so everything above must
    // already be undone.
    ack_teardown(&outbound, ack_id, &grpc, "vlan_teardown").await;

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn handle_vxlan_setup(
    message: VxlanSetup,
    outbound: Sender<MsgId>,
    triggers_state: Arc<TriggersState>,
    host_mappings_state: Arc<HostMappingsState>,
    grpc: NullnetGrpcInterface,
    firewall_peers: Arc<FirewallPeers>,
    firewall_vxlan_ports: Arc<FirewallVxlanPorts>,
    egress_state: Arc<EgressState>,
) -> Result<(), Error> {
    let egress_steer = message.egress_steer.unwrap_or(false);
    let egress_intercept = message.egress_intercept.unwrap_or(false);
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
    // Fail before touching the network if the key is malformed — running
    // this tunnel without a valid key would mean forwarding traffic in the
    // clear instead of encrypted. Skipped entirely when the server had
    // encryption disabled (ENCRYPTION_ENABLED=false) — `encryption_key` is
    // just placeholder zero bytes in that case, and vxlan-setup.sh ignores
    // it (see the `encrypted` arg passed below).
    let encryption_key: [u8; 32] = if message.encrypted {
        message
            .encryption_key
            .try_into()
            .map_err(|_| "VXLAN setup message carried a malformed encryption key")
            .handle_err(location!())
            .inspect_err(|e| {
                fire_event(
                    &grpc,
                    AgentEventKind::VxlanSetupFailed(AgentVxlanSetupFailed {
                        vxlan_id,
                        ns_name: ns_name.clone(),
                        error_code: -1,
                    }),
                );
                eprintln!("[vxlan_setup] {}", e.to_str());
            })?
    } else {
        [0u8; 32]
    };

    // allow this peer's data-plane (VXLAN underlay) traffic through the host
    // firewall before the tunnel comes up, so the first packets aren't dropped.
    firewall_peers.add(NetId::Vxlan(vxlan_id), remote_ip);

    // also allow this tunnel's own per-tunnel dstport: the eBPF firewall's
    // static data-plane check only knows the fixed 4789/9999 ports, not the
    // dynamically-allocated dstport each VXLAN tunnel gets (see
    // nullnet-server's `UdpPortPool`), so without this every packet on a real
    // tunnel — cross-host VXLAN encapsulation, including the overlay ARP that
    // has to succeed before any TCP connection can be routed — is silently
    // dropped at this host's NIC. Paired with `remote_ip` specifically, so a
    // different concurrent tunnel's peer can't satisfy this port.
    //
    // Skipped for the shared default port (same-host or unencrypted edges —
    // see nullnet-server's DEFAULT_VXLAN_DSTPORT doc comment): that port is
    // legitimately reused by many concurrent tunnels at once, so it can't be
    // paired to one specific peer here — it's already allowed via the eBPF
    // firewall's own VXLAN_PORT constant check instead (any known peer).
    if let Ok(dstport) = u16::try_from(message.dstport)
        && dstport != crate::DEFAULT_VXLAN_DSTPORT
    {
        firewall_vxlan_ports.add(vxlan_id, dstport, remote_ip);
    }

    // setup VXLAN on this machine (optionally attaching a Docker container)
    let init_t = std::time::Instant::now();
    let mut cmd = std::process::Command::new("./vxlan_scripts/vxlan-setup.sh");
    cmd.arg(vxlan_id.to_string())
        .arg(&ns_name)
        .arg(ns_net.to_string())
        .arg(&br_name)
        .arg(br_net.to_string())
        .arg(local_ip.to_string())
        .arg(remote_ip.to_string())
        .arg(hex_encode(&encryption_key))
        .arg(message.dstport.to_string())
        .arg(if message.encrypted { "true" } else { "false" });
    // Egress-steer edges keep their tunnel endpoint in the host root namespace
    // (no `docker_container` arg) so the initiator container's *forwarded*
    // external traffic can be policy-routed into the bridge. Other edges attach
    // the endpoint to the container as before.
    if !egress_steer && let Some(container) = &message.docker_container {
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

    // Egress edges install steering (initiator) or interception (proxy) instead
    // of the host-mapping + DNAT path used by proxy/backend edges.
    if egress_steer {
        // Proxy overlay IP (gateway) is carried in host_mapping.ip; our own
        // overlay bridge IP is the SNAT source; the container IP scopes the
        // source-based routing.
        let proxy_gw = message
            .host_mapping
            .as_ref()
            .and_then(|hm| hm.ip.parse::<Ipv4Addr>().ok());
        let snat_src = br_net.ip();
        let container_ip = message
            .docker_container
            .as_deref()
            .and_then(egress::container_ipv4);
        match (proxy_gw, container_ip) {
            (Some(gw), Some(cip)) => {
                if egress::install_steer(vxlan_id, &br_name, gw, snat_src, cip) {
                    egress_state.record(
                        vxlan_id,
                        EgressRecord::Steer {
                            br_name: br_name.clone(),
                            snat_src,
                            container_ip: cip,
                        },
                    );
                    // Steering is live — release any egress SYN the NFQUEUE
                    // listener is holding for this initiator. Mirrors the
                    // backend DNAT→mark_active ordering (install first, then
                    // wake, so the freed packet finds the rule in place).
                    if let Some(container) = message.docker_container.as_deref() {
                        triggers_state.mark_active(
                            container,
                            crate::triggers::EGRESS_DST_IP,
                            crate::triggers::EGRESS_TRIGGER_PORT,
                            vxlan_id,
                            gw,
                            cip,
                        );
                    }
                }
            }
            _ => {
                eprintln!(
                    "[vxlan_setup] egress steer missing gateway/container_ip \
                     (gw={proxy_gw:?}, cip={container_ip:?}); steering not installed"
                );
            }
        }
    } else if egress_intercept {
        if egress::install_gateway_forward(&br_name, &message.br_net) {
            egress_state.record(
                vxlan_id,
                EgressRecord::Gateway {
                    br_name: br_name.clone(),
                    br_net: message.br_net.clone(),
                },
            );
        } else {
            fire_event(
                &grpc,
                AgentEventKind::GatewayForwardInstallFailed(AgentGatewayForwardInstallFailed {
                    vxlan_id,
                    br_net: message.br_net.clone(),
                }),
            );
        }
    } else if let Some(host_mapping) = &message.host_mapping {
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
        //   2. install DNAT with `-s <container_ip> -d <placeholder_ip>`
        //   3. mark_active → wakes the waiter, packet released into the new
        //      rule
        if let Some(dnat_port) = message.dnat_port
            && let Ok(dnat_port) = u16::try_from(dnat_port)
            && let Ok(overlay_ip) = host_mapping.ip.parse::<Ipv4Addr>()
        {
            let container_key = message.docker_container.as_deref().unwrap_or("");
            // `host_mapping.name` is chain[0] for this trigger — the same
            // literal name the NFQUEUE listener read from its port→target
            // map when it observed the original packet — so recomputing the
            // placeholder here independently lands on the exact same
            // address it used, without needing it on the wire separately.
            let dest_ip = crate::placeholder::ip_for(&host_mapping.name);
            let container_ip = triggers_state.peek_container_ip(container_key, dest_ip, dnat_port);
            // Only promote to Active if the DNAT rule is actually live. Waking
            // the held packet without it would release the SYN into a missing
            // rule (→ misroute to the original dest); instead leave it Pending
            // so the listener drops at ACTIVE_TIMEOUT.
            if dnat::install(dnat_port, overlay_ip, container_ip, dest_ip) {
                triggers_state.mark_active(
                    container_key,
                    dest_ip,
                    dnat_port,
                    vxlan_id,
                    overlay_ip,
                    container_ip,
                );
            } else {
                fire_event(
                    &grpc,
                    AgentEventKind::DnatInstallFailed(AgentDnatInstallFailed {
                        port: u32::from(dnat_port),
                        overlay_ip: overlay_ip.to_string(),
                    }),
                );
            }
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

#[allow(clippy::too_many_arguments)]
async fn handle_vxlan_teardown(
    message: VxlanTeardown,
    triggers_state: Arc<TriggersState>,
    outbound: Sender<MsgId>,
    host_mappings_state: Arc<HostMappingsState>,
    grpc: NullnetGrpcInterface,
    firewall_peers: Arc<FirewallPeers>,
    firewall_vxlan_ports: Arc<FirewallVxlanPorts>,
    egress_state: Arc<EgressState>,
) {
    let ack_id = message.msg_id.clone();
    // reverse egress steering/interception if this was an egress edge
    if let Some(rec) = egress_state.take(message.vxlan_id) {
        match rec {
            EgressRecord::Steer {
                br_name,
                snat_src,
                container_ip,
            } => egress::remove_steer(message.vxlan_id, &br_name, snat_src, container_ip),
            EgressRecord::Gateway { br_name, br_net } => {
                egress::remove_gateway_forward(&br_name, &br_net)
            }
        }
    }

    // drop this peer's firewall allowance (refcounted; only removed if unused)
    firewall_peers.remove(NetId::Vxlan(message.vxlan_id));

    // drop this tunnel's per-tunnel dstport allowance
    firewall_vxlan_ports.remove(message.vxlan_id);

    // remove DNAT before tearing the tunnel down so existing flows reset
    // cleanly. The `container_ip`/`dst_ip` match the `-s`/`-d` we used at
    // install time. `remove_by_vxlan` also matches the egress steer's
    // sentinel-port entry (EGRESS_TRIGGER_PORT = 0). That path installs a
    // policy-route steer, not a DNAT — its teardown runs via EgressState
    // below — so skip DNAT removal for it. Only real backend DNAT ports (>0)
    // go through `dnat::remove`; otherwise we'd fire a bogus
    // `iptables -D --dport 0` and a false removal-failed event. Whether this
    // was a backend-trigger entry (vs. an egress steer) also decides how the
    // host mapping below gets torn down.
    let mut is_backend_trigger_entry = false;
    if let Some((_container, dst_ip, port, overlay_ip, container_ip)) =
        triggers_state.remove_by_vxlan(message.vxlan_id)
        && port != crate::triggers::EGRESS_TRIGGER_PORT
    {
        is_backend_trigger_entry = true;
        if !dnat::remove(port, overlay_ip, container_ip, dst_ip) {
            fire_event(
                &grpc,
                AgentEventKind::DnatRemovalFailed(AgentDnatRemovalFailed {
                    port: u32::from(port),
                    overlay_ip: overlay_ip.to_string(),
                }),
            );
        }
    }

    // Remove the host mapping installed at setup — except for a
    // backend-trigger entry, where we re-seed the placeholder instead of
    // deleting the line outright. Deleting would leave the initiator unable
    // to resolve the name at all: the next `connect(name, ...)` would
    // dead-end exactly like an un-seeded bare name does (no packet, no
    // re-trigger, chain never rebuilds). Proxy-dependency mappings are
    // unaffected — a fresh proxy request rebuilds the chain (and writes the
    // real mapping) before forwarding, so there's no gap to cover there.
    if let Some((host_mapping, docker_container)) = host_mappings_state.take_vxlan(message.vxlan_id)
    {
        if is_backend_trigger_entry && let Some(container) = docker_container.as_deref() {
            if let Err(e) = crate::placeholder::seed_placeholder(container, &host_mapping.name) {
                eprintln!(
                    "[control_channel] failed to re-seed placeholder for '{}' in {container}: {e:?}",
                    host_mapping.name
                );
            }
        } else {
            let _ = remove_host_mapping(&host_mapping, docker_container.as_deref());
        }
    }

    // teardown VXLAN on this machine
    let init_t = std::time::Instant::now();

    let vxlan_id = message.vxlan_id;
    let ns_name = message.ns_name.clone();
    let br_name = message.br_name;

    let mut cmd = std::process::Command::new("./vxlan_scripts/vxlan-teardown.sh");
    cmd.arg(vxlan_id.to_string())
        .arg(&ns_name)
        .arg(&br_name)
        .arg(&message.local_ip)
        .arg(&message.remote_ip)
        .arg(message.dstport.to_string());
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

    // Acked last: the server frees the net id on this, so every kernel object
    // named after it — bridge, veth/macsec pair, XFRM SA, DNAT — must already
    // be gone.
    ack_teardown(&outbound, ack_id, &grpc, "vxlan_teardown").await;
}

/// Pause an idle container. Fire-and-forget: the server marks the replica
/// suspended optimistically, so we only report a structured event on failure.
/// An already-paused container is treated as success (e.g. after a server
/// restart re-issues the command).
fn handle_container_suspend(message: ContainerSuspend, grpc: NullnetGrpcInterface) {
    let container = message.docker_container;
    let init_t = std::time::Instant::now();
    match docker_action("pause", &container, "is already paused") {
        Ok(()) => println!(
            "Paused container '{container}' in {} ms",
            init_t.elapsed().as_millis()
        ),
        Err(error_message) => fire_event(
            &grpc,
            AgentEventKind::ContainerSuspendFailed(AgentContainerSuspendFailed {
                docker_container: container,
                error_message,
            }),
        ),
    }
}

/// Resume a suspended container, confirm it is actually running, then ack so the
/// server only returns the upstream to the proxy once the service is serving.
/// On any failure we report an event and deliberately do NOT ack — the server's
/// resume wait then times out and surfaces the failure to the proxy.
async fn handle_container_resume(
    message: ContainerResume,
    outbound: Sender<MsgId>,
    grpc: NullnetGrpcInterface,
) -> Result<(), Error> {
    let msg_id = message
        .msg_id
        .ok_or("Missing message ID in container resume message")
        .handle_err(location!())?;
    let container = message.docker_container;
    let init_t = std::time::Instant::now();

    // unpause (tolerate an already-running container)
    if let Err(error_message) = docker_action("unpause", &container, "is not paused") {
        fire_event(
            &grpc,
            AgentEventKind::ContainerResumeFailed(AgentContainerResumeFailed {
                docker_container: container,
                error_message,
            }),
        );
        return Ok(());
    }

    // confirm it is running and not paused before acking; unpause preserves the
    // listening socket, so a running container is immediately serving.
    if !container_running(&container) {
        fire_event(
            &grpc,
            AgentEventKind::ContainerResumeFailed(AgentContainerResumeFailed {
                docker_container: container,
                error_message: "container not running after unpause".to_string(),
            }),
        );
        return Ok(());
    }

    println!(
        "Resumed container '{container}' in {} ms",
        init_t.elapsed().as_millis()
    );

    // acknowledge — tells the server the service is serving again
    if outbound.send(msg_id.clone()).await.is_err() {
        fire_event(
            &grpc,
            AgentEventKind::ControlChannelAckFailed(AgentControlChannelAckFailed {
                msg_id: msg_id.id.clone(),
                message_type: "container_resume".to_string(),
            }),
        );
    }

    Ok(())
}

/// Run `docker <action> <container>`, treating a non-zero exit whose stderr
/// contains `benign` (the container is already in the target state) as success.
fn docker_action(action: &str, container: &str, benign: &str) -> Result<(), String> {
    match std::process::Command::new("docker")
        .args([action, container])
        .output()
    {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            if stderr.contains(benign) {
                Ok(())
            } else {
                Err(stderr.trim().to_string())
            }
        }
        Err(e) => Err(e.to_string()),
    }
}

/// True when the container is running and not paused.
fn container_running(container: &str) -> bool {
    match std::process::Command::new("docker")
        .args([
            "inspect",
            "-f",
            "{{.State.Running}} {{.State.Paused}}",
            container,
        ])
        .output()
    {
        Ok(out) if out.status.success() => {
            String::from_utf8_lossy(&out.stdout).trim() == "true false"
        }
        _ => false,
    }
}

fn add_host_mapping(hm: &HostMapping, docker_container: Option<&str>) -> Result<(), Error> {
    let path = "/etc/hosts";
    // Marked so a restarted process can sweep its predecessor's entries without
    // touching the operator's (see `host_mappings::purge_stale_mappings`).
    let entry = format!("{} {} {HOSTS_MARKER}", hm.ip, hm.name);

    // Serialize against every other mapping change on this same file; the
    // read-modify-write below is only atomic while this is held.
    let lock = hosts_file_lock(docker_container);
    let _guard = lock.lock().unwrap_or_else(PoisonError::into_inner);

    if let Some(container) = docker_container {
        // container-targeted: the resolver that needs this name lives inside
        // the container, so write only there and leave the host's file alone.
        // Edited from the host side (see `host_mappings::edit_container_hosts`)
        // so a paused container is served just as well as a running one.
        //
        // Bail rather than write on a failed read: treating a missing file as
        // empty content would truncate a live `/etc/hosts` down to this one
        // entry.
        host_mappings::edit_container_hosts(container, |current| {
            host_mappings::upsert_hosts_entry(current, &hm.name, &entry)
        })
        .handle_err(location!())?;
    } else {
        // host-targeted: upsert into the host's /etc/hosts
        let content = std::fs::read_to_string(path).handle_err(location!())?;
        std::fs::write(
            path,
            host_mappings::upsert_hosts_entry(&content, &hm.name, &entry),
        )
        .handle_err(location!())?;
    }

    Ok(())
}

fn remove_host_mapping(hm: &HostMapping, docker_container: Option<&str>) -> Result<(), Error> {
    let path = "/etc/hosts";

    let lock = hosts_file_lock(docker_container);
    let _guard = lock.lock().unwrap_or_else(PoisonError::into_inner);

    if let Some(container) = docker_container {
        // container-targeted: setup only wrote inside the container, so the
        // matching removal is container-only too. Host-side, so this still
        // works when the container has already been paused — the teardown and
        // the `docker pause` that follows it race, and losing that race used to
        // strand the entry, leaving the name pointing at a dead overlay IP for
        // as long as the container kept being resumed rather than restarted.
        host_mappings::edit_container_hosts(container, |current| {
            host_mappings::remove_hosts_entry(current, &hm.name, &hm.ip)
        })
        .handle_err(location!())?;
    } else {
        // host-targeted: drop this net's line from the host file
        let content = std::fs::read_to_string(path).handle_err(location!())?;
        std::fs::write(
            path,
            host_mappings::remove_hosts_entry(&content, &hm.name, &hm.ip),
        )
        .handle_err(location!())?;
    }

    Ok(())
}

/// Lowercase hex encoding, used to pass the tunnel's AES key to
/// `vxlan-setup.sh`/`vxlan-teardown.sh` as a shell argument.
fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
