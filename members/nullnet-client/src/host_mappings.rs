use nullnet_grpc_lib::nullnet_grpc::HostMapping;
use std::collections::HashMap;
use std::process::Command;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

/// Trailing marker on every `/etc/hosts` line we write, so a fresh process can
/// tell its own entries from the operator's and sweep only the former.
pub const HOSTS_MARKER: &str = "# nullnet";

const HOSTS_PATH: &str = "/etc/hosts";

/// One lock per `/etc/hosts` file, keyed by container (`None` = the host's own
/// file).
///
/// Installing and removing a mapping is a read-modify-write: read the file,
/// edit it in memory, write it back. Every `VxlanSetup`/`VxlanTeardown` is
/// handled in its own `tokio::spawn`, so bringing up a chain with N
/// dependencies on the same container runs N of those cycles concurrently.
/// Without a lock they each start from a snapshot taken before the others
/// wrote, and the last writer silently drops everyone else's entries — the
/// container ends up with a live tunnel whose name was never mapped, and the
/// caller falls through to public DNS.
///
/// Per-file rather than global so unrelated containers don't serialize behind
/// each other's `docker exec` round trips.
type HostsLocks = Mutex<HashMap<Option<String>, Arc<Mutex<()>>>>;

static HOSTS_LOCKS: LazyLock<HostsLocks> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// The lock guarding one container's (or the host's) `/etc/hosts`. Hold it
/// across the whole read-modify-write, not just the write.
pub fn hosts_file_lock(docker_container: Option<&str>) -> Arc<Mutex<()>> {
    HOSTS_LOCKS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .entry(docker_container.map(String::from))
        .or_default()
        .clone()
}

/// Tracks the `/etc/hosts` entries installed by setup so the matching
/// teardown can remove them. Teardown messages don't carry the mapping,
/// so we record it locally at setup time and look it up on teardown.
#[derive(Default)]
pub struct HostMappingsState {
    by_vlan: Mutex<HashMap<u16, HostMapping>>,
    by_vxlan: Mutex<HashMap<u32, (HostMapping, Option<String>)>>,
}

impl HostMappingsState {
    pub fn record_vlan(&self, vlan_id: u16, hm: HostMapping) {
        self.by_vlan.lock().unwrap().insert(vlan_id, hm);
    }

    pub fn take_vlan(&self, vlan_id: u16) -> Option<HostMapping> {
        self.by_vlan.lock().unwrap().remove(&vlan_id)
    }

    pub fn record_vxlan(&self, vxlan_id: u32, hm: HostMapping, docker_container: Option<String>) {
        self.by_vxlan
            .lock()
            .unwrap()
            .insert(vxlan_id, (hm, docker_container));
    }

    pub fn take_vxlan(&self, vxlan_id: u32) -> Option<(HostMapping, Option<String>)> {
        self.by_vxlan.lock().unwrap().remove(&vxlan_id)
    }
}

/// Drop every mapping this process's predecessor left behind, on the host and
/// in each running container.
///
/// `VxlanTeardown` removes them per edge, but a client that was killed never
/// got one, and the in-memory `HostMappingsState` pairing an entry with its
/// tunnel dies with the process. A survivor resolves its name to an overlay IP
/// that no longer has a tunnel — strictly worse than falling through to public
/// DNS. Only marked lines go; entries written before [`HOSTS_MARKER`] existed
/// stay, rather than risk deleting an operator's.
pub fn purge_stale_mappings() {
    let mut swept = 0usize;

    let host_lock = hosts_file_lock(None);
    let guard = host_lock.lock().unwrap_or_else(PoisonError::into_inner);
    if let Ok(content) = std::fs::read_to_string(HOSTS_PATH) {
        let cleaned = strip_marked(&content);
        if cleaned != content {
            match std::fs::write(HOSTS_PATH, &cleaned) {
                Ok(()) => swept += 1,
                Err(e) => eprintln!("[hosts] purge: writing {HOSTS_PATH} failed: {e}"),
            }
        }
    }
    drop(guard);

    for container in running_containers() {
        let lock = hosts_file_lock(Some(&container));
        let _guard = lock.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(content) = container_hosts(&container) else {
            continue;
        };
        let cleaned = strip_marked(&content);
        if cleaned != content && write_container_hosts(&container, &cleaned) {
            swept += 1;
        }
    }

    println!("[hosts] purge: swept stale entries from {swept} file(s)");
}

/// Every line except the ones we wrote.
fn strip_marked(content: &str) -> String {
    let kept: Vec<&str> = content
        .lines()
        .filter(|line| !line.trim_end().ends_with(HOSTS_MARKER))
        .collect();
    kept.join("\n") + "\n"
}

fn running_containers() -> Vec<String> {
    let Ok(out) = Command::new("docker")
        .args(["ps", "-q", "--no-trunc"])
        .output()
    else {
        return vec![]; // no docker on this node
    };
    if !out.status.success() {
        return vec![];
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

/// `None` when the read failed — never an empty string, which would truncate
/// the file on write-back (the same trap `add_host_mapping` guards against).
fn container_hosts(container: &str) -> Option<String> {
    let out = Command::new("docker")
        .args(["exec", container, "cat", HOSTS_PATH])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn write_container_hosts(container: &str, content: &str) -> bool {
    use std::io::Write;
    let Ok(mut child) = Command::new("docker")
        .args([
            "exec",
            "-i",
            container,
            "sh",
            "-c",
            &format!("cat > {HOSTS_PATH}"),
        ])
        .stdin(std::process::Stdio::piped())
        .spawn()
    else {
        return false;
    };
    if let Some(mut stdin) = child.stdin.take()
        && stdin.write_all(content.as_bytes()).is_err()
    {
        return false;
    }
    child.wait().map(|s| s.success()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::{HOSTS_MARKER, strip_marked};

    #[test]
    fn strips_only_marked_lines() {
        let content = format!(
            "127.0.0.1 localhost\n\
             10.0.0.2 api.example.com {HOSTS_MARKER}\n\
             # a plain comment\n\
             192.168.1.5 operator-host\n\
             10.0.0.10 db.example.com {HOSTS_MARKER}\n"
        );
        assert_eq!(
            strip_marked(&content),
            "127.0.0.1 localhost\n# a plain comment\n192.168.1.5 operator-host\n"
        );
    }

    #[test]
    fn leaves_unmarked_content_untouched() {
        let content = "127.0.0.1 localhost\n10.0.0.2 legacy.example.com\n";
        assert_eq!(strip_marked(content), content);
    }
}
