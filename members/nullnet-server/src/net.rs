use ipnetwork::Ipv4Network;
use nullnet_grpc_lib::nullnet_grpc::{
    HostMapping, MsgId, Net, NetMessage, VlanSetup, VlanTeardown, VxlanSetup, VxlanTeardown,
    net_message,
};
use nullnet_liberror::{ErrorHandler, Location, location};
use std::net::{IpAddr, Ipv4Addr};

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
    ) -> Option<(Ipv4Addr, NetMessage)> {
        match self {
            Net::Vlan => vlan_setup(
                msg_id,
                dest,
                remote_server_name,
                net_id,
                remote,
                encryption_key,
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
                dstport.unwrap_or(0),
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
    ) -> NetMessage {
        match self {
            Net::Vlan => NetMessage {
                message: Some(net_message::Message::VlanTeardown(VlanTeardown {
                    vlan_id: net_id,
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
                    dstport: u32::from(dstport.unwrap_or(0)),
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
        server_veth,
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

    let host_mapping = remote_server_name.map(|name| HostMapping {
        ip: server_net_ip.to_string(),
        name,
    });

    Some((
        server_net_ip,
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
            })),
        },
    ))
}
