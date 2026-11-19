use futures::TryStreamExt;
use ipnetwork::Ipv4Network;
use rtnetlink::packet_route::{
    address::AddressAttribute,
    route::{RouteAddress, RouteAttribute, RouteHeader, RouteType},
};
use rtnetlink::{Handle, RouteMessageBuilder};
use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr};

pub(crate) async fn collect(handle: &Handle) -> Result<Vec<String>, String> {
    let links = handle
        .link()
        .get()
        .execute()
        .try_collect::<Vec<_>>()
        .await
        .map_err(|error| error.to_string())?;
    let owned = super::vxlan_recovery::owned_indices(&links);
    let mut conflicts = BTreeSet::new();

    let mut addresses = handle.address().get().execute();
    while let Some(message) = addresses
        .try_next()
        .await
        .map_err(|error| error.to_string())?
    {
        if owned.contains(&message.header.index) {
            continue;
        }
        let address = message
            .attributes
            .iter()
            .find_map(|attribute| match attribute {
                AddressAttribute::Local(IpAddr::V4(ip))
                | AddressAttribute::Address(IpAddr::V4(ip)) => Some(*ip),
                _ => None,
            });
        if let Some(address) = address {
            insert(&mut conflicts, address, message.header.prefix_len);
        }
    }

    let mut request = RouteMessageBuilder::<Ipv4Addr>::new().build();
    request.header.table = RouteHeader::RT_TABLE_UNSPEC;
    let mut routes = handle.route().get(request).execute();
    while let Some(message) = routes.try_next().await.map_err(|error| error.to_string())? {
        if !matches!(
            message.header.kind,
            RouteType::Unicast
                | RouteType::Local
                | RouteType::BlackHole
                | RouteType::Unreachable
                | RouteType::Prohibit
        ) || message.attributes.iter().any(
            |attribute| matches!(attribute, RouteAttribute::Oif(index) if owned.contains(index)),
        ) {
            continue;
        }
        let destination = message
            .attributes
            .iter()
            .find_map(|attribute| match attribute {
                RouteAttribute::Destination(RouteAddress::Inet(ip)) => Some(*ip),
                _ => None,
            });
        if let Some(destination) = destination {
            insert(
                &mut conflicts,
                destination,
                message.header.destination_prefix_length,
            );
        }
    }

    Ok(conflicts.into_iter().collect())
}

fn insert(conflicts: &mut BTreeSet<String>, address: Ipv4Addr, prefix: u8) {
    if prefix < 8 {
        return;
    }
    let Ok(network) = Ipv4Network::new(address, prefix) else {
        return;
    };
    if network.network().octets()[0] == 10 {
        conflicts.insert(format!("{}/{prefix}", network.network()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_explicit_ten_space_prefixes_but_not_defaults() {
        let mut conflicts = BTreeSet::new();
        insert(&mut conflicts, Ipv4Addr::new(10, 0, 7, 3), 24);
        insert(&mut conflicts, Ipv4Addr::new(10, 1, 2, 3), 32);
        insert(&mut conflicts, Ipv4Addr::UNSPECIFIED, 0);
        insert(&mut conflicts, Ipv4Addr::new(172, 18, 0, 3), 16);

        assert_eq!(
            conflicts,
            BTreeSet::from(["10.0.7.0/24".into(), "10.1.2.3/32".into()])
        );
    }
}
