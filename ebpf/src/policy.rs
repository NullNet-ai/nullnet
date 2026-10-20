//! Permission-gated IPv4 forwarding over shared policy transports.
use aya_ebpf::{
    EbpfContext,
    bindings::{TC_ACT_SHOT, bpf_tunnel_key},
    helpers::{
        bpf_ktime_get_ns, bpf_redirect, bpf_skb_get_tunnel_key, bpf_skb_get_tunnel_opt,
        bpf_skb_set_tunnel_key, bpf_skb_set_tunnel_opt,
    },
    macros::{classifier, map},
    maps::{Array, HashMap, LruHashMap},
    programs::TcContext,
};
use core::mem::{size_of, zeroed};

#[path = "../../shared/policy.rs"]
#[allow(dead_code)]
mod types;
use types::{APP_IP, FlowKey, OutKey, Rule, XFRM_ROUTE_BIT};

#[map]
static POLICY_OUT: HashMap<OutKey, Rule> = HashMap::with_max_entries(65536, 0);
#[map]
static POLICY_IN: HashMap<u32, Rule> = HashMap::with_max_entries(65536, 0);
#[map]
static POLICY_DEADLINE: Array<u64> = Array::with_max_entries(1, 0);
#[map]
static POLICY_FLOWS: LruHashMap<FlowKey, u64> = LruHashMap::with_max_entries(262144, 0);

fn alive() -> bool {
    POLICY_DEADLINE
        .get(0)
        .is_some_and(|deadline| *deadline > unsafe { bpf_ktime_get_ns() })
}

fn header(ctx: &TcContext) -> Result<(usize, u8), i64> {
    if u16::from_be(ctx.load::<u16>(12)?) != 0x0800 {
        return Err(-1);
    }
    let version = ctx.load::<u8>(14)?;
    let len = usize::from(version & 15) * 4;
    if version >> 4 != 4 || len < 20 || u16::from_be(ctx.load::<u16>(20)?) & 0x3fff != 0 {
        return Err(-1);
    }
    let proto = ctx.load::<u8>(23)?;
    if proto != 6 && proto != 17 && proto != 1 {
        return Err(-1);
    }
    Ok((len, proto))
}

fn translate(
    ctx: &TcContext,
    offset: usize,
    target: u32,
    len: usize,
    proto: u8,
) -> Result<(), i64> {
    let old = ctx.load::<u32>(offset)?;
    let new = target.to_be();
    ctx.l3_csum_replace(24, old as u64, new as u64, 4)?;
    let checksum = 14 + len + if proto == 6 { 16 } else { 6 };
    if proto == 6 || (proto == 17 && ctx.load::<u16>(checksum)? != 0) {
        ctx.l4_csum_replace(checksum, old as u64, new as u64, 4 | 16 | 32)?;
    }
    ctx.store(offset, &new, 0)
}

fn flow(
    ctx: &TcContext,
    rule: &Rule,
    interface: u32,
    len: usize,
    proto: u8,
    incoming: bool,
) -> Result<(), i64> {
    if proto != 6 {
        return Ok(());
    }
    let source = ctx.load::<u16>(14 + len)?;
    let dest = ctx.load::<u16>(16 + len)?;
    let flags = ctx.load::<u8>(27 + len)?;
    let key = FlowKey {
        interface,
        peer: rule.peer,
        local_port: if incoming { dest } else { source },
        remote_port: if incoming { source } else { dest },
    };
    if flags & 0x02 != 0 {
        POLICY_FLOWS.insert(key, rule.generation, 0)?;
    } else if unsafe { POLICY_FLOWS.get(key) }.copied() != Some(rule.generation) {
        return Err(-1);
    }
    Ok(())
}

#[classifier]
pub fn nullnet_policy_out(ctx: TcContext) -> i32 {
    outgoing(&ctx).unwrap_or(TC_ACT_SHOT)
}

fn outgoing(ctx: &TcContext) -> Result<i32, i64> {
    if !alive() {
        return Err(-1);
    }
    let (len, proto) = header(ctx)?;
    if u32::from_be(ctx.load::<u32>(26)?) != APP_IP {
        return Err(-1);
    }
    let interface = unsafe { (*ctx.skb.skb).ifindex };
    let key = OutKey {
        interface,
        peer: u32::from_be(ctx.load::<u32>(30)?),
    };
    let rule = unsafe { POLICY_OUT.get(key) }.copied().ok_or(-1)?;
    if rule.enabled == 0 {
        return Err(-1);
    }
    flow(ctx, &rule, interface, len, proto, false)?;
    translate(ctx, 26, rule.local, len, proto)?;

    if rule.underlay_local == rule.underlay_remote {
        translate(ctx, 30, APP_IP, len, proto)?;
        ctx.store(0, &rule.mac, 0)?;
        return Ok(unsafe { bpf_redirect(rule.output, 0) } as i32);
    }

    let mut tunnel: bpf_tunnel_key = unsafe { zeroed() };
    tunnel.tunnel_id = (rule.generation & 0xff_ffff) as u32;
    tunnel.tunnel_ttl = 64;
    tunnel.__bindgen_anon_1.remote_ipv4 = rule.underlay_remote;
    tunnel.__bindgen_anon_3.local_ipv4 = rule.underlay_local;
    let mut option = (rule.generation >> 24) as u32 | (1 << 22);
    let skb = ctx.as_ptr().cast();
    if unsafe { bpf_skb_set_tunnel_key(skb, &mut tunnel, size_of::<bpf_tunnel_key>() as u32, 2) }
        != 0
        || unsafe { bpf_skb_set_tunnel_opt(skb, (&mut option as *mut u32).cast(), 4) } != 0
    {
        return Err(-1);
    }
    ctx.set_mark(rule.mark | XFRM_ROUTE_BIT);
    Ok(unsafe { bpf_redirect(rule.transport, 0) } as i32)
}

#[classifier]
pub fn nullnet_policy_in(ctx: TcContext) -> i32 {
    incoming(&ctx).unwrap_or(TC_ACT_SHOT)
}

fn incoming(ctx: &TcContext) -> Result<i32, i64> {
    if !alive() {
        return Err(-1);
    }
    let (len, proto) = header(ctx)?;
    let local = u32::from_be(ctx.load::<u32>(30)?);
    let rule = unsafe { POLICY_IN.get(local) }.copied().ok_or(-1)?;
    let mut tunnel: bpf_tunnel_key = unsafe { zeroed() };
    let mut option = 0u32;
    let skb = ctx.as_ptr().cast();
    if unsafe { (*ctx.skb.skb).mark } != rule.mark
        || u32::from_be(ctx.load::<u32>(26)?) != rule.peer
        || unsafe {
            bpf_skb_get_tunnel_key(skb, &mut tunnel, size_of::<bpf_tunnel_key>() as u32, 0)
        } != 0
        || unsafe { bpf_skb_get_tunnel_opt(skb, (&mut option as *mut u32).cast(), 4) } != 4
    {
        return Err(-1);
    }
    if unsafe { tunnel.__bindgen_anon_1.remote_ipv4 } != rule.underlay_remote
        || (u64::from(option & 0xffff) << 24 | u64::from(tunnel.tunnel_id)) != rule.generation
    {
        return Err(-1);
    }
    flow(ctx, &rule, rule.output, len, proto, true)?;
    translate(ctx, 30, APP_IP, len, proto)?;
    ctx.store(0, &rule.mac, 0)?;
    ctx.set_mark(0);
    Ok(unsafe { bpf_redirect(rule.output, 0) } as i32)
}
