//! Fixed XFRM-interface shards for multiplexed policy transports.
use super::netlink::get_link_by_name;
use futures::TryStreamExt;
use rtnetlink::{
    Handle, IpVersion, LinkUnspec, LinkXfrm, RouteMessageBuilder,
    packet_route::{
        route::RouteScope,
        rule::{RuleAction, RuleAttribute, RuleMessage},
    },
};
use std::{array, io, net::Ipv4Addr};
use tokio::sync::OnceCell;

pub(super) const COUNT: u32 = 64;
const IF_ID_PREFIX: u32 = 0x4e71_0000;
const TABLE_BASE: u32 = 0x4e00;
const PRIORITY_BASE: u32 = 12_000;
const ROUTE_MARK_PREFIX: u32 = 0x4e80_0000 | super::policy::types::XFRM_ROUTE_BIT;
const OWNED_MARK_MASK: u32 = 0xffc0_0000;
const SHARD_MARK_MASK: u32 = OWNED_MARK_MASK | (COUNT - 1);

struct Shards {
    handle: Handle,
    parent: u32,
    cells: [OnceCell<u32>; COUNT as usize],
}

static SHARDS: OnceCell<Shards> = OnceCell::const_new();

fn error(value: impl std::fmt::Display) -> io::Error {
    io::Error::other(value.to_string())
}

fn shard(id: u32) -> u32 {
    id & (COUNT - 1)
}

fn if_id(shard: u32) -> u32 {
    IF_ID_PREFIX | (shard + 1)
}

fn table(shard: u32) -> u32 {
    TABLE_BASE + shard
}

fn mark(shard: u32) -> u32 {
    ROUTE_MARK_PREFIX | shard
}

fn rule(
    handle: &Handle,
    priority: u32,
    mark: u32,
    mask: u32,
    action: RuleAction,
    table: Option<u32>,
) -> rtnetlink::RuleAddRequest<Ipv4Addr> {
    let mut request = handle
        .rule()
        .add()
        .v4()
        .priority(priority)
        .fw_mark(mark)
        .action(action);
    request
        .message_mut()
        .attributes
        .push(RuleAttribute::FwMask(mask));
    if let Some(table) = table {
        request = request.table_id(table);
    } else {
        request.message_mut().header.table = 0;
    }
    request
}

pub(super) async fn initialize(handle: &Handle, parent_name: &str) -> io::Result<()> {
    let parent = get_link_by_name(handle, parent_name)
        .await
        .map_err(|e| io::Error::other(e.to_str().to_owned()))?
        .header
        .index;
    rule(
        handle,
        PRIORITY_BASE + COUNT,
        ROUTE_MARK_PREFIX,
        OWNED_MARK_MASK,
        RuleAction::Unreachable,
        None,
    )
    .execute()
    .await
    .map_err(error)?;
    SHARDS
        .set(Shards {
            handle: handle.clone(),
            parent,
            cells: array::from_fn(|_| OnceCell::new()),
        })
        .map_err(|_| io::Error::other("XFRM shards are already initialized"))
}

pub(super) async fn ensure(id: u32) -> io::Result<u32> {
    let shards = SHARDS
        .get()
        .ok_or_else(|| io::Error::other("XFRM shards are not initialized"))?;
    let shard = shard(id);
    shards.cells[shard as usize]
        .get_or_try_init(|| async move {
            let name = format!("nn-xf-{shard}");
            shards
                .handle
                .link()
                .add(
                    LinkXfrm::new(&name, shards.parent, if_id(shard))
                        .link_group(super::policy::types::GROUP)
                        .build(),
                )
                .execute()
                .await
                .map_err(error)?;
            let index = get_link_by_name(&shards.handle, &name)
                .await
                .map_err(|e| io::Error::other(e.to_str().to_owned()))?
                .header
                .index;
            let setup = async {
                shards
                    .handle
                    .link()
                    .set(LinkUnspec::new_with_index(index).up().build())
                    .execute()
                    .await
                    .map_err(error)?;
                let route = RouteMessageBuilder::<Ipv4Addr>::new()
                    .table_id(table(shard))
                    .scope(RouteScope::Link)
                    .output_interface(index)
                    .build();
                shards
                    .handle
                    .route()
                    .add(route)
                    .execute()
                    .await
                    .map_err(error)?;
                rule(
                    &shards.handle,
                    PRIORITY_BASE + shard,
                    mark(shard),
                    SHARD_MARK_MASK,
                    RuleAction::ToTable,
                    Some(table(shard)),
                )
                .execute()
                .await
                .map_err(error)?;
                Ok(if_id(shard))
            }
            .await;
            if setup.is_err() {
                let _ = shards.handle.link().del(index).execute().await;
            }
            setup
        })
        .await
        .copied()
}

fn owned_rule(rule: &RuleMessage) -> bool {
    let mut priority = None;
    let mut mark = None;
    let mut mask = None;
    for attribute in &rule.attributes {
        match attribute {
            RuleAttribute::Priority(value) => priority = Some(*value),
            RuleAttribute::FwMark(value) => mark = Some(*value),
            RuleAttribute::FwMask(value) => mask = Some(*value),
            _ => {}
        }
    }
    priority.is_some_and(|value| (PRIORITY_BASE..=PRIORITY_BASE + COUNT).contains(&value))
        && mark.is_some_and(|value| value & OWNED_MARK_MASK == ROUTE_MARK_PREFIX)
        && mask.is_some_and(|value| value == OWNED_MARK_MASK || value == SHARD_MARK_MASK)
}

pub(super) async fn recover(handle: &Handle) -> Result<(), String> {
    let rules: Vec<_> = handle
        .rule()
        .get(IpVersion::V4)
        .execute()
        .try_collect()
        .await
        .map_err(|e| e.to_string())?;
    for rule in rules.into_iter().filter(owned_rule) {
        handle
            .rule()
            .del(rule)
            .execute()
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_distribute_across_fixed_shards() {
        assert_eq!(shard(0x20_0000), 0);
        assert_eq!(shard(0x20_003f), 63);
        assert_eq!(shard(0x20_0040), 0);
        assert_ne!(if_id(0), if_id(63));
    }
}
