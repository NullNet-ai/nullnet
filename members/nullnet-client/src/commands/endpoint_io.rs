//! Acknowledged addressing, conntrack cleanup and endpoint readiness.
use super::endpoint_namespace::EndpointNamespace;
use super::native_netlink::{CREATE, NESTED, NativeSocket, REQUEST, attr, attrs};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io;
use std::net::Ipv4Addr;
use std::os::fd::AsRawFd;
use std::sync::{Arc, Mutex};

pub(super) type EndpointSockets = Arc<Mutex<(EndpointIo, Option<EndpointIo>)>>;

#[derive(Clone)]
pub(super) struct Cleanup {
    pub root_names: Vec<String>,
    pub io: EndpointSockets,
    pub namespace: Option<Arc<EndpointNamespace>>,
    pub root_ips: [Ipv4Addr; 2],
    pub container_ip: Ipv4Addr,
    pub crypto_id: Option<u32>,
}

pub(super) async fn cleanup_batch(cleanups: Vec<Cleanup>) -> io::Result<()> {
    for cleanup in &cleanups {
        if let Some(id) = cleanup.crypto_id {
            super::native_xfrm::remove(id).await?;
        }
    }
    super::native_netlink::blocking(move || {
        let root_ips: Vec<_> = cleanups.iter().flat_map(|c| c.root_ips).collect();
        cleanups[0].io.lock().unwrap().0.clear_flows(&root_ips)?;
        let mut namespaces: HashMap<usize, (EndpointSockets, Vec<Ipv4Addr>)> = HashMap::new();
        for cleanup in cleanups {
            if let Some(namespace) = &cleanup.namespace {
                namespaces
                    .entry(Arc::as_ptr(namespace) as usize)
                    .or_insert_with(|| (cleanup.io.clone(), Vec::new()))
                    .1
                    .push(cleanup.container_ip);
            }
        }
        for (io, ips) in namespaces.into_values() {
            io.lock().unwrap().1.as_mut().unwrap().clear_flows(&ips)?;
        }
        Ok(())
    })
    .await
}

pub(super) struct EndpointIo {
    pub route: NativeSocket,
    conntrack: NativeSocket,
}

impl EndpointIo {
    pub fn open(namespace: Option<&File>) -> io::Result<Self> {
        let original = File::open("/proc/thread-self/ns/net")?;
        if let Some(namespace) = namespace
            && unsafe { libc::setns(namespace.as_raw_fd(), libc::CLONE_NEWNET) } != 0
        {
            return Err(io::Error::last_os_error());
        }
        let result = (|| {
            Ok(Self {
                route: NativeSocket::new(libc::NETLINK_ROUTE as _)?,
                conntrack: NativeSocket::new(libc::NETLINK_NETFILTER as _)?,
            })
        })();
        if namespace.is_some()
            && unsafe { libc::setns(original.as_raw_fd(), libc::CLONE_NEWNET) } != 0
        {
            std::process::abort();
        }
        result
    }

    pub fn clear_flows(&mut self, ips: &[Ipv4Addr]) -> io::Result<()> {
        // Filtered DELETE scans the entire host table under a global mutex.
        // Dump once, then delete exact reply tuples with their conntrack IDs.
        let ips: HashSet<[u8; 4]> = ips.iter().map(Ipv4Addr::octets).collect();
        for entry in self.conntrack.dump(257, &[2, 0, 0, 0])? {
            if let Some(body) = flow_deletion(&entry, &ips)? {
                match self.conntrack.request(258, REQUEST, &body) {
                    Ok(_) => {}
                    Err(e) if e.raw_os_error() == Some(libc::ENOENT) => {}
                    Err(e) => return Err(e),
                }
            }
        }
        Ok(())
    }
}

fn flow_deletion(entry: &[u8], ips: &HashSet<[u8; 4]>) -> io::Result<Option<Vec<u8>>> {
    let body = entry
        .get(4..)
        .ok_or_else(|| io::Error::other("Truncated conntrack entry"))?;
    if entry[0] != 2 {
        return Ok(None);
    }
    let fields = attrs(body)?;
    let mut matches = false;
    for (_, tuple) in fields.iter().filter(|(kind, _)| matches!(kind, 1 | 2)) {
        for (_, addresses) in attrs(tuple)?.into_iter().filter(|(kind, _)| *kind == 1) {
            for (kind, address) in attrs(addresses)? {
                if matches!(kind, 1 | 2) && ips.contains(address) {
                    matches = true;
                }
            }
        }
    }
    if !matches {
        return Ok(None);
    }
    let field = |kind| {
        fields
            .iter()
            .find(|(k, _)| *k == kind)
            .map(|(_, value)| *value)
            .ok_or_else(|| io::Error::other("Conntrack entry lacks identity"))
    };
    let mut deletion = entry[..4].to_vec();
    deletion.extend(attr(2 | NESTED, field(2)?));
    deletion.extend(attr(12, field(12)?));
    if let Some((_, zone)) = fields.iter().find(|(kind, _)| *kind == 18) {
        deletion.extend(attr(18, zone));
    }
    Ok(Some(deletion))
}

pub(super) fn info(index: u32, up: bool) -> Vec<u8> {
    [
        vec![0; 4],
        index.to_ne_bytes().to_vec(),
        u32::from(up).to_ne_bytes().to_vec(),
        1u32.to_ne_bytes().to_vec(),
    ]
    .concat()
}

pub(super) fn set_up(route: &mut NativeSocket, index: u32) -> io::Result<()> {
    let body = info(index, true);
    route.request(19, REQUEST, &body)?;
    Ok(())
}

pub(super) fn add_address(
    route: &mut NativeSocket,
    index: u32,
    ip: Ipv4Addr,
    prefix: u8,
) -> io::Result<()> {
    let body = [
        vec![2, prefix, 0, 0],
        index.to_ne_bytes().to_vec(),
        attr(1, &ip.octets()),
        attr(2, &ip.octets()),
    ]
    .concat();
    route.request(20, CREATE, &body)?;
    Ok(())
}

pub(super) fn synchronize(route: &mut NativeSocket, name: &str, index: u32) -> io::Result<()> {
    let data = route.request(
        18,
        REQUEST,
        &[vec![0; 16], attr(3, format!("{name}\0").as_bytes())].concat(),
    )?;
    if data.get(4..8) != Some(index.to_ne_bytes().as_slice()) {
        return Err(io::Error::other("Fresh interface identity changed"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "requires root, conntrack and an isolated root network namespace"]
    async fn batched_cleanup_removes_owned_tcp_udp_flows_and_preserves_foreign_flows() {
        use tokio::process::Command;
        let name = "ns_ct_batch";
        let namespace = super::super::endpoint_namespace::create(name)
            .await
            .unwrap();
        for prefix in [Vec::new(), vec!["netns", "exec", name]] {
            for (protocol, source, destination, reply_destination, zone) in [
                ("tcp", "10.252.0.1", "192.0.2.1", "10.252.0.1", "0"),
                ("udp", "192.0.2.2", "10.252.1.1", "192.0.2.2", "0"),
                ("tcp", "172.31.0.2", "192.0.2.3", "10.252.1.1", "79"),
                ("udp", "172.31.0.3", "192.0.2.4", "172.31.0.3", "0"),
            ] {
                let mut command = if prefix.is_empty() {
                    Command::new("conntrack")
                } else {
                    let mut command = Command::new("ip");
                    command.args(&prefix).arg("conntrack");
                    command
                };
                command.args([
                    "-I",
                    "-p",
                    protocol,
                    "--orig-src",
                    source,
                    "--orig-dst",
                    destination,
                    "--sport",
                    "45554",
                    "--dport",
                    "45555",
                    "--reply-src",
                    destination,
                    "--reply-dst",
                    reply_destination,
                    "--reply-port-src",
                    "45555",
                    "--reply-port-dst",
                    "45554",
                    "--timeout",
                    "300",
                    "--zone",
                    zone,
                ]);
                if protocol == "tcp" {
                    command.args(["--state", "ESTABLISHED"]);
                }
                assert!(command.status().await.unwrap().success());
            }
        }
        let mut cleanups = Vec::new();
        for subnet in [0, 1] {
            let ns = namespace.clone();
            let io = super::super::native_netlink::blocking(move || {
                Ok((
                    EndpointIo::open(None)?,
                    Some(EndpointIo::open(Some(&ns.file))?),
                ))
            })
            .await
            .unwrap();
            cleanups.push(Cleanup {
                root_names: Vec::new(),
                io: Arc::new(Mutex::new(io)),
                namespace: Some(namespace.clone()),
                root_ips: [
                    Ipv4Addr::new(10, 252, subnet, 1),
                    Ipv4Addr::new(10, 252, subnet, 2),
                ],
                container_ip: Ipv4Addr::new(10, 252, subnet, 1),
                crypto_id: None,
            });
        }
        cleanup_batch(cleanups.clone()).await.unwrap();
        cleanup_batch(cleanups).await.unwrap();
        for prefix in [Vec::new(), vec!["netns", "exec", name]] {
            let mut command = if prefix.is_empty() {
                Command::new("conntrack")
            } else {
                let mut command = Command::new("ip");
                command.args(&prefix).arg("conntrack");
                command
            };
            let output = command.arg("-L").output().await.unwrap();
            assert!(output.status.success());
            let entries = String::from_utf8(output.stdout).unwrap();
            assert_eq!(entries.lines().count(), 1, "{entries}");
            assert!(entries.contains("src=172.31.0.3"));
            assert!(!entries.contains("10.252."));
        }
        super::super::endpoint_namespace::remove(name)
            .await
            .unwrap();
    }

    #[test]
    fn flow_cleanup_matches_both_tuples_and_preserves_zone_and_identity() {
        let ips: Vec<Ipv4Addr> = (1..=4).map(|last| Ipv4Addr::new(192, 0, 2, last)).collect();
        let tuple = |a: Ipv4Addr, b: Ipv4Addr| {
            [
                attr(
                    1 | NESTED,
                    &[attr(1, &a.octets()), attr(2, &b.octets())].concat(),
                ),
                attr(2 | NESTED, &attr(1, &[6])),
            ]
            .concat()
        };
        let reply = tuple(ips[2], ips[3]);
        let entry = [
            vec![2, 0, 0, 0],
            attr(1 | NESTED, &tuple(ips[0], ips[1])),
            attr(2 | NESTED, &reply),
            attr(12, &123u32.to_be_bytes()),
            attr(18, &79u16.to_be_bytes()),
        ]
        .concat();
        let expected = [
            vec![2, 0, 0, 0],
            attr(2 | NESTED, &reply),
            attr(12, &123u32.to_be_bytes()),
            attr(18, &79u16.to_be_bytes()),
        ]
        .concat();
        for ip in &ips {
            assert_eq!(
                flow_deletion(&entry, &HashSet::from([ip.octets()])).unwrap(),
                Some(expected.clone())
            );
        }
        assert!(
            flow_deletion(&entry, &HashSet::from([Ipv4Addr::LOCALHOST.octets()]))
                .unwrap()
                .is_none()
        );
    }
}

pub(super) fn peer_route(
    route: &mut NativeSocket,
    destination: Ipv4Addr,
    index: u32,
    source: Ipv4Addr,
) -> io::Result<()> {
    route.request(
        24,
        CREATE,
        &[
            vec![2, 32, 0, 0, 254, 4, 253, 1, 0, 0, 0, 0],
            attr(1, &destination.octets()),
            attr(4, &index.to_ne_bytes()),
            attr(7, &source.octets()),
        ]
        .concat(),
    )?;
    Ok(())
}
