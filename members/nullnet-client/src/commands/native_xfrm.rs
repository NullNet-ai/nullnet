//! IPsec associations for dedicated endpoints and multiplexed host pairs.
use super::native_netlink::{CREATE, NativeSocket, REQUEST, attr, attrs, blocking};
use super::policy::types::XFRM_ROUTE_BIT;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::io;
use std::net::Ipv4Addr;
use std::sync::{Arc, LazyLock, Mutex};
use tokio::sync::Mutex as AsyncMutex;

const MARK_PREFIX: u32 = 0x4e80_0000;
const SPI_PREFIX: u32 = 0x4e00_0000;
const POLICY_INDEX_PREFIX: u32 = 0x4e00_0000;
const REPLAY_WINDOW: u32 = 4096;
type Peer = (Ipv4Addr, Ipv4Addr, u16);
type Owners = Arc<AsyncMutex<HashSet<u32>>>;
static PEERS: LazyLock<Mutex<HashMap<Peer, Owners>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static EDGES: LazyLock<Mutex<HashMap<u32, Edge>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Clone, Copy)]
struct Edge {
    id: u32,
    generation: u32,
    local: Ipv4Addr,
    remote: Ipv4Addr,
    port: u16,
    if_id: u32,
}
impl Edge {
    fn mark(self) -> u32 {
        MARK_PREFIX | self.id
    }
    fn direction(self) -> u32 {
        u32::from(self.local > self.remote)
    }
    fn multiplexed(self) -> bool {
        self.generation != 0
    }
    fn route_mark(self) -> u32 {
        self.mark() | XFRM_ROUTE_BIT
    }
    fn spi(self, direction: u32) -> u32 {
        SPI_PREFIX | (self.id << 1) | direction
    }
    fn policy_index(self) -> u32 {
        if self.generation == 0 {
            0
        } else {
            POLICY_INDEX_PREFIX | (self.id << 3) | 1
        }
    }
    fn policy_priority(self) -> u32 {
        if self.generation == 0 {
            0
        } else {
            u32::MAX - self.generation
        }
    }
    fn owners(self) -> Owners {
        PEERS
            .lock()
            .unwrap()
            .entry((self.local, self.remote, self.port))
            .or_default()
            .clone()
    }
}

fn address(ip: Ipv4Addr) -> Vec<u8> {
    [ip.octets().as_slice(), &[0; 12]].concat()
}

fn transport_owned_by(
    owners: &HashSet<u32>,
    edges: &HashMap<u32, Edge>,
    id: u32,
    except: Option<u32>,
) -> bool {
    owners
        .iter()
        .any(|owner| Some(*owner) != except && edges.get(owner).is_some_and(|edge| edge.id == id))
}

fn selector(source: Ipv4Addr, destination: Ipv4Addr, port: u16, inexact: bool) -> Vec<u8> {
    let mut body = vec![0; 56];
    if !inexact {
        body[..16].copy_from_slice(&address(destination));
        body[16..32].copy_from_slice(&address(source));
    }
    body[32..34].copy_from_slice(&port.to_be_bytes());
    body[34..36].copy_from_slice(&u16::MAX.to_be_bytes());
    body[40..42].copy_from_slice(&(libc::AF_INET as u16).to_ne_bytes());
    body[42..45].copy_from_slice(&[u8::from(!inexact) * 32, u8::from(!inexact) * 32, 17]);
    body
}
fn lifetime() -> Vec<u8> {
    [vec![255; 32], vec![0; 32]].concat()
}
fn identity(destination: Ipv4Addr, spi: u32) -> Vec<u8> {
    [
        address(destination),
        spi.to_be_bytes().to_vec(),
        vec![50, 0, 0, 0],
    ]
    .concat()
}
fn mark_attribute(mark: u32) -> Vec<u8> {
    attr(21, &[mark.to_ne_bytes(), u32::MAX.to_ne_bytes()].concat())
}

fn state_message(edge: Edge, direction: u32, key: &[u8; 32], delete: bool) -> (u16, u16, Vec<u8>) {
    let inbound = direction != edge.direction();
    let (source, destination) = if inbound {
        (edge.remote, edge.local)
    } else {
        (edge.local, edge.remote)
    };
    let spi = edge.spi(direction);
    if delete {
        let mut body = address(destination);
        body.extend(spi.to_be_bytes());
        body.extend((libc::AF_INET as u16).to_ne_bytes());
        body.extend([50, 0]);
        if !inbound {
            body.extend(mark_attribute(if edge.multiplexed() {
                edge.route_mark()
            } else {
                edge.mark()
            }));
            if edge.multiplexed() {
                body.extend(attr(31, &edge.if_id.to_ne_bytes()));
            }
        }
        return (17, REQUEST, body);
    }
    let mut body = vec![0; 224];
    body[56..80].copy_from_slice(&identity(destination, spi));
    body[80..96].copy_from_slice(&address(source));
    body[96..160].copy_from_slice(&lifetime());
    body[208..212].copy_from_slice(&edge.mark().to_ne_bytes());
    body[212..214].copy_from_slice(&(libc::AF_INET as u16).to_ne_bytes());
    body[214] = u8::from(edge.multiplexed());
    let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
    let derived = Sha256::digest(format!("{hex}{direction}").as_bytes());
    let derived_hex: String = derived.iter().map(|b| format!("{b:02x}")).collect();
    let salt = Sha256::digest(derived_hex.as_bytes());
    let mut algorithm = b"rfc4106(gcm(aes))".to_vec();
    algorithm.resize(64, 0);
    algorithm.extend(288u32.to_ne_bytes());
    algorithm.extend(128u32.to_ne_bytes());
    algorithm.extend_from_slice(&derived);
    algorithm.extend_from_slice(&salt[..4]);
    body.extend(attr(18, &algorithm));
    let words = REPLAY_WINDOW.div_ceil(32);
    let mut replay = vec![0; 24 + words as usize * 4];
    replay[..4].copy_from_slice(&words.to_ne_bytes());
    replay[20..24].copy_from_slice(&REPLAY_WINDOW.to_ne_bytes());
    body.extend(attr(23, &replay));
    if inbound || edge.multiplexed() {
        body.extend(attr(29, &edge.mark().to_ne_bytes()));
        body.extend(attr(30, &u32::MAX.to_ne_bytes()));
    }
    if !inbound {
        body.extend(mark_attribute(if edge.multiplexed() {
            edge.route_mark()
        } else {
            edge.mark()
        }));
        if edge.multiplexed() {
            body.extend(attr(31, &edge.if_id.to_ne_bytes()));
        }
    }
    (16, CREATE, body)
}

fn policy_mark(edge: Edge, inbound: bool) -> Vec<u8> {
    if inbound {
        attr(
            21,
            &[MARK_PREFIX.to_ne_bytes(), 0xffc0_0000u32.to_ne_bytes()].concat(),
        )
    } else {
        mark_attribute(if edge.multiplexed() {
            edge.route_mark()
        } else {
            edge.mark()
        })
    }
}

fn policy_message(edge: Edge, inbound: bool, delete: bool) -> (u16, u16, Vec<u8>) {
    let (source, destination) = if inbound {
        (edge.remote, edge.local)
    } else {
        (edge.local, edge.remote)
    };
    let direction = u8::from(!inbound);
    let mut body = selector(
        source,
        destination,
        edge.port,
        !inbound && edge.multiplexed(),
    );
    if delete {
        body.extend(if inbound { 0 } else { edge.policy_index() }.to_ne_bytes());
        body.extend([direction, 0, 0, 0]);
        body.extend(policy_mark(edge, inbound));
        if !inbound && edge.multiplexed() {
            body.extend(attr(31, &edge.if_id.to_ne_bytes()));
        }
        return (20, REQUEST, body);
    }
    body.extend(lifetime());
    body.resize(168, 0);
    if !inbound {
        body[152..156].copy_from_slice(&edge.policy_priority().to_ne_bytes());
        body[156..160].copy_from_slice(&edge.policy_index().to_ne_bytes());
    }
    body[160] = direction;
    let mut template = identity(
        destination,
        if inbound {
            0
        } else {
            edge.spi(edge.direction())
        },
    );
    template.extend((libc::AF_INET as u16).to_ne_bytes());
    template.extend([0, 0]);
    template.extend(address(source));
    template.extend((if inbound { 0 } else { edge.mark() }).to_ne_bytes());
    template.extend([0; 4]);
    template.extend([255; 12]);
    template[48] = u8::from(edge.multiplexed());
    body.extend(attr(5, &template));
    body.extend(policy_mark(edge, inbound));
    if !inbound && edge.multiplexed() {
        body.extend(attr(31, &edge.if_id.to_ne_bytes()));
    }
    (19, CREATE, body)
}

fn policy(socket: &mut NativeSocket, edge: Edge, inbound: bool, delete: bool) -> io::Result<()> {
    let (kind, flags, body) = policy_message(edge, inbound, delete);
    let result = socket.request(kind, flags, &body);
    if delete {
        missing_ok(result)
    } else {
        result.map(drop)
    }
}

fn batch(
    socket: &mut NativeSocket,
    messages: &[(u16, u16, Vec<u8>)],
    delete: bool,
) -> io::Result<()> {
    let requests: Vec<_> = messages
        .iter()
        .map(|(kind, flags, body)| (*kind, *flags, body.as_slice()))
        .collect();
    for result in socket.requests(&requests)? {
        match result {
            Ok(()) => {}
            Err(error)
                if delete && matches!(error.raw_os_error(), Some(libc::ENOENT | libc::ESRCH)) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

pub(super) fn mark(id: u32) -> u32 {
    MARK_PREFIX | id
}

pub(super) async fn install_port(
    owner: u32,
    id: u32,
    generation: u32,
    hosts: (Ipv4Addr, Ipv4Addr),
    key: [u8; 32],
    port: u16,
    if_id: u32,
) -> io::Result<()> {
    let edge = Edge {
        id,
        generation,
        local: hosts.0,
        remote: hosts.1,
        port,
        if_id,
    };
    let owners = edge.owners();
    let mut owners = owners.lock().await;
    let first_peer = owners.is_empty();
    let first_transport = {
        let edges = EDGES.lock().unwrap();
        !transport_owned_by(&owners, &edges, id, None)
    };
    if first_peer {
        blocking(move || {
            policy(
                &mut NativeSocket::new(libc::NETLINK_XFRM as _)?,
                edge,
                true,
                false,
            )
        })
        .await?;
    }
    let result = if first_transport {
        blocking(move || {
            let mut socket = NativeSocket::new(libc::NETLINK_XFRM as _)?;
            batch(
                &mut socket,
                &[
                    state_message(edge, edge.direction(), &key, false),
                    state_message(edge, 1 - edge.direction(), &key, false),
                    policy_message(edge, false, false),
                ],
                false,
            )
        })
        .await
    } else {
        Ok(())
    };
    if let Err(error) = result {
        if first_peer {
            let _ = blocking(move || {
                policy(
                    &mut NativeSocket::new(libc::NETLINK_XFRM as _)?,
                    edge,
                    true,
                    true,
                )
            })
            .await;
        }
        return Err(error);
    }
    EDGES.lock().unwrap().insert(owner, edge);
    owners.insert(owner);
    Ok(())
}

pub(super) async fn remove(owner: u32) -> io::Result<()> {
    let edge = EDGES.lock().unwrap().get(&owner).copied();
    let Some(edge) = edge else {
        return Ok(());
    };
    let owners = edge.owners();
    let mut owners = owners.lock().await;
    let last_transport = {
        let edges = EDGES.lock().unwrap();
        !transport_owned_by(&owners, &edges, edge.id, Some(owner))
    };
    let mut first_error = if last_transport {
        blocking(move || {
            let mut socket = NativeSocket::new(libc::NETLINK_XFRM as _)?;
            Ok(batch(
                &mut socket,
                &[
                    policy_message(edge, false, true),
                    state_message(edge, edge.direction(), &[0; 32], true),
                    state_message(edge, 1 - edge.direction(), &[0; 32], true),
                ],
                true,
            )
            .err())
        })
        .await?
    } else {
        None
    };
    if owners.contains(&owner)
        && owners.len() == 1
        && let Err(error) = blocking(move || {
            policy(
                &mut NativeSocket::new(libc::NETLINK_XFRM as _)?,
                edge,
                true,
                true,
            )
        })
        .await
    {
        first_error.get_or_insert(error);
    }
    if let Some(error) = first_error {
        Err(error)
    } else {
        owners.remove(&owner);
        EDGES.lock().unwrap().remove(&owner);
        Ok(())
    }
}

fn missing_ok(result: io::Result<Vec<u8>>) -> io::Result<()> {
    match result {
        Ok(_) => Ok(()),
        Err(e) if matches!(e.raw_os_error(), Some(libc::ENOENT | libc::ESRCH)) => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(body: &[u8], offset: usize) -> Vec<u16> {
        attrs(&body[offset..])
            .unwrap()
            .into_iter()
            .map(|(kind, _)| kind)
            .collect()
    }

    #[test]
    fn both_hosts_derive_the_same_directional_identity() {
        let a = Edge {
            id: 101,
            generation: 7,
            port: 4790,
            local: Ipv4Addr::new(192, 0, 2, 1),
            remote: Ipv4Addr::new(192, 0, 2, 2),
            if_id: 0x4e71_0001,
        };
        let b = Edge {
            local: a.remote,
            remote: a.local,
            ..a
        };
        assert_eq!(a.spi(a.direction()), b.spi(1 - b.direction()));
        assert_ne!(a.spi(0), a.spi(1));
        assert_eq!(a.mark(), b.mark());
        assert_eq!(a.policy_index() & 7, 1);
        assert_eq!(a.policy_index() & 0xfe00_0000, POLICY_INDEX_PREFIX);
        assert_eq!(a.policy_priority(), u32::MAX - 7);
        assert_eq!(selector(a.local, a.remote, a.port, false).len(), 56);
        assert_eq!(
            &selector(a.local, a.remote, a.port, false)[32..36],
            &[0x12, 0xb6, 0xff, 0xff]
        );
        assert_eq!(&selector(a.local, a.remote, a.port, true)[42..44], &[0, 0]);
        assert_eq!(a.route_mark(), a.mark() | XFRM_ROUTE_BIT);
        assert_eq!(Edge { generation: 0, ..a }.policy_index(), 0);
        assert_eq!(Edge { generation: 0, ..a }.policy_priority(), 0);
    }

    #[test]
    fn multiplexed_messages_use_tunnel_mode_shard_and_routing_mark() {
        let edge = Edge {
            id: 0x20_002a,
            generation: 9,
            port: 4791,
            local: Ipv4Addr::new(192, 0, 2, 1),
            remote: Ipv4Addr::new(192, 0, 2, 2),
            if_id: 0x4e71_002b,
        };
        let (_, _, outbound_state) = state_message(edge, edge.direction(), &[7; 32], false);
        let (_, _, inbound_state) = state_message(edge, 1 - edge.direction(), &[7; 32], false);
        let (_, _, outbound_policy) = policy_message(edge, false, false);
        let (_, _, inbound_policy) = policy_message(edge, true, false);

        assert_eq!(outbound_state[214], 1);
        assert_eq!(inbound_state[214], 1);
        assert_eq!(kinds(&outbound_state, 224), vec![18, 23, 29, 30, 21, 31]);
        assert_eq!(kinds(&inbound_state, 224), vec![18, 23, 29, 30]);
        assert_eq!(&outbound_policy[42..44], &[0, 0]);
        assert_eq!(&inbound_policy[42..44], &[32, 32]);
        assert_eq!(kinds(&outbound_policy, 168), vec![5, 21, 31]);
        assert_eq!(kinds(&inbound_policy, 168), vec![5, 21]);
        let state_attributes = attrs(&outbound_state[224..]).unwrap();
        let mark = state_attributes
            .iter()
            .find(|(kind, _)| *kind == 21)
            .unwrap()
            .1;
        assert_eq!(
            u32::from_ne_bytes(mark[..4].try_into().unwrap()),
            edge.route_mark()
        );
        let output_mark = state_attributes
            .iter()
            .find(|(kind, _)| *kind == 29)
            .unwrap()
            .1;
        assert_eq!(
            u32::from_ne_bytes(output_mark.try_into().unwrap()),
            edge.mark()
        );
    }

    #[test]
    fn dedicated_messages_remain_exact_transport_mode_without_shards() {
        let edge = Edge {
            id: 42,
            generation: 0,
            port: 4790,
            local: Ipv4Addr::new(192, 0, 2, 1),
            remote: Ipv4Addr::new(192, 0, 2, 2),
            if_id: 0,
        };
        let (_, _, outbound_state) = state_message(edge, edge.direction(), &[7; 32], false);
        let (_, _, outbound_policy) = policy_message(edge, false, false);

        assert_eq!(outbound_state[214], 0);
        assert_eq!(kinds(&outbound_state, 224), vec![18, 23, 21]);
        assert_eq!(&outbound_policy[42..44], &[32, 32]);
        assert_eq!(kinds(&outbound_policy, 168), vec![5, 21]);
    }

    #[test]
    fn shared_transport_lives_until_its_last_owner() {
        let first = Edge {
            id: 0x10_0000,
            generation: 0,
            port: 47890,
            local: Ipv4Addr::new(192, 0, 2, 1),
            remote: Ipv4Addr::new(192, 0, 2, 2),
            if_id: 0,
        };
        let second = Edge { ..first };
        let mut edges = HashMap::new();
        edges.insert(101, first);
        edges.insert(102, second);
        let owners = HashSet::from([101, 102]);

        assert!(transport_owned_by(&owners, &edges, first.id, None));
        assert!(transport_owned_by(&owners, &edges, first.id, Some(101)));
        assert!(transport_owned_by(&owners, &edges, first.id, Some(102)));
        let last = HashSet::from([102]);
        assert!(!transport_owned_by(&last, &edges, first.id, Some(102)));
    }
}

pub(super) async fn purge() -> io::Result<()> {
    blocking(|| {
        let mut socket = NativeSocket::new(libc::NETLINK_XFRM as _)?;
        let policies = socket.dump(21, &[0; 64])?;
        for message in policies {
            if message.len() < 168 {
                return Err(io::Error::other("truncated XFRM policy dump"));
            }
            let attributes = attrs(&message[168..])?;
            let marked = attributes
                .iter()
                .any(|(kind, value)| *kind == 21 && owned_mark(value));
            let templated = attributes.iter().any(|(kind, value)| {
                *kind == 5
                    && value.as_chunks::<64>().0.iter().any(|template| {
                        template[20] == 50
                            && u32::from_be_bytes(template[16..20].try_into().unwrap())
                                & 0xff80_0000
                                == SPI_PREFIX
                            && u32::from_ne_bytes(template[44..48].try_into().unwrap())
                                & 0xffc0_0000
                                == MARK_PREFIX
                    })
            });
            if !marked && !templated {
                continue;
            }
            let mut body = message[..56].to_vec();
            body.extend_from_slice(&message[156..161]);
            body.extend([0; 3]);
            for (kind, value) in attributes {
                if matches!(kind, 21 | 31) {
                    body.extend(attr(kind, value));
                }
            }
            missing_ok(socket.request(20, REQUEST, &body))?;
        }
        for message in socket.dump(18, &[0; 24])? {
            if message.len() < 224 {
                return Err(io::Error::other("truncated XFRM state dump"));
            }
            let reqid = u32::from_ne_bytes(message[208..212].try_into().unwrap());
            let spi = u32::from_be_bytes(message[72..76].try_into().unwrap());
            if reqid & 0xffc0_0000 != MARK_PREFIX || spi & 0xff80_0000 != SPI_PREFIX {
                continue;
            }
            let mut body = message[56..76].to_vec();
            body.extend_from_slice(&message[212..214]);
            body.extend([50, 0]);
            for (kind, value) in attrs(&message[224..])? {
                if matches!(kind, 21 | 31) {
                    body.extend(attr(kind, value));
                }
            }
            missing_ok(socket.request(17, REQUEST, &body))?;
        }
        Ok(())
    })
    .await
}

fn owned_mark(value: &[u8]) -> bool {
    value.len() == 8
        && u32::from_ne_bytes(value[..4].try_into().unwrap()) & 0xffc0_0000 == MARK_PREFIX
        && matches!(
            u32::from_ne_bytes(value[4..].try_into().unwrap()),
            u32::MAX | 0xffc0_0000
        )
}
