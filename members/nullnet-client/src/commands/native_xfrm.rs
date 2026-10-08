//! Dedicated endpoint IPsec associations sharing a VXLAN UDP socket.
use super::native_netlink::{CREATE, NativeSocket, REQUEST, attr, attrs, blocking};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::io;
use std::net::Ipv4Addr;
use std::sync::{Arc, LazyLock, Mutex};
use tokio::sync::Mutex as AsyncMutex;

const MARK_PREFIX: u32 = 0x4e80_0000;
const SPI_PREFIX: u32 = 0x4e00_0000;
const REPLAY_WINDOW: u32 = 4096;
type Peer = (Ipv4Addr, Ipv4Addr, u16);
type Owners = Arc<AsyncMutex<HashSet<u32>>>;
static PEERS: LazyLock<Mutex<HashMap<Peer, Owners>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static EDGES: LazyLock<Mutex<HashMap<u32, Edge>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Clone, Copy)]
struct Edge {
    id: u32,
    local: Ipv4Addr,
    remote: Ipv4Addr,
    port: u16,
}
impl Edge {
    fn mark(self) -> u32 {
        MARK_PREFIX | self.id
    }
    fn direction(self) -> u32 {
        u32::from(self.local > self.remote)
    }
    fn spi(self, direction: u32) -> u32 {
        SPI_PREFIX | (self.id << 1) | direction
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
fn selector(source: Ipv4Addr, destination: Ipv4Addr, port: u16) -> Vec<u8> {
    let mut body = vec![0; 56];
    body[..16].copy_from_slice(&address(destination));
    body[16..32].copy_from_slice(&address(source));
    body[32..34].copy_from_slice(&port.to_be_bytes());
    body[34..36].copy_from_slice(&u16::MAX.to_be_bytes());
    body[40..42].copy_from_slice(&(libc::AF_INET as u16).to_ne_bytes());
    body[42..45].copy_from_slice(&[32, 32, 17]);
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

fn state(
    socket: &mut NativeSocket,
    edge: Edge,
    direction: u32,
    key: &[u8; 32],
    delete: bool,
) -> io::Result<()> {
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
            body.extend(mark_attribute(edge.mark()));
        }
        return missing_ok(socket.request(17, REQUEST, &body));
    }
    let mut body = vec![0; 224];
    body[56..80].copy_from_slice(&identity(destination, spi));
    body[80..96].copy_from_slice(&address(source));
    body[96..160].copy_from_slice(&lifetime());
    body[208..212].copy_from_slice(&edge.mark().to_ne_bytes());
    body[212..214].copy_from_slice(&(libc::AF_INET as u16).to_ne_bytes());
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
    if inbound {
        body.extend(attr(29, &edge.mark().to_ne_bytes()));
        body.extend(attr(30, &u32::MAX.to_ne_bytes()));
    } else {
        body.extend(mark_attribute(edge.mark()));
    }
    socket.request(16, CREATE, &body)?;
    Ok(())
}

fn policy_mark(edge: Edge, inbound: bool) -> Vec<u8> {
    if inbound {
        attr(
            21,
            &[MARK_PREFIX.to_ne_bytes(), 0xffc0_0000u32.to_ne_bytes()].concat(),
        )
    } else {
        mark_attribute(edge.mark())
    }
}

fn policy(socket: &mut NativeSocket, edge: Edge, inbound: bool, delete: bool) -> io::Result<()> {
    let (source, destination) = if inbound {
        (edge.remote, edge.local)
    } else {
        (edge.local, edge.remote)
    };
    let direction = u8::from(!inbound);
    let mut body = selector(source, destination, edge.port);
    if delete {
        body.extend([0, 0, 0, 0, direction, 0, 0, 0]);
        body.extend(policy_mark(edge, inbound));
        return missing_ok(socket.request(20, REQUEST, &body));
    }
    body.extend(lifetime());
    body.resize(168, 0);
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
    body.extend(attr(5, &template));
    body.extend(policy_mark(edge, inbound));
    socket.request(19, CREATE, &body)?;
    Ok(())
}

pub(super) fn mark(id: u32) -> u32 {
    MARK_PREFIX | id
}

pub(super) async fn install_port(
    id: u32,
    local: Ipv4Addr,
    remote: Ipv4Addr,
    key: [u8; 32],
    port: u16,
) -> io::Result<()> {
    let edge = Edge {
        id,
        local,
        remote,
        port,
    };
    EDGES.lock().unwrap().insert(id, edge);
    let owners = edge.owners();
    let mut owners = owners.lock().await;
    if owners.is_empty() {
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
    owners.insert(id);
    drop(owners);
    blocking(move || {
        let mut socket = NativeSocket::new(libc::NETLINK_XFRM as _)?;
        state(&mut socket, edge, edge.direction(), &key, false)?;
        state(&mut socket, edge, 1 - edge.direction(), &key, false)?;
        policy(&mut socket, edge, false, false)
    })
    .await
}

pub(super) async fn remove(id: u32) -> io::Result<()> {
    let edge = EDGES.lock().unwrap().get(&id).copied();
    let Some(edge) = edge else {
        return Ok(());
    };
    blocking(move || {
        let mut socket = NativeSocket::new(libc::NETLINK_XFRM as _)?;
        policy(&mut socket, edge, false, true)?;
        state(&mut socket, edge, edge.direction(), &[0; 32], true)?;
        state(&mut socket, edge, 1 - edge.direction(), &[0; 32], true)
    })
    .await?;
    let owners = edge.owners();
    let mut owners = owners.lock().await;
    if owners.contains(&id) && owners.len() == 1 {
        blocking(move || {
            policy(
                &mut NativeSocket::new(libc::NETLINK_XFRM as _)?,
                edge,
                true,
                true,
            )
        })
        .await?;
    }
    owners.remove(&id);
    EDGES.lock().unwrap().remove(&id);
    Ok(())
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

    #[test]
    fn both_hosts_derive_the_same_directional_identity() {
        let a = Edge {
            id: 101,
            port: 4790,
            local: Ipv4Addr::new(192, 0, 2, 1),
            remote: Ipv4Addr::new(192, 0, 2, 2),
        };
        let b = Edge {
            local: a.remote,
            remote: a.local,
            ..a
        };
        assert_eq!(a.spi(a.direction()), b.spi(1 - b.direction()));
        assert_ne!(a.spi(0), a.spi(1));
        assert_eq!(a.mark(), b.mark());
        assert_eq!(selector(a.local, a.remote, a.port).len(), 56);
        assert_eq!(
            &selector(a.local, a.remote, a.port)[32..36],
            &[0x12, 0xb6, 0xff, 0xff]
        );
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
                    && value.chunks_exact(64).any(|template| {
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
                if kind == 21 {
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
                if kind == 21 {
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
