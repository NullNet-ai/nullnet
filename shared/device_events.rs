//! Reserved virtual-device names; Docker and legacy OVS VLAN ports are excluded.
pub fn owned_name(name: &[u8], descendants: bool) -> bool {
    let end = name
        .iter()
        .position(|&b| b == 0 || (descendants && b == b'/'))
        .unwrap_or(name.len());
    let name = &name[..end];
    for prefix in [b"nnp_".as_slice(), b"nnb_", b"nnv_"] {
        if name.starts_with(prefix) {
            return true;
        }
    }
    for prefix in [b"br_".as_slice(), b"ns_", b"vxlan-ns_"] {
        if name.starts_with(prefix) && name.get(prefix.len()).is_some_and(u8::is_ascii_digit) {
            return true;
        }
    }
    for (prefix, separator) in [(b"veth-".as_slice(), true), (b"macsec-".as_slice(), false)] {
        if let Some(suffix) = name.strip_prefix(prefix) {
            let Some((&side, id)) = suffix.split_last() else {
                continue;
            };
            if side != b's' && side != b'c' {
                continue;
            }
            let id = if separator {
                let Some(id) = id.strip_suffix(b"-") else {
                    continue;
                };
                id
            } else {
                id
            };
            if !id.is_empty() && id.iter().all(u8::is_ascii_digit) {
                return true;
            }
        }
    }
    false
}
