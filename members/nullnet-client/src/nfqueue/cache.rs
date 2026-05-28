use std::collections::{HashMap, HashSet};
use std::net::Ipv4Addr;
use std::process::Stdio;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};

/// First 12 hex chars of `NetworkSettings.SandboxID` are what Docker uses
/// in `gateway_<id>` endpoint names on docker_gwbridge — that's the join
/// key for resolving Swarm task sandboxes back to their owning container.
const SANDBOX_PREFIX_LEN: usize = 12;

/// Upper bound on how long any `docker` subprocess in this module is
/// allowed to take. The `docker` CLI normally returns in well under 100 ms;
/// 5 s is room for an unusually slow daemon plus headroom. If the daemon
/// hangs entirely we'd rather log + bail than stall the events watcher,
/// which would freeze the cache forever.
const DOCKER_SUBPROCESS_TIMEOUT: Duration = Duration::from_secs(5);

/// Bridge-IP → container-name lookup the NFQUEUE listener consults on every
/// queued packet. Populated by enumerating every Docker network and joining
/// each endpoint back to its owning container, and kept fresh by an async
/// watcher subscribed to `docker events`. Lock holds are brief; readers
/// (per-packet) never block writers for long.
#[derive(Clone, Default)]
pub struct BridgeIpCache {
    inner: Arc<RwLock<HashMap<Ipv4Addr, String>>>,
}

impl BridgeIpCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, ip: Ipv4Addr) -> Option<String> {
        self.inner.read().unwrap().get(&ip).cloned()
    }

    fn replace(&self, map: HashMap<Ipv4Addr, String>) {
        *self.inner.write().unwrap() = map;
    }

    /// One-shot refresh: rebuild the map from the current Docker state.
    /// Atomic swap; readers either see the old map or the new map, never a
    /// half-built one.
    pub async fn refresh(&self) {
        match query_docker().await {
            Ok(map) => {
                let size = map.len();
                self.replace(map);
                println!("[nfqueue/cache] refresh: {size} container IP(s) loaded");
            }
            Err(e) => {
                eprintln!("[nfqueue/cache] refresh failed: {e}");
            }
        }
    }
}

/// Index built from `docker inspect` of running containers, used to resolve
/// network-endpoint names back to container names while enumerating
/// networks.
struct ContainerIndex {
    /// Container names (leading `/` stripped). An endpoint whose `Name`
    /// matches one of these resolves directly to that container.
    names: HashSet<String>,
    /// `SandboxID[..12]` → container name. Resolves the `gateway_<id>`
    /// endpoints docker_gwbridge uses for Swarm task sandboxes — those
    /// sandboxes are attached at the libnetwork level and never appear in
    /// a container's own `.NetworkSettings.Networks`.
    sandbox12_to_name: HashMap<String, String>,
}

/// Build the cache by enumerating every Docker **network** rather than every
/// container. Container-POV inspection (`.NetworkSettings.Networks`) is
/// structurally incomplete: Swarm tasks bind to `docker_gwbridge` at the
/// libnetwork sandbox level, and that attachment never shows up under the
/// container's declared networks — so traffic exiting via gwbridge would
/// always miss the cache. Network-POV inspection (`.Containers`) is the
/// network's own list of attachments and is complete by libnetwork's
/// contract, regardless of network type (bridge, overlay, gwbridge,
/// macvlan, ipvlan, user-defined).
///
/// Endpoint names returned by `docker network inspect` come in three shapes
/// we have to map back to a container:
///   1. `Name = "<container-name>"` — plain Docker, Compose, user-defined
///      bridges, Swarm tasks on their overlays. Direct hit.
///   2. `Name = "gateway_<sandboxid12>"` — Swarm task sandbox on
///      `docker_gwbridge`. Joined via the first 12 hex chars of the
///      container's `NetworkSettings.SandboxID`.
///   3. `Name = "gateway_ingress-sbox"` or `Name = "lb-*"` — Swarm ingress
///      / overlay load-balancer sandboxes. These have no single owning
///      task; whatever traffic comes out of them has already been SNAT'd
///      from the real initiator. Skipped intentionally — the original
///      caller is unrecoverable at PREROUTING.
///
/// Stale `gateway_<id>` endpoints (left behind by a reaped task whose
/// sandbox ID no longer matches any running container) fall through to
/// `None` and get dropped, so the cache never holds dead IPs.
async fn query_docker() -> Result<HashMap<Ipv4Addr, String>, String> {
    let ids = list_container_ids().await?;
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let index = inspect_container_index(&ids).await?;

    let network_ids = list_network_ids().await?;

    let mut map = HashMap::new();
    for net_id in network_ids {
        match inspect_network_endpoints(&net_id).await {
            Ok(endpoints) => {
                for (endpoint_name, ip_cidr) in endpoints {
                    let Some(name) = resolve_endpoint(&endpoint_name, &index) else {
                        continue;
                    };
                    let Some(ip) = strip_cidr_to_ipv4(&ip_cidr) else {
                        continue;
                    };
                    map.insert(ip, name);
                }
            }
            Err(e) => {
                eprintln!("[nfqueue/cache] network inspect {net_id}: {e}; skipping that network");
            }
        }
    }

    Ok(map)
}

async fn list_container_ids() -> Result<Vec<String>, String> {
    let out = run_docker(&["ps", "-q", "--no-trunc"], "docker ps").await?;
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect())
}

async fn inspect_container_index(ids: &[String]) -> Result<ContainerIndex, String> {
    let mut args: Vec<&str> = vec![
        "inspect",
        "--format",
        "{{.Name}}|{{.NetworkSettings.SandboxID}}",
    ];
    args.extend(ids.iter().map(String::as_str));
    let out = run_docker(&args, "docker inspect").await?;
    Ok(parse_container_index(&String::from_utf8_lossy(&out.stdout)))
}

fn parse_container_index(s: &str) -> ContainerIndex {
    let mut names = HashSet::new();
    let mut sandbox12_to_name = HashMap::new();
    for line in s.lines() {
        let Some((name_part, sandbox_part)) = line.split_once('|') else {
            continue;
        };
        let name = name_part.trim().trim_start_matches('/').to_string();
        if name.is_empty() {
            continue;
        }
        names.insert(name.clone());
        let sandbox = sandbox_part.trim();
        if sandbox.len() >= SANDBOX_PREFIX_LEN {
            sandbox12_to_name.insert(sandbox[..SANDBOX_PREFIX_LEN].to_string(), name);
        }
    }
    ContainerIndex {
        names,
        sandbox12_to_name,
    }
}

async fn list_network_ids() -> Result<Vec<String>, String> {
    let out = run_docker(&["network", "ls", "-q", "--no-trunc"], "docker network ls").await?;
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect())
}

async fn inspect_network_endpoints(net_id: &str) -> Result<Vec<(String, String)>, String> {
    // `{{"\n"}}` in the Go template emits a real newline between endpoints
    // so the output stays parseable as one record per line even when an
    // endpoint name contains no special chars.
    let out = run_docker(
        &[
            "network",
            "inspect",
            net_id,
            "--format",
            r#"{{range $k, $v := .Containers}}{{$v.Name}}|{{$v.IPv4Address}}{{"\n"}}{{end}}"#,
        ],
        "docker network inspect",
    )
    .await?;
    Ok(parse_network_endpoints(&String::from_utf8_lossy(
        &out.stdout,
    )))
}

/// Run `docker` with the given args, bounded by `DOCKER_SUBPROCESS_TIMEOUT`
/// and with `kill_on_drop` so the child is reaped if we abandon it. Any
/// timeout / spawn / non-zero-exit case becomes `Err(String)` the caller can
/// propagate. `label` is folded into error messages so the source of the
/// failure is identifiable in logs.
async fn run_docker(args: &[&str], label: &str) -> Result<std::process::Output, String> {
    let mut cmd = tokio::process::Command::new("docker");
    cmd.args(args).kill_on_drop(true);
    let out = match tokio::time::timeout(DOCKER_SUBPROCESS_TIMEOUT, cmd.output()).await {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => return Err(format!("{label}: {e}")),
        Err(_) => {
            return Err(format!(
                "{label}: timed out after {DOCKER_SUBPROCESS_TIMEOUT:?}"
            ));
        }
    };
    if !out.status.success() {
        return Err(format!(
            "{label} exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(out)
}

fn parse_network_endpoints(s: &str) -> Vec<(String, String)> {
    s.lines()
        .filter_map(|line| {
            let (name, ip) = line.split_once('|')?;
            let name = name.trim();
            let ip = ip.trim();
            if name.is_empty() || ip.is_empty() {
                return None;
            }
            Some((name.to_string(), ip.to_string()))
        })
        .collect()
}

/// Resolve a network endpoint's `Name` back to a container name. Returns
/// `None` for endpoints that don't correspond to a single owning container
/// (LB sandboxes, ingress, or stale gateway endpoints whose container was
/// reaped between our `docker ps` and `docker network inspect` calls).
fn resolve_endpoint(endpoint_name: &str, idx: &ContainerIndex) -> Option<String> {
    // Direct match wins. Has to come first so that a user who deliberately
    // names their container `gateway_xxx` still resolves correctly.
    if idx.names.contains(endpoint_name) {
        return Some(endpoint_name.to_string());
    }
    // Swarm task sandbox on docker_gwbridge: endpoint name is
    // `gateway_<sandboxid[..12]>`. The 12-hex check rejects
    // `gateway_ingress-sbox` and anything else with a non-hex tail.
    if let Some(tail) = endpoint_name.strip_prefix("gateway_")
        && tail.len() == SANDBOX_PREFIX_LEN
        && tail.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return idx.sandbox12_to_name.get(tail).cloned();
    }
    None
}

fn strip_cidr_to_ipv4(ip_with_mask: &str) -> Option<Ipv4Addr> {
    let s = ip_with_mask.trim();
    let ip_part = s.split_once('/').map_or(s, |(ip, _)| ip);
    ip_part.parse().ok()
}

/// Spawn the long-running `docker events` watcher. Triggers a refresh after
/// every container start/die. If docker isn't installed or the subprocess
/// can't be spawned, the task logs and exits — listener falls back to the
/// initial cache snapshot. Restarts the subprocess on unexpected exit.
pub fn spawn_events_watcher(cache: BridgeIpCache) {
    tokio::spawn(async move {
        loop {
            if let Err(e) = run_events_loop(&cache).await {
                eprintln!("[nfqueue/cache] events watcher: {e}; restarting in 5s");
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        }
    });
}

async fn run_events_loop(cache: &BridgeIpCache) -> Result<(), String> {
    let mut child = tokio::process::Command::new("docker")
        .args([
            "events",
            "--filter",
            "type=container",
            "--filter",
            "event=start",
            "--filter",
            "event=die",
            "--format",
            "{{.Status}}",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn docker events: {e}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "docker events: no stdout pipe".to_string())?;
    let mut lines = BufReader::new(stdout).lines();
    while let Some(line) = lines
        .next_line()
        .await
        .map_err(|e| format!("read docker events: {e}"))?
    {
        // We don't parse the line — any container start/die warrants a
        // full refresh. Cheap enough: a few processes per event.
        println!("[nfqueue/cache] docker event: {line} — refreshing");
        cache.refresh().await;
    }
    Err("docker events stream ended".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idx_with(name: &str, sandbox_prefix: &str) -> ContainerIndex {
        let mut names = HashSet::new();
        names.insert(name.to_string());
        let mut s = HashMap::new();
        s.insert(sandbox_prefix.to_string(), name.to_string());
        ContainerIndex {
            names,
            sandbox12_to_name: s,
        }
    }

    #[test]
    fn container_index_extracts_name_and_sandbox_prefix() {
        // Real-world sample from a Swarm task: leading "/" on Name and a
        // 64-hex SandboxID we take the first 12 of.
        let s = "/myapp|ff39aaed6ec3af9adb5185fea25d959b3d8c9ce56ac1fb2d018faa45875f67f1\n";
        let idx = parse_container_index(s);
        assert!(idx.names.contains("myapp"));
        assert_eq!(
            idx.sandbox12_to_name.get("ff39aaed6ec3"),
            Some(&"myapp".to_string())
        );
    }

    #[test]
    fn container_index_handles_multiple_lines() {
        let s = "/a|aaaaaaaaaaaaffffffff\n/b|bbbbbbbbbbbbffffffff\n";
        let idx = parse_container_index(s);
        assert_eq!(idx.names.len(), 2);
        assert_eq!(
            idx.sandbox12_to_name.get("aaaaaaaaaaaa"),
            Some(&"a".to_string())
        );
        assert_eq!(
            idx.sandbox12_to_name.get("bbbbbbbbbbbb"),
            Some(&"b".to_string())
        );
    }

    #[test]
    fn container_index_skips_short_sandbox() {
        // A container with no sandbox (or one shorter than 12 chars) still
        // gets its name recorded so direct endpoint-name lookups work.
        let s = "/a|short\n";
        let idx = parse_container_index(s);
        assert!(idx.names.contains("a"));
        assert!(idx.sandbox12_to_name.is_empty());
    }

    #[test]
    fn container_index_skips_empty_name() {
        let s = "|aaaaaaaaaaaa1234567890\n";
        let idx = parse_container_index(s);
        assert!(idx.names.is_empty());
        assert!(idx.sandbox12_to_name.is_empty());
    }

    #[test]
    fn container_index_skips_malformed_lines() {
        let s = "no-pipe-here\n/ok|aaaaaaaaaaaa1234567890\n";
        let idx = parse_container_index(s);
        assert_eq!(idx.names.len(), 1);
        assert!(idx.names.contains("ok"));
    }

    #[test]
    fn network_endpoints_parses_basic() {
        let s = "myapp|172.17.0.5/16\ngateway_abc123def456|172.18.0.5/16\n";
        let v = parse_network_endpoints(s);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0], ("myapp".to_string(), "172.17.0.5/16".to_string()));
        assert_eq!(
            v[1],
            (
                "gateway_abc123def456".to_string(),
                "172.18.0.5/16".to_string()
            )
        );
    }

    #[test]
    fn network_endpoints_skips_blanks_and_empty_fields() {
        // Blank lines, lines with no IP, lines with no name — all dropped.
        let s = "\n|\nmyapp|\n|172.17.0.5/16\nmyapp|172.17.0.5/16\n";
        let v = parse_network_endpoints(s);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0], ("myapp".to_string(), "172.17.0.5/16".to_string()));
    }

    #[test]
    fn resolve_direct_match_wins() {
        let idx = idx_with("myapp", "abc123def456");
        assert_eq!(resolve_endpoint("myapp", &idx), Some("myapp".to_string()));
    }

    #[test]
    fn resolve_direct_match_wins_even_when_name_looks_like_gateway() {
        // A user could name their container "gateway_<12hex>". The direct
        // match must take precedence over the SandboxID join so we don't
        // misroute traffic from that container.
        let mut idx = idx_with("real-app", "999999999999");
        idx.names.insert("gateway_abc123def456".to_string());
        assert_eq!(
            resolve_endpoint("gateway_abc123def456", &idx),
            Some("gateway_abc123def456".to_string())
        );
    }

    #[test]
    fn resolve_gateway_prefix_with_sandbox_match() {
        // The Swarm-on-gwbridge case: endpoint name is gateway_<sandbox12>,
        // sandbox12 matches a running task → resolves to the task's name.
        let idx = idx_with("mystack_actix-sample.1.xyz", "ff39aaed6ec3");
        assert_eq!(
            resolve_endpoint("gateway_ff39aaed6ec3", &idx),
            Some("mystack_actix-sample.1.xyz".to_string())
        );
    }

    #[test]
    fn resolve_gateway_ingress_sbox_skipped() {
        // gateway_ingress-sbox is Swarm's ingress sandbox; tail is not hex.
        let idx = idx_with("myapp", "abc123def456");
        assert_eq!(resolve_endpoint("gateway_ingress-sbox", &idx), None);
    }

    #[test]
    fn resolve_gateway_unknown_sandbox_skipped() {
        // 12 hex chars but not in the index — stale endpoint left over from
        // a reaped task. Drop it so the cache doesn't hold dead IPs.
        let idx = idx_with("myapp", "abc123def456");
        assert_eq!(resolve_endpoint("gateway_deadbeef0000", &idx), None);
    }

    #[test]
    fn resolve_lb_endpoint_skipped() {
        // Overlay LB sandbox endpoints (lb-<network>) have no single owning
        // task — they SNAT'd the real initiator out of existence.
        let idx = idx_with("myapp", "abc123def456");
        assert_eq!(resolve_endpoint("lb-some-overlay", &idx), None);
    }

    #[test]
    fn resolve_unknown_name_skipped() {
        let idx = idx_with("myapp", "abc123def456");
        assert_eq!(resolve_endpoint("random-thing", &idx), None);
    }

    #[test]
    fn strip_cidr_with_mask() {
        assert_eq!(
            strip_cidr_to_ipv4("172.18.0.5/16"),
            Some(Ipv4Addr::new(172, 18, 0, 5))
        );
    }

    #[test]
    fn strip_cidr_without_mask() {
        assert_eq!(
            strip_cidr_to_ipv4("172.18.0.5"),
            Some(Ipv4Addr::new(172, 18, 0, 5))
        );
    }

    #[test]
    fn strip_cidr_garbage_returns_none() {
        assert_eq!(strip_cidr_to_ipv4(""), None);
        assert_eq!(strip_cidr_to_ipv4("not-an-ip/16"), None);
        assert_eq!(
            strip_cidr_to_ipv4("172.18.0.5/garbage"),
            Some(Ipv4Addr::new(172, 18, 0, 5))
        );
    }

    #[tokio::test]
    async fn replace_and_get_round_trip() {
        let cache = BridgeIpCache::new();
        assert_eq!(cache.get(Ipv4Addr::new(1, 2, 3, 4)), None);

        let mut m = HashMap::new();
        m.insert(Ipv4Addr::new(1, 2, 3, 4), "alpha".to_string());
        cache.replace(m);
        assert_eq!(cache.get(Ipv4Addr::new(1, 2, 3, 4)), Some("alpha".into()));

        cache.replace(HashMap::new());
        assert_eq!(cache.get(Ipv4Addr::new(1, 2, 3, 4)), None);
    }
}
