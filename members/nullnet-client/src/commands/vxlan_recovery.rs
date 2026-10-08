//! Remove old owned resources before a new client incarnation registers.
use futures::{StreamExt, TryStreamExt};
use rtnetlink::packet_route::link::{InfoKind, LinkAttribute, LinkInfo, LinkMessage};
use rtnetlink::{Handle, LinkUnspec};
const ALIAS: &str = "nullnet:vxlan-bridge:v1";
const GROUP: u32 = 0x4e60_0001;
fn name(link: &LinkMessage) -> Option<&str> {
    link.attributes.iter().find_map(|a| match a {
        LinkAttribute::IfName(name) => Some(name.as_str()),
        _ => None,
    })
}

pub(super) fn owned(link: &LinkMessage) -> bool {
    let bridge = link.attributes.iter().any(|a| {
        matches!(a, LinkAttribute::LinkInfo(info)
        if info.iter().any(|i| matches!(i, LinkInfo::Kind(InfoKind::Bridge))))
    });
    link.attributes.iter().any(|a| {
        matches!(a, LinkAttribute::IfAlias(alias) if alias == ALIAS)
            || matches!(a, LinkAttribute::Group(group) if *group == GROUP)
    }) && bridge
        || name(link)
            .and_then(|name| name.strip_prefix("br_"))
            .is_some_and(|suffix| {
                if !super::owned_endpoint_suffix(suffix) {
                    return false;
                }
                let (id, side) = suffix.rsplit_once('_').unwrap();
                let group =
                    0x4e00_0000 | (id.parse::<u32>().unwrap() << 1) | u32::from(side == "c");
                bridge || link.attributes
                    .iter()
                    .any(|a| matches!(a, LinkAttribute::Group(value) if *value == group || *value == 0x4e40_0000))
            })
}

fn owned_transport(link: &LinkMessage, links: &[LinkMessage]) -> bool {
    let Some(name) = name(link).filter(|name| super::owned_vxlan_link(name)) else {
        return false;
    };
    let kind = link.attributes.iter().find_map(|a| match a {
        LinkAttribute::LinkInfo(info) => info.iter().find_map(|i| match i {
            LinkInfo::Kind(kind) => Some(kind),
            _ => None,
        }),
        _ => None,
    });
    let expected = if name.starts_with("macsec-") {
        Some(&InfoKind::MacSec)
    } else if name.starts_with("nnv_") || name.starts_with("vxlan-") {
        Some(&InfoKind::Vxlan)
    } else {
        Some(&InfoKind::Veth)
    };
    if kind != expected {
        return false;
    }
    let marked = |link: &LinkMessage| {
        link.attributes.iter().any(|a|
        matches!(a, LinkAttribute::Group(group) if *group & 0xffc0_0000 == 0x4e00_0000 || *group == 0x4e40_0000))
    };
    let attached_to_owned_bridge = |link: &LinkMessage| {
        link.attributes.iter().any(|a|
        matches!(a, LinkAttribute::Controller(master) if links.iter().any(|bridge| bridge.header.index == *master && owned(bridge))))
    };
    marked(link) || attached_to_owned_bridge(link) || (kind == Some(&InfoKind::MacSec) && link.attributes.iter().any(|a|
        matches!(a, LinkAttribute::Link(parent) if links.iter().any(|p| p.header.index == *parent && (marked(p) || attached_to_owned_bridge(p))))))
}

pub(super) async fn recover(handle: &Handle) -> Result<(), String> {
    let links: Vec<LinkMessage> = handle
        .link()
        .get()
        .execute()
        .try_collect()
        .await
        .map_err(|e| e.to_string())?;
    let owned: Vec<_> = links
        .iter()
        .filter(|link| owned(link) || owned_transport(link, &links))
        .collect();
    for link in &owned {
        handle
            .link()
            .set(LinkUnspec::new_with_index(link.header.index).down().build())
            .execute()
            .await
            .map_err(|e| e.to_string())?;
    }
    futures::stream::iter(owned)
        .map(Ok::<_, String>)
        .try_for_each_concurrent(32, |link| async move {
            match handle.link().del(link.header.index).execute().await {
                Ok(()) => Ok(()),
                Err(rtnetlink::Error::NetlinkError(e))
                    if e.code.map(std::num::NonZeroI32::get) == Some(-libc::ENODEV) =>
                {
                    Ok(())
                }
                Err(e) => Err(e.to_string()),
            }
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use rtnetlink::{LinkBridge, LinkVeth, LinkVxlan};

    #[tokio::test]
    #[ignore = "requires root and an isolated root network namespace"]
    async fn recovery_removes_legacy_and_retiring_endpoints_but_preserves_docker_links() {
        let root = super::super::RtNetLinkHandle::new().unwrap();
        let handle = &root.handle;
        for name in ["br_9200_s", "docker0", "br_operator"] {
            handle
                .link()
                .add(LinkBridge::new(name).build())
                .execute()
                .await
                .unwrap();
        }
        let bridge = super::super::netlink::get_link_by_name(handle, "br_9200_s")
            .await
            .unwrap()
            .header
            .index;
        for (name, peer) in [
            ("ns_9200_s-out", "p1legacy"),
            ("vethabcdef0", "vethfedcba0"),
        ] {
            handle
                .link()
                .add(LinkVeth::new(name, peer).build())
                .execute()
                .await
                .unwrap();
        }
        let legacy = super::super::netlink::get_link_by_name(handle, "ns_9200_s-out")
            .await
            .unwrap()
            .header
            .index;
        handle
            .link()
            .set(
                LinkUnspec::new_with_index(legacy)
                    .controller(bridge)
                    .build(),
            )
            .execute()
            .await
            .unwrap();
        handle
            .link()
            .add(
                LinkVxlan::new("vxlan-ns_9200_s", 9200)
                    .local("192.0.2.1".parse().unwrap())
                    .remote("192.0.2.2".parse().unwrap())
                    .port(56666)
                    .controller(bridge)
                    .build(),
            )
            .execute()
            .await
            .unwrap();
        handle
            .link()
            .add(
                LinkVeth::new("br_1959201_s", "p1retiring")
                    .link_group(0x4e40_0000)
                    .build(),
            )
            .execute()
            .await
            .unwrap();
        recover(handle).await.unwrap();
        let remaining: Vec<LinkMessage> =
            handle.link().get().execute().try_collect().await.unwrap();
        for name in [
            "br_9200_s",
            "ns_9200_s-out",
            "vxlan-ns_9200_s",
            "p1legacy",
            "br_1959201_s",
            "p1retiring",
        ] {
            assert!(!remaining.iter().any(|link| super::name(link) == Some(name)));
        }
        for name in ["docker0", "br_operator", "vethabcdef0", "vethfedcba0"] {
            assert!(remaining.iter().any(|link| super::name(link) == Some(name)));
        }
    }
}
