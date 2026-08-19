//! Client-side egress country-policy state.
//!
//! Verdicts are decided by the server (`CheckEgressDestination`) and cached
//! here per `(container, dst_ip)`. The cache is consulted by the egress
//! NFQUEUE handler for the first packet of every NEW external flow; on a
//! server-pushed `EgressPolicyChanged` it is cleared and conntrack is flushed
//! so live flows re-enter the queue as NEW and get re-verdicted — flows the
//! new policy denies die on their next packet.

use crate::conntrack::EgressOpenFlows;
use crate::nfqueue::BridgeIpCache;
use nullnet_grpc_lib::NullnetGrpcInterface;
use nullnet_grpc_lib::nullnet_grpc::{
    AgentConntrackFlushFailed, AgentEvent, agent_event::Event as AgentEventKind,
};
use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long a cached verdict stays valid. Bounds staleness if the server's
/// policy-change push is missed (e.g. control channel down at reload time).
const VERDICT_TTL: Duration = Duration::from_secs(60);
/// Cap on cached verdicts; the least-recently-checked entry is evicted.
const MAX_VERDICTS: usize = 4096;

/// TTL'd `(container, dst_ip)` → allowed cache. Sync mutex; holds are brief
/// and never span an `.await`.
#[derive(Default)]
pub struct PolicyVerdicts {
    map: Mutex<HashMap<(String, Ipv4Addr), (bool, Instant)>>,
}

impl PolicyVerdicts {
    /// Cached verdict, or `None` if absent or older than `VERDICT_TTL`.
    pub fn get(&self, container: &str, dst_ip: Ipv4Addr) -> Option<bool> {
        let map = self.map.lock().unwrap();
        let (allowed, at) = map.get(&(container.to_string(), dst_ip))?;
        (at.elapsed() < VERDICT_TTL).then_some(*allowed)
    }

    pub fn put(&self, container: &str, dst_ip: Ipv4Addr, allowed: bool) {
        let key = (container.to_string(), dst_ip);
        let mut map = self.map.lock().unwrap();
        if map.len() >= MAX_VERDICTS
            && !map.contains_key(&key)
            && let Some(oldest) = map
                .iter()
                .min_by_key(|(_, (_, at))| *at)
                .map(|(k, _)| k.clone())
        {
            map.remove(&oldest);
        }
        map.insert(key, (allowed, Instant::now()));
    }

    pub fn clear(&self) {
        self.map.lock().unwrap().clear();
    }
}

/// Delete the conntrack entries originating from each container bridge IP so
/// every live flow re-enters the NFQUEUE as NEW and is re-verdicted. Exit
/// code 1 just means "no entries matched" — only real failures are logged.
/// How long a container's emptiness stays untrustworthy after we flush its
/// conntrack entries. Long enough for an active connection to carry a packet
/// and re-register through NFQUEUE; bounded so a genuinely idle container still
/// reaps. See `OpenFlows::suppress_for`.
const FLUSH_SUPPRESSION: Duration = Duration::from_secs(120);

pub async fn flush_container_conntrack(
    grpc: &NullnetGrpcInterface,
    ips: Vec<Ipv4Addr>,
    open_flows: &EgressOpenFlows,
    cache: &BridgeIpCache,
) {
    // Suppress BEFORE flushing, not after: the DESTROY events our own deletions
    // raise are indistinguishable from real closes, and with no grace window a
    // false zero is an immediate reap of a live edge.
    {
        let mut guard = open_flows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for ip in &ips {
            if let Some(container) = cache.get(*ip) {
                guard.suppress_for(container, FLUSH_SUPPRESSION);
            }
        }
    }

    for ip in ips {
        let out = tokio::process::Command::new("conntrack")
            .args(["-D", "-s", &ip.to_string()])
            .output()
            .await;
        // A failed flush leaves flows the new policy denies running until they
        // close on their own, so the policy change is only partly in force.
        let error_message = match out {
            Ok(o) if o.status.code() == Some(0) || o.status.code() == Some(1) => continue,
            Ok(o) => {
                let stderr = String::from_utf8_lossy(&o.stderr).trim().to_string();
                eprintln!(
                    "[egress-policy] conntrack -D -s {ip} exited {}: {stderr}",
                    o.status
                );
                format!("conntrack exited {}: {stderr}", o.status)
            }
            Err(e) => {
                eprintln!("[egress-policy] conntrack flush {ip}: {e} (is conntrack installed?)");
                format!("{e} (is conntrack installed?)")
            }
        };
        let grpc = grpc.clone();
        let event = AgentEvent {
            event: Some(AgentEventKind::ConntrackFlushFailed(
                AgentConntrackFlushFailed {
                    ip: ip.to_string(),
                    error_message,
                },
            )),
        };
        tokio::spawn(async move {
            let _ = grpc.report_event(event).await;
        });
    }
}
