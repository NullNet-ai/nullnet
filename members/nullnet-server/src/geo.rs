//! IP → geo/ASN enrichment for contacted egress destinations.
//!
//! A process-wide, IP-keyed cache in front of `nullnet-libipinfo`. Every distinct
//! contacted IP is looked up **at most once** (successes and failures are both
//! cached), so an API provider is never charged twice for the same address — the
//! central point that keeps credit use bounded no matter how many services/nodes
//! contact the same host. Lookups are async and fire-and-forget: `ensure` returns
//! immediately, the result lands in the cache within ~1s, and the graph render
//! joins it in on the next poll.
//!
//! Provider is env-driven (see `build_handler`): an API provider when
//! `IPINFO_API_URL` is set, otherwise libipinfo's free db-ip.com fallback so the
//! pipeline works with zero config. This same cache is intended to back the
//! future per-service country egress policy.

use nullnet_libipinfo::IpInfoHandler;
#[cfg(not(test))]
use nullnet_libipinfo::{ApiFields, IpInfoProvider};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};

/// The subset of `nullnet_libipinfo::IpInfo` we surface. `country_code` is
/// whichever field the provider maps to `country` (configure it to return the
/// ISO alpha-2 code so the UI can render a flag); `org` is the ASN org name.
#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct GeoInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) country_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) asn: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) org: Option<String>,
}

impl GeoInfo {
    fn is_empty(&self) -> bool {
        self.country_code.is_none() && self.asn.is_none() && self.org.is_none()
    }
}

/// Upper bound on a single provider lookup. Without it a blackholed geo
/// endpoint would hang the awaiting caller (a held egress first-packet)
/// indefinitely; `nullnet-libipinfo`/reqwest sets no default request timeout.
const LOOKUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Removes `ip` from the in-flight set when dropped, so a lookup future that is
/// cancelled (e.g. the gRPC caller disconnects mid-lookup) or panics can't pin
/// the IP as permanently in-flight — which would make every later lookup miss,
/// stall, and report the IP as unknown, silently defeating a country policy.
struct InflightGuard {
    inflight: Arc<Mutex<HashSet<Ipv4Addr>>>,
    ip: Ipv4Addr,
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        self.inflight.lock().unwrap().remove(&self.ip);
    }
}

/// One provider lookup, bounded by `LOOKUP_TIMEOUT`; error and timeout both map
/// to an empty result (caller decides caching + policy on "unknown").
async fn lookup_geo(handler: &IpInfoHandler, ip: Ipv4Addr) -> GeoInfo {
    match tokio::time::timeout(LOOKUP_TIMEOUT, handler.lookup(&ip.to_string())).await {
        Ok(Ok(i)) => GeoInfo {
            country_code: i.country,
            asn: i.asn,
            org: i.org,
        },
        Ok(Err(e)) => {
            eprintln!("[geo] lookup {ip} failed: {e:?}");
            GeoInfo::default()
        }
        Err(_) => {
            eprintln!("[geo] lookup {ip} timed out after {LOOKUP_TIMEOUT:?}");
            GeoInfo::default()
        }
    }
}

#[derive(Clone)]
pub(crate) struct GeoCache {
    /// `None` if the handler failed to init — `ensure`/`get` then no-op.
    handler: Option<Arc<IpInfoHandler>>,
    /// Presence = "looked up". Empty results are cached only on the API path
    /// (see `cache_empty`); the free fallback leaves them uncached to retry.
    cache: Arc<Mutex<HashMap<Ipv4Addr, GeoInfo>>>,
    /// IPs with a lookup in flight, so concurrent `ensure`s collapse to one call.
    inflight: Arc<Mutex<HashSet<Ipv4Addr>>>,
    /// Cache empty/failed lookups? True on the API path (bounds credit use);
    /// false on the db-ip fallback, whose dbs load async at startup — caching an
    /// empty during that blind window pins the miss forever, and re-reads are free.
    cache_empty: bool,
}

// `IpInfoHandler` isn't `Debug`; `Orchestrator` derives it, so provide a terse one.
impl std::fmt::Debug for GeoCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeoCache")
            .field("enabled", &self.handler.is_some())
            .field("cached", &self.cache.lock().unwrap().len())
            .finish()
    }
}

impl GeoCache {
    pub(crate) fn from_env() -> Self {
        #[cfg(not(test))]
        let (handler, cache_empty) = build_handler();
        #[cfg(test)]
        let (handler, cache_empty) = (None, false);
        Self {
            handler: handler.map(Arc::new),
            cache: Arc::new(Mutex::new(HashMap::new())),
            inflight: Arc::new(Mutex::new(HashSet::new())),
            cache_empty,
        }
    }

    /// Ensure `ip` gets enriched: no-op if already looked up or in flight,
    /// otherwise spawn a single async lookup that populates the cache. Cheap and
    /// non-blocking — safe to call on every recorded destination.
    pub(crate) fn ensure(&self, ip: Ipv4Addr) {
        let Some(handler) = &self.handler else { return };
        if self.cache.lock().unwrap().contains_key(&ip) {
            return;
        }
        if !self.inflight.lock().unwrap().insert(ip) {
            return; // a lookup for this IP is already running
        }
        let handler = handler.clone();
        let cache = self.cache.clone();
        let inflight = self.inflight.clone();
        let cache_empty = self.cache_empty;
        tokio::spawn(async move {
            let _guard = InflightGuard { inflight, ip };
            let info = lookup_geo(&handler, ip).await;
            if !info.is_empty() || cache_empty {
                cache.lock().unwrap().insert(ip, info);
            }
        });
    }

    /// Cached geo for `ip`, or `None` if not looked up yet or the lookup yielded
    /// nothing (an all-empty result is reported as `None` to keep JSON clean).
    pub(crate) fn get(&self, ip: Ipv4Addr) -> Option<GeoInfo> {
        let info = self.cache.lock().unwrap().get(&ip).cloned()?;
        (!info.is_empty()).then_some(info)
    }

    /// Like `ensure` + `get`, but awaits the result — for the egress policy
    /// check, which must know the country before verdicting a held packet.
    /// Same discipline (one lookup per IP ever); waits on a lookup already in
    /// flight, giving up after ~5s (`None` = unknown, policy decides).
    pub(crate) async fn lookup_now(&self, ip: Ipv4Addr) -> Option<GeoInfo> {
        let handler = self.handler.as_ref()?.clone();
        for _ in 0..100 {
            if let Some(info) = self.cache.lock().unwrap().get(&ip).cloned() {
                return (!info.is_empty()).then_some(info);
            }
            if self.inflight.lock().unwrap().insert(ip) {
                let _guard = InflightGuard {
                    inflight: self.inflight.clone(),
                    ip,
                };
                let info = lookup_geo(&handler, ip).await;
                if !info.is_empty() || self.cache_empty {
                    self.cache.lock().unwrap().insert(ip, info.clone());
                }
                return (!info.is_empty()).then_some(info);
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        None
    }
}

/// Read a JSON-field-name env override, leaked to `'static` (ApiFields needs
/// `&'static str`). Leaks once at startup — the mapping lives for the whole run.
#[cfg(not(test))]
fn leak_env(var: &str, default: &'static str) -> &'static str {
    match std::env::var(var) {
        Ok(v) if !v.trim().is_empty() => Box::leak(v.trim().to_string().into_boxed_str()),
        _ => default,
    }
}

/// Build the handler from env. `IPINFO_API_URL` (with `{ip}`/`{api_key}`
/// placeholders) + `IPINFO_API_KEY` select an API provider; the
/// `IPINFO_FIELD_{COUNTRY,ASN,ORG}` vars map its JSON response fields. Unset →
/// libipinfo's free db-ip.com fallback. Returns `(handler, is_api)`; `is_api`
/// drives whether empty lookups are cached (see `GeoCache::cache_empty`).
#[cfg(not(test))]
fn build_handler() -> (Option<IpInfoHandler>, bool) {
    let (providers, is_api) = match std::env::var("IPINFO_API_URL") {
        Ok(url) if !url.trim().is_empty() => {
            let api_key = std::env::var("IPINFO_API_KEY").unwrap_or_default();
            // ApiFields are JSON Pointers — the leading slash is required, or
            // extraction silently yields nothing.
            let fields = ApiFields {
                country: Some(leak_env("IPINFO_FIELD_COUNTRY", "/country")),
                asn: Some(leak_env("IPINFO_FIELD_ASN", "/asn")),
                org: Some(leak_env("IPINFO_FIELD_ORG", "/org")),
                continent_code: None,
                city: None,
                region: None,
                postal: None,
                timezone: None,
            };
            println!("[geo] using API provider {}", url.trim());
            (
                vec![IpInfoProvider::new_api_provider(
                    url.trim(),
                    &api_key,
                    fields,
                )],
                true,
            )
        }
        _ => {
            println!("[geo] IPINFO_API_URL not set; using free db-ip.com fallback");
            (Vec::new(), false)
        }
    };
    let handler = match IpInfoHandler::new(providers) {
        Ok(h) => Some(h),
        Err(e) => {
            eprintln!("[geo] IpInfoHandler init failed: {e:?}; enrichment disabled");
            None
        }
    };
    (handler, is_api)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unit_test_geo_never_starts_external_provider_work() {
        let cache = GeoCache::from_env();
        assert!(cache.handler.is_none());
        let ip = Ipv4Addr::new(1, 1, 1, 1);
        cache.ensure(ip);
        assert!(cache.lookup_now(ip).await.is_none());
        assert!(cache.inflight.lock().unwrap().is_empty());
    }
}
