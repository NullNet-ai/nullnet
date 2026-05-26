#![no_std]
#![no_main]

use aya_ebpf::{bindings::TC_ACT_SHOT, macros::classifier, programs::TcContext};

/// Unconditional drop classifier. Currently defined but not attached anywhere;
/// kept as the seed for a future "block traffic not associated with a nullnet
/// flow" feature that will hook this (gated by a policy) somewhere on the
/// host's network path. Trigger detection has moved to the userspace NFQUEUE
/// listener — see members/nullnet-client/src/nfqueue/.
#[classifier]
pub fn nullnet_drop(_ctx: TcContext) -> i32 {
    TC_ACT_SHOT
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    unsafe { core::hint::unreachable_unchecked() }
}
