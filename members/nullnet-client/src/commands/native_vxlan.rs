//! VXLAN VNI-filter membership through route Netlink.
use super::native_netlink::{NESTED, NativeSocket, REQUEST, attr, blocking};
use std::io;

const RTM_NEWTUNNEL: u16 = 120;
const RTM_DELTUNNEL: u16 = 121;
const RTM_DELLINK: u16 = 17;
const VXLAN_VNIFILTER_ENTRY: u16 = 1;
const VXLAN_VNIFILTER_ENTRY_START: u16 = 1;

fn message(ifindex: u32, vni: u32) -> Vec<u8> {
    let mut body = vec![libc::AF_BRIDGE as u8, 0, 0, 0];
    body.extend(ifindex.to_ne_bytes());
    let entry = attr(VXLAN_VNIFILTER_ENTRY_START, &vni.to_ne_bytes());
    body.extend(attr(VXLAN_VNIFILTER_ENTRY | NESTED, &entry));
    body
}

async fn modify(ifindex: u32, vni: u32, add: bool) -> io::Result<()> {
    blocking(move || {
        let kind = if add { RTM_NEWTUNNEL } else { RTM_DELTUNNEL };
        let result = NativeSocket::new(libc::NETLINK_ROUTE as _)?.request(
            kind,
            REQUEST,
            &message(ifindex, vni),
        );
        match result {
            Ok(_) => Ok(()),
            Err(error)
                if !add && matches!(error.raw_os_error(), Some(libc::ENOENT | libc::ESRCH)) =>
            {
                Ok(())
            }
            Err(error) => Err(error),
        }
    })
    .await
}

pub(super) async fn add_vni(ifindex: u32, vni: u32) -> io::Result<()> {
    modify(ifindex, vni, true).await
}

pub(super) async fn remove_vni(ifindex: u32, vni: u32) -> io::Result<()> {
    modify(ifindex, vni, false).await
}

pub(super) async fn remove_link(ifindex: u32) -> io::Result<()> {
    blocking(move || {
        let mut body = vec![libc::AF_UNSPEC as u8, 0, 0, 0];
        body.extend(ifindex.to_ne_bytes());
        body.extend([0; 8]);
        let result =
            NativeSocket::new(libc::NETLINK_ROUTE as _)?.request(RTM_DELLINK, REQUEST, &body);
        match result {
            Ok(_) => Ok(()),
            Err(error) if matches!(error.raw_os_error(), Some(libc::ENODEV | libc::ENOENT)) => {
                Ok(())
            }
            Err(error) => Err(error),
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::native_netlink::attrs;

    #[test]
    fn vni_filter_message_matches_tunnel_uapi() {
        let body = message(17, 0x12_3456);
        assert_eq!(body[..4], [libc::AF_BRIDGE as u8, 0, 0, 0]);
        assert_eq!(u32::from_ne_bytes(body[4..8].try_into().unwrap()), 17);
        let outer = attrs(&body[8..]).unwrap();
        assert_eq!(outer[0].0, VXLAN_VNIFILTER_ENTRY);
        let inner = attrs(outer[0].1).unwrap();
        assert_eq!(inner[0].0, VXLAN_VNIFILTER_ENTRY_START);
        assert_eq!(
            u32::from_ne_bytes(inner[0].1.try_into().unwrap()),
            0x12_3456
        );
    }
}
