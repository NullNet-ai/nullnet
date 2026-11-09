//! Linux MACsec route and generic-Netlink configuration.
use super::native_netlink::{CREATE, NESTED, NativeSocket, REQUEST, attr, attrs, blocking};
use std::collections::{HashMap, HashSet};
use std::io;
use std::sync::OnceLock;

static FAMILY: OnceLock<u16> = OnceLock::new();

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct MacsecSnapshot {
    pub(crate) tx_sci: Option<u64>,
    pub(crate) tx_pn: Option<u64>,
    pub(crate) tx_encrypted: u64,
    pub(crate) rx_sci: Option<u64>,
    pub(crate) rx_pn: Option<u64>,
    pub(crate) rx_ok: u64,
    pub(crate) rx_invalid: u64,
    pub(crate) rx_not_valid: u64,
    pub(crate) rx_unknown_sci: u64,
}

pub(super) async fn prepare(
    name: String,
    parent: u32,
    peer_mac: Vec<u8>,
    group: u32,
) -> io::Result<u32> {
    blocking(move || {
        let mut route = NativeSocket::new(libc::NETLINK_ROUTE as _)?;
        let mut body = vec![0; 16];
        body.extend(attr(3, format!("{name}\0").as_bytes()));
        body.extend(attr(5, &parent.to_ne_bytes()));
        body.extend(attr(27, &group.to_ne_bytes()));
        let settings = [
            attr(2, &1u16.to_be_bytes()),
            attr(4, &0x0080_c200_0100_0002u64.to_ne_bytes()),
            attr(5, &128u32.to_ne_bytes()),
            attr(7, &[1]),
            attr(8, &[1]),
            attr(12, &[1]),
            attr(13, &[2]),
        ]
        .concat();
        body.extend(attr(
            18 | NESTED,
            &[attr(1, b"macsec\0"), attr(2 | NESTED, &settings)].concat(),
        ));
        route.request(16, CREATE, &body)?;
        let mut lookup = vec![0; 16];
        lookup.extend(attr(3, format!("{name}\0").as_bytes()));
        let reply = route.request(18, REQUEST, &lookup)?;
        let index = u32::from_ne_bytes(
            reply
                .get(4..8)
                .ok_or_else(|| io::Error::other("MACsec index missing"))?
                .try_into()
                .unwrap(),
        );
        let mut generic = NativeSocket::new(libc::NETLINK_GENERIC as _)?;
        let family = match FAMILY.get() {
            Some(family) => *family,
            None => {
                let reply = generic.generic(16, 3, 2, &attr(2, b"macsec\0"))?;
                let family = attrs(&reply)?
                    .into_iter()
                    .find(|(kind, _)| *kind == 1)
                    .and_then(|(_, data)| <[u8; 2]>::try_from(data).ok())
                    .map(u16::from_ne_bytes)
                    .ok_or_else(|| io::Error::other("MACsec family missing"))?;
                let _ = FAMILY.set(family);
                family
            }
        };
        let base = attr(1, &index.to_ne_bytes());
        let mut sci = peer_mac;
        sci.extend_from_slice(&1u16.to_be_bytes());
        generic.generic(
            family,
            1,
            1,
            &[
                base.clone(),
                attr(2 | NESTED, &[attr(1, &sci), attr(2, &[1])].concat()),
            ]
            .concat(),
        )?;
        Ok(index)
    })
    .await
}

pub(super) async fn associations(index: u32, peer_mac: Vec<u8>, key: [u8; 32]) -> io::Result<()> {
    blocking(move || {
        let mut generic = NativeSocket::new(libc::NETLINK_GENERIC as _)?;
        let family = *FAMILY
            .get()
            .ok_or_else(|| io::Error::other("MACsec was not prepared"))?;
        let base = attr(1, &index.to_ne_bytes());
        let mut sci = peer_mac;
        sci.extend_from_slice(&1u16.to_be_bytes());
        let rx = attr(2 | NESTED, &attr(1, &sci));
        use sha2::{Digest, Sha256};
        let id = Sha256::digest(key);
        let sa = attr(
            3 | NESTED,
            &[
                attr(1, &[0]),
                attr(2, &[1]),
                attr(3, &1u32.to_ne_bytes()),
                attr(4, &key),
                attr(5, &id[..16]),
            ]
            .concat(),
        );
        generic.generic(family, 4, 1, &[base.clone(), sa.clone()].concat())?;
        generic.generic(family, 7, 1, &[base, rx, sa].concat())?;
        Ok(())
    })
    .await
}

pub(crate) async fn snapshots(indices: Vec<u32>) -> io::Result<HashMap<u32, MacsecSnapshot>> {
    blocking(move || {
        let wanted: HashSet<_> = indices.into_iter().collect();
        if wanted.is_empty() {
            return Ok(HashMap::new());
        }
        let family = *FAMILY
            .get()
            .ok_or_else(|| io::Error::other("MACsec family unavailable"))?;
        let mut generic = NativeSocket::new(libc::NETLINK_GENERIC as _)?;
        let mut result = HashMap::new();
        for message in generic.generic_dump(family, 0, 1)? {
            let attributes = attrs(&message)?;
            let Some(index) = attributes
                .iter()
                .find(|(kind, _)| *kind == 1)
                .and_then(|(_, value)| read_u32(value))
            else {
                continue;
            };
            if wanted.contains(&index) {
                result.insert(index, parse_snapshot(&attributes)?);
            }
        }
        Ok(result)
    })
    .await
}

fn parse_snapshot(attributes: &[(u16, &[u8])]) -> io::Result<MacsecSnapshot> {
    let mut snapshot = MacsecSnapshot::default();
    for (kind, value) in attributes {
        match *kind {
            4 => {
                for (kind, value) in attrs(value)? {
                    if kind == 1 {
                        snapshot.tx_sci = read_sci(value);
                    }
                }
            }
            5 => {
                for (_, sa) in attrs(value)? {
                    for (kind, value) in attrs(sa)? {
                        if kind == 3 {
                            snapshot.tx_pn = read_counter(value);
                        }
                    }
                }
            }
            6 => {
                for (_, channel) in attrs(value)? {
                    parse_rx_channel(channel, &mut snapshot)?;
                }
            }
            7 => {
                for (kind, value) in attrs(value)? {
                    if kind == 2 {
                        snapshot.tx_encrypted = read_u64(value).unwrap_or_default();
                    }
                }
            }
            8 => {
                for (kind, value) in attrs(value)? {
                    if kind == 6 {
                        snapshot.rx_unknown_sci = read_u64(value).unwrap_or_default();
                    }
                }
            }
            _ => {}
        }
    }
    Ok(snapshot)
}

fn parse_rx_channel(data: &[u8], snapshot: &mut MacsecSnapshot) -> io::Result<()> {
    for (kind, value) in attrs(data)? {
        match kind {
            1 => snapshot.rx_sci = read_sci(value),
            3 => {
                for (_, sa) in attrs(value)? {
                    for (kind, value) in attrs(sa)? {
                        if kind == 3 {
                            snapshot.rx_pn = read_counter(value);
                        }
                    }
                }
            }
            4 => {
                for (kind, value) in attrs(value)? {
                    let counter = read_u64(value).unwrap_or_default();
                    match kind {
                        5 => snapshot.rx_ok += counter,
                        6 => snapshot.rx_invalid += counter,
                        8 => snapshot.rx_not_valid += counter,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn read_u32(value: &[u8]) -> Option<u32> {
    Some(u32::from_ne_bytes(value.try_into().ok()?))
}

fn read_u64(value: &[u8]) -> Option<u64> {
    Some(u64::from_ne_bytes(value.try_into().ok()?))
}

fn read_counter(value: &[u8]) -> Option<u64> {
    match value.len() {
        4 => read_u32(value).map(u64::from),
        8 => read_u64(value),
        _ => None,
    }
}

fn read_sci(value: &[u8]) -> Option<u64> {
    Some(u64::from_be_bytes(value.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_safe_macsec_diagnostics() {
        let tx_sa = attr(1 | NESTED, &attr(3, &12u32.to_ne_bytes()));
        let rx_sa = attr(1 | NESTED, &attr(3, &8u32.to_ne_bytes()));
        let rx_stats = [
            attr(5, &7u64.to_ne_bytes()),
            attr(6, &2u64.to_ne_bytes()),
            attr(8, &3u64.to_ne_bytes()),
        ]
        .concat();
        let rx_channel = attr(
            1 | NESTED,
            &[
                attr(1, &0x0202_0000_0102_0001u64.to_be_bytes()),
                attr(3 | NESTED, &rx_sa),
                attr(4 | NESTED, &rx_stats),
            ]
            .concat(),
        );
        let data = [
            attr(
                4 | NESTED,
                &attr(1, &0x0202_0000_0101_0001u64.to_be_bytes()),
            ),
            attr(5 | NESTED, &tx_sa),
            attr(6 | NESTED, &rx_channel),
            attr(7 | NESTED, &attr(2, &11u64.to_ne_bytes())),
            attr(8 | NESTED, &attr(6, &4u64.to_ne_bytes())),
        ]
        .concat();

        let attributes = attrs(&data).unwrap();
        let snapshot = parse_snapshot(&attributes).unwrap();
        assert_eq!(snapshot.tx_sci, Some(0x0202_0000_0101_0001));
        assert_eq!(snapshot.rx_sci, Some(0x0202_0000_0102_0001));
        assert_eq!(snapshot.tx_pn, Some(12));
        assert_eq!(snapshot.rx_pn, Some(8));
        assert_eq!(snapshot.tx_encrypted, 11);
        assert_eq!(snapshot.rx_ok, 7);
        assert_eq!(snapshot.rx_invalid, 2);
        assert_eq!(snapshot.rx_not_valid, 3);
        assert_eq!(snapshot.rx_unknown_sci, 4);
    }
}
