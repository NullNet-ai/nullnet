//! Linux MACsec route and generic-Netlink configuration.
use super::native_netlink::{CREATE, NESTED, NativeSocket, REQUEST, attr, attrs, blocking};
use std::io;
use std::sync::OnceLock;

static FAMILY: OnceLock<u16> = OnceLock::new();

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
