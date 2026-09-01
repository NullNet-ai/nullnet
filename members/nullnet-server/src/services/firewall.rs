//! Per-service traffic filter rules (issue #143): a generalization of the old
//! country-only egress/ingress policy to arbitrary AND/OR combinations of
//! Country, Org, Src IP (ingress) and Dst IP (egress), evaluated via the
//! `rpn-predicate-interpreter` postfix expression engine — the same library
//! and evaluation pattern `appguard-server/src/firewall/` already uses.
//!
//! Rules are modeled as OR-of-AND groups (`FilterPolicy::Block`/`Allow`'s
//! `groups: Vec<Vec<FilterRule>>` — each inner `Vec` is a group whose rules
//! must all match; groups are OR'ed together). The postfix expression is
//! built directly from that shape at evaluation time — no infix parsing is
//! needed since this is the only shape ever constructed.

use crate::geo::GeoInfo;
use async_trait::async_trait;
use ipnetwork::IpNetwork;
use nullnet_liberror::{Error, ErrorHandler, Location, location};
use rpn_predicate_interpreter::{Operator, PostfixExpression, PostfixToken, PredicateEvaluator};
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr};
use std::str::FromStr;

/// Which direction a `FilterPolicy` applies to — shapes which fields are
/// valid (`SrcIp` is ingress-only, `DstIp` is egress-only).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Direction {
    Egress,
    Ingress,
}

impl Direction {
    fn label(self) -> &'static str {
        match self {
            Direction::Egress => "egress",
            Direction::Ingress => "ingress",
        }
    }
}

/// Matchable fields for a traffic filter rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FilterField {
    Country,
    /// The peer's ASN organization name — matched rather than the ASN itself
    /// because the org is what the topology and Sessions views display, so a
    /// rule is written against the string the operator can actually see.
    Org,
    /// Ingress only: the proxy client's source address.
    SrcIp,
    /// Egress only: the contacted destination address.
    DstIp,
}

/// `Equal`/`NotEqual` — the field value is/isn't one of `values` (Country/Org).
/// `Contains`/`NotContains` — the address is/isn't within any CIDR in
/// `values` (`SrcIp`/`DstIp`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FilterCondition {
    Equal,
    NotEqual,
    Contains,
    NotContains,
}

impl FilterCondition {
    /// Case-insensitive: the geo provider's casing for country/org strings
    /// isn't guaranteed, and `validate` already uppercases country values purely
    /// for display consistency, not as a matching contract.
    fn compare_str(self, left: &str, right: &[String]) -> bool {
        match self {
            Self::Equal => right.iter().any(|v| v.eq_ignore_ascii_case(left)),
            Self::NotEqual => right.iter().all(|v| !v.eq_ignore_ascii_case(left)),
            // Rejected at validation time for Country/Org — never reached.
            Self::Contains | Self::NotContains => false,
        }
    }

    fn compare_ip(self, ip: Ipv4Addr, values: &[String]) -> bool {
        let addr = IpAddr::V4(ip);
        let nets = values.iter().filter_map(|v| IpNetwork::from_str(v).ok());
        match self {
            Self::Contains => nets.into_iter().any(|n| n.contains(addr)),
            // `nets` is consumed by the first arm's `any`; a fresh iterator is
            // built per match arm, so this is fine despite the shared name.
            Self::NotContains => values
                .iter()
                .filter_map(|v| IpNetwork::from_str(v).ok())
                .all(|n| !n.contains(addr)),
            // Rejected at validation time for SrcIp/DstIp — never reached.
            Self::Equal | Self::NotEqual => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct FilterRule {
    pub(crate) field: FilterField,
    pub(crate) condition: FilterCondition,
    pub(crate) values: Vec<String>,
}

/// Per-service traffic filter (egress or ingress — see `ServiceInfo`). A
/// drop-in generalization of the old country-only `CountryPolicy`: `Block`/
/// `Allow` now match an arbitrary AND/OR combination of fields instead of a
/// single country list, but the outer semantics are unchanged — `Block`
/// denies a match (no match → allow); `Allow` permits only a match (no
/// match → deny).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FilterPolicy {
    #[default]
    None,
    Block {
        groups: Vec<Vec<FilterRule>>,
    },
    Allow {
        groups: Vec<Vec<FilterRule>>,
    },
}

impl FilterPolicy {
    /// Used as a `#[serde(skip_serializing_if = "...")]` predicate so a
    /// service with no filter round-trips through TOML without a stray
    /// `egress_filter = "none"` line.
    pub(crate) fn is_none(&self) -> bool {
        matches!(self, FilterPolicy::None)
    }

    /// Validate this policy for `direction` and normalize it in place
    /// (`Country` values uppercased so runtime comparison is a plain `==`).
    /// Rejects a field that doesn't apply to `direction`, an empty group, a
    /// rule with no values, and an unparsable `SrcIp`/`DstIp` CIDR/address.
    pub(crate) fn validate(&mut self, direction: Direction, service: &str) -> Result<(), Error> {
        let groups = match self {
            FilterPolicy::None => return Ok(()),
            FilterPolicy::Block { groups } | FilterPolicy::Allow { groups } => groups,
        };
        if groups.is_empty() || groups.iter().any(Vec::is_empty) {
            return Err(format!(
                "service '{service}': {} filter has an empty rule group",
                direction.label()
            ))
            .handle_err(location!());
        }
        for rule in groups.iter_mut().flatten() {
            if rule.values.is_empty() {
                return Err(format!(
                    "service '{service}': {} filter rule has no values",
                    direction.label()
                ))
                .handle_err(location!());
            }
            match rule.field {
                FilterField::SrcIp if direction == Direction::Egress => {
                    return Err(format!(
                        "service '{service}': 'src_ip' only applies to ingress filters"
                    ))
                    .handle_err(location!());
                }
                FilterField::DstIp if direction == Direction::Ingress => {
                    return Err(format!(
                        "service '{service}': 'dst_ip' only applies to egress filters"
                    ))
                    .handle_err(location!());
                }
                FilterField::Country => {
                    for v in &mut rule.values {
                        *v = v.to_uppercase();
                    }
                }
                FilterField::SrcIp | FilterField::DstIp => {
                    for v in &rule.values {
                        IpNetwork::from_str(v)
                            .map_err(|_| format!("service '{service}': invalid CIDR/address '{v}'"))
                            .handle_err(location!())?;
                    }
                }
                FilterField::Org => {}
            }
        }
        Ok(())
    }

    /// Whether traffic described by `ctx` may pass under this policy.
    pub(crate) async fn allows(&self, ctx: &FilterContext) -> bool {
        match self {
            FilterPolicy::None => true,
            FilterPolicy::Block { groups } => !matches(groups, ctx).await,
            FilterPolicy::Allow { groups } => matches(groups, ctx).await,
        }
    }
}

/// Build the canonical OR-of-ANDs postfix expression for `groups` and
/// evaluate it against `ctx`. `groups` is validated non-empty (with
/// non-empty inner groups) by `FilterPolicy::validate` before it's ever
/// persisted, so `to_postfix` only fails here on a bug in that guarantee —
/// treated as "no match" rather than a panic.
async fn matches(groups: &[Vec<FilterRule>], ctx: &FilterContext) -> bool {
    let Some(expr) = to_postfix(groups) else {
        return false;
    };
    expr.evaluate(ctx, &()).await.0
}

fn to_postfix(groups: &[Vec<FilterRule>]) -> Option<PostfixExpression<FilterRule>> {
    let mut tokens = Vec::new();
    for (gi, group) in groups.iter().enumerate() {
        if group.is_empty() {
            return None;
        }
        for (ri, rule) in group.iter().enumerate() {
            tokens.push(PostfixToken::Predicate(rule.clone()));
            if ri > 0 {
                tokens.push(PostfixToken::Operator(Operator::And));
            }
        }
        if gi > 0 {
            tokens.push(PostfixToken::Operator(Operator::Or));
        }
    }
    PostfixExpression::from_tokens(tokens)
}

/// Everything a filter rule can be evaluated against: the address in question
/// (dst IP for egress, client IP for ingress) and its resolved geo data, if
/// any (cached, at-most-once-per-IP lookup — see `crate::geo`).
pub(crate) struct FilterContext {
    pub(crate) ip: Ipv4Addr,
    pub(crate) geo: Option<GeoInfo>,
}

#[async_trait]
impl PredicateEvaluator for FilterContext {
    type Predicate = FilterRule;
    type Reason = String;
    type Context = ();

    async fn evaluate_predicate(&self, predicate: &FilterRule, _context: &()) -> bool {
        match predicate.field {
            FilterField::Country => self
                .geo
                .as_ref()
                .and_then(|g| g.country_code.as_deref())
                .is_some_and(|c| predicate.condition.compare_str(c, &predicate.values)),
            FilterField::Org => self
                .geo
                .as_ref()
                .and_then(|g| g.org.as_deref())
                .is_some_and(|o| predicate.condition.compare_str(o, &predicate.values)),
            FilterField::SrcIp | FilterField::DstIp => {
                predicate.condition.compare_ip(self.ip, &predicate.values)
            }
        }
    }

    fn get_reason(&self, predicate: &FilterRule) -> String {
        serde_json::to_string(predicate).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(field: FilterField, condition: FilterCondition, values: &[&str]) -> FilterRule {
        FilterRule {
            field,
            condition,
            values: values.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    fn ctx(ip: &str, country: Option<&str>, org: Option<&str>) -> FilterContext {
        FilterContext {
            ip: ip.parse().unwrap(),
            geo: Some(GeoInfo {
                country_code: country.map(String::from),
                asn: None,
                org: org.map(String::from),
            }),
        }
    }

    #[tokio::test]
    async fn block_matches_any_group_denies() {
        let policy = FilterPolicy::Block {
            groups: vec![
                vec![rule(FilterField::Country, FilterCondition::Equal, &["US"])],
                vec![rule(
                    FilterField::Org,
                    FilterCondition::Equal,
                    &["Evil Corp"],
                )],
            ],
        };
        assert!(
            !policy
                .allows(&ctx("1.1.1.1", Some("US"), Some("Good Corp")))
                .await
        );
        assert!(
            !policy
                .allows(&ctx("1.1.1.1", Some("CA"), Some("evil corp")))
                .await
        );
        assert!(
            policy
                .allows(&ctx("1.1.1.1", Some("CA"), Some("Good Corp")))
                .await
        );
    }

    #[tokio::test]
    async fn allow_requires_all_conditions_in_a_group() {
        let policy = FilterPolicy::Allow {
            groups: vec![vec![
                rule(FilterField::Country, FilterCondition::Equal, &["US"]),
                rule(FilterField::Org, FilterCondition::NotEqual, &["Evil Corp"]),
            ]],
        };
        assert!(
            policy
                .allows(&ctx("1.1.1.1", Some("US"), Some("Good Corp")))
                .await
        );
        assert!(
            !policy
                .allows(&ctx("1.1.1.1", Some("US"), Some("evil corp")))
                .await
        );
        assert!(
            !policy
                .allows(&ctx("1.1.1.1", Some("CA"), Some("Good Corp")))
                .await
        );
    }

    #[tokio::test]
    async fn dst_ip_cidr_containment() {
        let policy = FilterPolicy::Block {
            groups: vec![vec![rule(
                FilterField::DstIp,
                FilterCondition::Contains,
                &["10.0.0.0/8"],
            )]],
        };
        assert!(!policy.allows(&ctx("10.1.2.3", None, None)).await);
        assert!(policy.allows(&ctx("8.8.8.8", None, None)).await);
    }

    #[test]
    fn validate_rejects_wrong_direction_field() {
        let mut policy = FilterPolicy::Block {
            groups: vec![vec![rule(
                FilterField::SrcIp,
                FilterCondition::Contains,
                &["1.2.3.0/24"],
            )]],
        };
        assert!(policy.validate(Direction::Egress, "svc").is_err());
        assert!(policy.validate(Direction::Ingress, "svc").is_ok());
    }

    #[test]
    fn validate_uppercases_country_values() {
        let mut policy = FilterPolicy::Allow {
            groups: vec![vec![rule(
                FilterField::Country,
                FilterCondition::Equal,
                &["us"],
            )]],
        };
        policy.validate(Direction::Egress, "svc").unwrap();
        let FilterPolicy::Allow { groups } = &policy else {
            unreachable!()
        };
        assert_eq!(groups[0][0].values, vec!["US".to_string()]);
    }
}
