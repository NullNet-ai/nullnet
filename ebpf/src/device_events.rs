//! Filter owned multicast notifications before host consumers are awakened.
use aya_ebpf::{
    helpers::{bpf_get_socket_cookie, bpf_ktime_get_ns},
    macros::{map, socket_filter},
    maps::{Array, LruHashMap, PerCpuArray},
    programs::SkBuffContext,
};

#[path = "../../shared/device_events.rs"]
mod names;

#[map]
static EVENT_LEASE: Array<u64> = Array::with_max_entries(1, 0);
#[map]
static EVENT_SOCKETS: LruHashMap<u64, u32> = LruHashMap::with_max_entries(65536, 0);
#[map]
static EVENT_OWNED: LruHashMap<u32, u32> = LruHashMap::with_max_entries(65536, 0);
#[map]
static EVENT_COUNTS: PerCpuArray<u64> = PerCpuArray::with_max_entries(5, 0);

#[inline(always)]
fn enabled() -> bool {
    EVENT_LEASE
        .get(0)
        .is_some_and(|deadline| unsafe { bpf_ktime_get_ns() } < *deadline)
}

#[inline(always)]
fn drop_event(kind: u32) -> i64 {
    if let Some(counter) = EVENT_COUNTS.get_ptr_mut(kind) {
        unsafe {
            *counter += 1;
        }
    }
    0
}

#[socket_filter]
pub fn nullnet_event_uevent(ctx: SkBuffContext) -> i64 {
    if !enabled() {
        return i64::from(u32::MAX);
    }
    match uevent(&ctx) {
        Some(true) => drop_event(0),
        _ => i64::from(u32::MAX),
    }
}

#[inline(always)]
fn uevent(ctx: &SkBuffContext) -> Option<bool> {
    let action = ctx.load::<[u8; 4]>(0).ok()?;
    let start = match &action {
        b"add@" => 4,
        b"remo" | b"chan" | b"onli" | b"unbi" => 7,
        b"move" | b"bind" => 5,
        b"offl" => 8,
        _ => return Some(false),
    };
    if ctx.load::<[u8; 21]>(start).ok()? != *b"/devices/virtual/net/" {
        return Some(false);
    }
    Some(names::owned_name(
        &ctx.load::<[u8; 16]>(start + 21).ok()?,
        true,
    ))
}

#[socket_filter]
pub fn nullnet_event_route(ctx: SkBuffContext) -> i64 {
    if !enabled() {
        return i64::from(u32::MAX);
    }
    match route(&ctx) {
        Some(kind) => drop_event(kind),
        _ => i64::from(u32::MAX),
    }
}

#[inline(always)]
fn route(ctx: &SkBuffContext) -> Option<u32> {
    let len = ctx.load::<u32>(0).ok()?;
    // Dumps and mixed datagrams pass, as do replies to this socket's requests.
    if len != ctx.len() || len < 32 || ctx.load::<u16>(6).ok()? & 2 != 0 {
        return None;
    }
    let cookie = unsafe { bpf_get_socket_cookie(ctx.skb.skb.cast()) };
    let recipient = unsafe { EVENT_SOCKETS.get(cookie) }.copied()?;
    let sender = ctx.load::<u32>(12).ok()?;
    if sender == recipient || (sender == 0 && ctx.load::<u32>(8).ok()? != 0) {
        return None;
    }
    match ctx.load::<u16>(4).ok()? {
        16 | 17 => {
            if ctx.load::<u16>(34).ok()? != 3 {
                return None;
            }
            let attr_len = ctx.load::<u16>(32).ok()?;
            if !(5..=20).contains(&attr_len) || 32 + u32::from(attr_len) > len {
                return None;
            }
            let mut name = [0u8; 16];
            // Short DEL notifications need not contain sixteen name bytes.
            for (i, byte) in name.iter_mut().enumerate() {
                if i + 4 >= usize::from(attr_len) {
                    break;
                }
                *byte = ctx.load::<u8>(36 + i).ok()?;
            }
            let index = ctx.load::<u32>(20).ok()?;
            if names::owned_name(&name, false) {
                EVENT_OWNED.insert(index, 1, 0).ok()?;
                Some(1)
            } else {
                let _ = EVENT_OWNED.remove(index);
                None
            }
        }
        20 | 21 => owned_index(ctx.load::<u32>(20).ok()?, 2),
        28 | 29 => owned_index(ctx.load::<u32>(20).ok()?, 4),
        24 | 25 => {
            let mut offset = 28usize;
            for _ in 0..32 {
                let size = usize::from(ctx.load::<u16>(offset).ok()?);
                if size < 4 || offset + size > len as usize {
                    return None;
                }
                match ctx.load::<u16>(offset + 2).ok()? {
                    4 if size == 8 => return owned_index(ctx.load::<u32>(offset + 4).ok()?, 3),
                    9 => return None,
                    _ => (),
                }
                offset += (size + 3) & !3;
                if offset >= len as usize {
                    return None;
                }
            }
            None
        }
        _ => None,
    }
}

#[inline(always)]
fn owned_index(index: u32, kind: u32) -> Option<u32> {
    unsafe { EVENT_OWNED.get(index) }.map(|_| kind)
}
