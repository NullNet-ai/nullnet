//! Per-interface configuration shared by userspace and the TC classifiers.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct EndpointConfig {
    pub gateway: u32,
    pub output: u32,
    pub mark: u32,
    pub padding: u32,
}
