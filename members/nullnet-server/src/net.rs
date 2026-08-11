use crate::net_id_pool::DEFAULT_VXLAN_DSTPORT;
use ipnetwork::Ipv4Network;
use nullnet_grpc_lib::nullnet_grpc::{
    HostMapping, MsgId, Net, NetMessage, VlanSetup, VlanTeardown, VxlanSetup, VxlanTeardown,
    net_message,
};
use nullnet_liberror::{ErrorHandler, Location, location};
use std::net::{IpAddr, Ipv4Addr};

/// Per-edge role for the egress forward-proxy feature. Only meaningful on the
/// single edge of an egress edge (VXLAN only); ignored for VLAN.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum EgressRole {
    /// Not an egress edge (backend/proxy edges).
    #[default]
    None,
    /// Initiator (client) side: policy-route + SNAT external traffic into the
    /// tunnel toward the proxy overlay IP, destination preserved (no DNAT).
    Steer,
    /// Gateway (server) side: forward tunnelled external packets to the internet
    /// (kernel ip_forward + MASQUERADE), the Cilium egress-gateway model.
    Intercept,
}

pub(crate) trait NetExt {
    #[allow(clippy::too_many_arguments)]
    fn setup(
        self,
        msg_id: String,
        dest: IpAddr,
        remote_server_name: Option<String>,
        net_id: u32,
        remote: IpAddr,
        docker_containers: (Option<String>, Option<String>),
        dnat_port: Option<u32>,
        encryption_key: [u8; 32],
        dstport: Option<u32>,
        encrypted: bool,
        egress: EgressRole,
    ) -> Option<(Ipv4Addr, NetMessage)>;

    #[allow(clippy::too_many_arguments)]
    fn teardown(
        self,
        net_id: u32,
        side: &str,
        docker_container: Option<String>,
        local_ip: IpAddr,
        remote_ip: IpAddr,
        dstport: Option<u16>,
        msg_id: String,
    ) -> NetMessage;
}

impl NetExt for Net {
    fn setup(
        self,
        msg_id: String,
        dest: IpAddr,
        remote_server_name: Option<String>,
        net_id: u32,
        remote: IpAddr,
        docker_containers: (Option<String>, Option<String>),
        dnat_port: Option<u32>,
        encryption_key: [u8; 32],
        dstport: Option<u32>,
        encrypted: bool,
        egress: EgressRole,
    ) -> Option<(Ipv4Addr, NetMessage)> {
        match self {
            Net::Vlan => vlan_setup(
                msg_id,
                dest,
                remote_server_name,
                net_id,
                remote,
                encryption_key,
                encrypted,
            ),
            Net::Vxlan => vxlan_setup(
                msg_id,
                dest,
                remote_server_name,
                net_id,
                remote,
                docker_containers,
                dnat_port,
                encryption_key,
                dstport.unwrap_or(u32::from(DEFAULT_VXLAN_DSTPORT)),
                encrypted,
                egress,
            ),
        }
    }

    fn teardown(
        self,
        net_id: u32,
        side: &str,
        docker_container: Option<String>,
        local_ip: IpAddr,
        remote_ip: IpAddr,
        dstport: Option<u16>,
        msg_id: String,
    ) -> NetMessage {
        let msg_id = Some(MsgId { id: msg_id });
        match self {
            Net::Vlan => NetMessage {
                message: Some(net_message::Message::VlanTeardown(VlanTeardown {
                    vlan_id: net_id,
                    msg_id,
                })),
            },
            Net::Vxlan => NetMessage {
                message: Some(net_message::Message::VxlanTeardown(VxlanTeardown {
                    vxlan_id: net_id,
                    ns_name: format!("ns_{net_id}_{side}"),
                    br_name: format!("br_{net_id}_{side}"),
                    docker_container,
                    local_ip: local_ip.to_string(),
                    remote_ip: remote_ip.to_string(),
                    dstport: u32::from(dstport.unwrap_or(DEFAULT_VXLAN_DSTPORT)),
                    msg_id,
                })),
            },
        }
    }
}

#[allow(clippy::unnecessary_wraps)]
fn vlan_setup(
    msg_id: String,
    dest: IpAddr,
    remote_server_name: Option<String>,
    vlan_id: u32,
    remote: IpAddr,
    encryption_key: [u8; 32],
    encrypted: bool,
) -> Option<(Ipv4Addr, NetMessage)> {
    // Map vlan_id to a /30 block within 10.0.0.0/8.
    // Each ID gets 4 IPs (2 usable), with 2 IPs used for server/client veth.
    let offset = vlan_id * 4;
    let [_, a, b, c] = offset.to_be_bytes();

    let server_veth = Ipv4Addr::new(10, a, b, c + 1);
    let client_veth = Ipv4Addr::new(10, a, b, c + 2);

    let (local_veth, remote_veth) = if remote_server_name.is_some() {
        // this is for client
        (client_veth, server_veth)
    } else {
        // this is for server
        (server_veth, client_veth)
    };

    let host_mapping = remote_server_name.map(|name| HostMapping {
        ip: server_veth.to_string(),
        name,
    });

    Some((
        local_veth,
        NetMessage {
            message: Some(net_message::Message::VlanSetup(VlanSetup {
                msg_id: Some(MsgId { id: msg_id }),
                vlan_id,
                local_veth: local_veth.to_string(),
                remote_veth: remote_veth.to_string(),
                local_ip: dest.to_string(),
                remote_ip: remote.to_string(),
                host_mapping,
                encryption_key: encryption_key.to_vec(),
                encrypted,
            })),
        },
    ))
}

#[allow(clippy::too_many_arguments)]
fn vxlan_setup(
    msg_id: String,
    dest: IpAddr,
    remote_server_name: Option<String>,
    vxlan_id: u32,
    remote: IpAddr,
    docker_containers: (Option<String>, Option<String>),
    dnat_port: Option<u32>,
    encryption_key: [u8; 32],
    dstport: u32,
    encrypted: bool,
    egress: EgressRole,
) -> Option<(Ipv4Addr, NetMessage)> {
    // Map vxlan_id to a /29 block within 10.0.0.0/8.
    // Each ID gets 8 IPs (6 usable), with 4 IPs used for ns/br server/client.
    let offset = vxlan_id * 8;
    let [_, a, b, c] = offset.to_be_bytes();

    let client_docker = docker_containers.0;
    let server_docker = docker_containers.1;
    let is_server_docker = server_docker.is_some();

    let ns_net_server = Ipv4Network::new(Ipv4Addr::new(10, a, b, c + 1), 29)
        .handle_err(location!())
        .ok()?;
    let br_net_server = Ipv4Network::new(Ipv4Addr::new(10, a, b, c + 2), 29)
        .handle_err(location!())
        .ok()?;

    let (ns_net, br_net, docker_container, side) = if remote_server_name.is_some() {
        // this is for client
        let ns_net_client = Ipv4Network::new(Ipv4Addr::new(10, a, b, c + 3), 29)
            .handle_err(location!())
            .ok()?;
        let br_net_client = Ipv4Network::new(Ipv4Addr::new(10, a, b, c + 4), 29)
            .handle_err(location!())
            .ok()?;
        (ns_net_client, br_net_client, client_docker, "c")
    } else {
        // this is for server
        (ns_net_server, br_net_server, server_docker, "s")
    };

    let server_net_ip = if is_server_docker {
        ns_net_server.ip()
    } else {
        br_net_server.ip()
    };

    // The address this call's own side should report as its tunnel IP, as
    // opposed to server_net_ip above which is always the server's (used only
    // for host_mapping). ns_net/br_net/docker_container are already the
    // per-side values picked by the branch above.
    let local_net_ip = if docker_container.is_some() {
        ns_net.ip()
    } else {
        br_net.ip()
    };

    let host_mapping = remote_server_name.map(|name| HostMapping {
        ip: server_net_ip.to_string(),
        name,
    });

    // Egress markers: Steer on the initiator (client) edge, Intercept on the
    // proxy (server) edge. Mutually exclusive; both None for non-egress edges.
    let (egress_steer, egress_intercept) = match egress {
        EgressRole::None => (None, None),
        EgressRole::Steer => (Some(true), None),
        EgressRole::Intercept => (None, Some(true)),
    };

    Some((
        local_net_ip,
        NetMessage {
            message: Some(net_message::Message::VxlanSetup(VxlanSetup {
                msg_id: Some(MsgId { id: msg_id }),
                vxlan_id,
                ns_name: format!("ns_{vxlan_id}_{side}"),
                ns_net: ns_net.to_string(),
                br_name: format!("br_{vxlan_id}_{side}"),
                br_net: br_net.to_string(),
                local_ip: dest.to_string(),
                remote_ip: remote.to_string(),
                host_mapping,
                docker_container,
                dnat_port,
                encryption_key: encryption_key.to_vec(),
                dstport,
                egress_steer,
                egress_intercept,
                encrypted,
            })),
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Regression test for a bug where the client-side and server-side setup
    // calls returned the same address, making session client/server IPs
    // appear equal in the UI (Sessions/Topology pages).
    #[test]
    fn vlan_setup_client_and_server_addresses_differ() {
        let dest = IpAddr::from(Ipv4Addr::new(1, 1, 1, 1));
        let remote = IpAddr::from(Ipv4Addr::new(2, 2, 2, 2));
        let key = [0u8; 32];

        let (server_addr, _) =
            vlan_setup("m1".to_string(), dest, None, 5, remote, key, false).unwrap();
        let (client_addr, _) = vlan_setup(
            "m2".to_string(),
            dest,
            Some("srv".to_string()),
            5,
            remote,
            key,
            false,
        )
        .unwrap();

        assert_ne!(server_addr, client_addr);
        assert_eq!(server_addr, Ipv4Addr::new(10, 0, 0, 21));
        assert_eq!(client_addr, Ipv4Addr::new(10, 0, 0, 22));
    }

    #[test]
    fn vxlan_setup_client_and_server_addresses_differ() {
        let dest = IpAddr::from(Ipv4Addr::new(1, 1, 1, 1));
        let remote = IpAddr::from(Ipv4Addr::new(2, 2, 2, 2));
        let key = [0u8; 32];

        let (server_addr, _) = vxlan_setup(
            "m1".to_string(),
            dest,
            None,
            5,
            remote,
            (None, None),
            None,
            key,
            4789,
            false,
            EgressRole::None,
        )
        .unwrap();
        let (client_addr, _) = vxlan_setup(
            "m2".to_string(),
            dest,
            Some("srv".to_string()),
            5,
            remote,
            (None, None),
            None,
            key,
            4789,
            false,
            EgressRole::None,
        )
        .unwrap();

        assert_ne!(server_addr, client_addr);
    }
}
