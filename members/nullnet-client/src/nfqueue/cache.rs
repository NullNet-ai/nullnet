use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::process::Stdio;
use std::sync::{Arc, RwLock};
use tokio::io::{AsyncBufReadExt, BufReader};

/// Bridge-IP → container-name lookup the NFQUEUE listener consults on every
/// queued packet. Populated at startup by enumerating running containers and
/// kept fresh by an async watcher subscribed to `docker events`. Lock holds
/// are brief; readers (per-packet) never block writers for long.
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

    /// One-shot refresh: enumerate running containers and rebuild the map.
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

/// Run `docker ps -q` + `docker inspect` and parse the result. Returns an
/// empty map (not an error) when no containers are running.
async fn query_docker() -> Result<HashMap<Ipv4Addr, String>, String> {
    let ids_output = tokio::process::Command::new("docker")
        .args(["ps", "-q", "--no-trunc"])
        .output()
        .await
        .map_err(|e| format!("docker ps: {e}"))?;
    if !ids_output.status.success() {
        return Err(format!(
            "docker ps exited {}: {}",
            ids_output.status,
            String::from_utf8_lossy(&ids_output.stderr)
        ));
    }
    let ids: Vec<String> = String::from_utf8_lossy(&ids_output.stdout)
        .lines()
        .map(String::from)
        .filter(|s| !s.is_empty())
        .collect();
    if ids.is_empty() {
        return Ok(HashMap::new());
    }

    let mut args = vec![
        "inspect".to_string(),
        "--format".to_string(),
        "{{.Name}}|{{range $k, $v := .NetworkSettings.Networks}}{{$v.IPAddress}},{{end}}"
            .to_string(),
    ];
    args.extend(ids);
    let inspect_output = tokio::process::Command::new("docker")
        .args(&args)
        .output()
        .await
        .map_err(|e| format!("docker inspect: {e}"))?;
    if !inspect_output.status.success() {
        return Err(format!(
            "docker inspect exited {}: {}",
            inspect_output.status,
            String::from_utf8_lossy(&inspect_output.stderr)
        ));
    }
    Ok(parse_inspect_output(&String::from_utf8_lossy(
        &inspect_output.stdout,
    )))
}

/// Parse the docker-inspect output produced by the format string above.
/// Each line is `Name|ip1,ip2,...` with a leading `/` on Name to strip.
fn parse_inspect_output(s: &str) -> HashMap<Ipv4Addr, String> {
    let mut map = HashMap::new();
    for line in s.lines() {
        let Some((name_part, ips_part)) = line.split_once('|') else {
            continue;
        };
        let name = name_part.trim().trim_start_matches('/').to_string();
        if name.is_empty() {
            continue;
        }
        for ip_s in ips_part.split(',') {
            let ip_s = ip_s.trim();
            if ip_s.is_empty() {
                continue;
            }
            if let Ok(ip) = ip_s.parse::<Ipv4Addr>() {
                map.insert(ip, name.clone());
            }
        }
    }
    map
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

    #[test]
    fn parses_single_container_single_network() {
        let s = "/web|172.17.0.5,\n";
        let map = parse_inspect_output(s);
        assert_eq!(map.get(&Ipv4Addr::new(172, 17, 0, 5)), Some(&"web".into()));
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn parses_multi_network() {
        let s = "/api|172.18.0.3,10.0.5.4,\n";
        let map = parse_inspect_output(s);
        assert_eq!(map.get(&Ipv4Addr::new(172, 18, 0, 3)), Some(&"api".into()));
        assert_eq!(map.get(&Ipv4Addr::new(10, 0, 5, 4)), Some(&"api".into()));
    }

    #[test]
    fn parses_multiple_containers() {
        let s = "/a|172.17.0.2,\n/b|172.17.0.3,\n/c|10.0.0.5,\n";
        let map = parse_inspect_output(s);
        assert_eq!(map.len(), 3);
        assert_eq!(map.get(&Ipv4Addr::new(172, 17, 0, 2)), Some(&"a".into()));
        assert_eq!(map.get(&Ipv4Addr::new(172, 17, 0, 3)), Some(&"b".into()));
        assert_eq!(map.get(&Ipv4Addr::new(10, 0, 0, 5)), Some(&"c".into()));
    }

    #[test]
    fn skips_empty_ips() {
        // Containers in `--network host` or without a network have empty IP.
        let s = "/host-mode|,\n/normal|172.17.0.2,\n";
        let map = parse_inspect_output(s);
        assert_eq!(map.len(), 1);
        assert_eq!(
            map.get(&Ipv4Addr::new(172, 17, 0, 2)),
            Some(&"normal".into())
        );
    }

    #[test]
    fn skips_malformed_lines() {
        let s = "no-pipe-here\n/ok|172.17.0.4,\n";
        let map = parse_inspect_output(s);
        assert_eq!(map.len(), 1);
        assert_eq!(map.get(&Ipv4Addr::new(172, 17, 0, 4)), Some(&"ok".into()));
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
