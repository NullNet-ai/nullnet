//! Wire-independent layouts for policy dataplane maps.
pub const APP_IP: u32 = 0xa9fe_f002;
pub const GATEWAY_IP: u32 = 0xa9fe_f001;
pub const GROUP_PORT: u16 = 4791;
pub const GROUP: u32 = 0x4e70_0000;
pub const MAX_GENERATION: u64 = (1 << 40) - 1;

#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct OutKey {
    pub interface: u32,
    pub peer: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Rule {
    pub generation: u64,
    pub vni: u32,
    pub local: u32,
    pub peer: u32,
    pub underlay_local: u32,
    pub underlay_remote: u32,
    pub transport: u32,
    pub mark: u32,
    pub output: u32,
    pub enabled: u32,
    pub mac: [u8; 6],
    pub pad: [u8; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct FlowKey {
    pub interface: u32,
    pub peer: u32,
    pub local_port: u16,
    pub remote_port: u16,
}
