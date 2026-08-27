use crate::services::clients::Client;
use std::net::IpAddr;

pub(crate) struct RegisteredEdge {
    pub(crate) client: (IpAddr, Client),
    pub(crate) server: (IpAddr, Client),
    pub(crate) client_docker: Option<String>,
    pub(crate) server_docker: Option<String>,
    /// `Some(port)` iff this edge is the entry point of a backend-triggered
    /// chain. The port is the trigger port observed by the initiator and is
    /// echoed in the client-side `VxlanSetup.dnat_port` so the receiver can
    /// install DNAT(port -> `overlay_ip`).
    pub(crate) backend_entry_port: Option<u32>,
    /// `true` iff this is an egress forward-proxy edge (initiator service ->
    /// proxy). Drives the `egress_steer`/`egress_intercept` markers on the two
    /// `VxlanSetup` messages instead of DNAT.
    pub(crate) egress: bool,
}

impl RegisteredEdge {
    pub(crate) fn new(
        client_ip: IpAddr,
        client: Client,
        client_docker: Option<String>,
        server_ip: IpAddr,
        server: Client,
        server_docker: Option<String>,
    ) -> Self {
        Self {
            client: (client_ip, client),
            server: (server_ip, server),
            client_docker,
            server_docker,
            backend_entry_port: None,
            egress: false,
        }
    }
}
