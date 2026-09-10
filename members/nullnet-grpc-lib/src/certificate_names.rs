/// Match a cert name (`san`) against a target `domain`: exact (case-insensitive)
/// or a `*.`-prefixed wildcard covering exactly one label.
pub fn name_matches(san: &str, domain: &str) -> bool {
    if san.eq_ignore_ascii_case(domain) {
        return true;
    }
    if let (Some(suffix), Some((_, parent))) = (san.strip_prefix("*."), domain.split_once('.')) {
        return parent.eq_ignore_ascii_case(suffix);
    }
    false
}
