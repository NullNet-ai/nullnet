//! Shared classifiers retain dedicated per-edge forwarding and encryption.
use aya_ebpf::{
    bindings::{TC_ACT_OK, TC_ACT_SHOT},
    helpers::bpf_redirect,
    macros::{classifier, map},
    maps::HashMap,
    programs::TcContext,
};

#[path = "../../shared/endpoint_tc.rs"]
mod config;
use config::EndpointConfig;

#[map]
static ENDPOINT_CONFIG: HashMap<u32, EndpointConfig> = HashMap::with_max_entries(65536, 0);

#[classifier]
pub fn nullnet_endpoint_ingress(ctx: TcContext) -> i32 {
    ingress(&ctx).unwrap_or(TC_ACT_SHOT)
}

#[inline(always)]
fn ingress(ctx: &TcContext) -> Option<i32> {
    let index = unsafe { (*ctx.skb.skb).ifindex };
    let config = unsafe { ENDPOINT_CONFIG.get(index) }?;
    let mark = unsafe { (*ctx.skb.skb).mark };
    let protocol = ctx.load::<[u8; 2]>(12).ok()?;
    let destination = match protocol {
        [8, 0] => ctx.load::<u32>(30).ok()?,
        [8, 6] => ctx.load::<u32>(38).ok()?,
        _ => 0,
    };
    if config.mark != 0 && mark != config.mark {
        return Some(TC_ACT_SHOT);
    }
    if destination == config.gateway || config.output == 0 {
        return Some(TC_ACT_OK);
    }
    Some(unsafe { bpf_redirect(config.output, 0) } as i32)
}

#[classifier]
pub fn nullnet_endpoint_egress(ctx: TcContext) -> i32 {
    let index = unsafe { (*ctx.skb.skb).ifindex };
    let Some(config) = (unsafe { ENDPOINT_CONFIG.get(index) }) else {
        return TC_ACT_SHOT;
    };
    if config.mark != 0 {
        ctx.set_mark(config.mark);
    }
    TC_ACT_OK
}
