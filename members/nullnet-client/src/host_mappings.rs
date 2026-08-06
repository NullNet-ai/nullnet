use nullnet_grpc_lib::nullnet_grpc::HostMapping;
use nullnet_liberror::{Error, ErrorHandler, Location, location};
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
        if cleaned != content && write_container_hosts(&container, &cleaned).is_ok() {
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
/// the file on write-back (the same trap `upsert_container_host_entry` guards
/// against).
fn container_hosts(container: &str) -> Option<String> {
    let out = Command::new("docker")
        .args(["exec", container, "cat", HOSTS_PATH])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Upsert `name -> ip` into `container`'s own `/etc/hosts` via `docker exec`,
/// tagged with [`HOSTS_MARKER`] so a restarted process can sweep it later.
/// Shared by two callers: the reactive real-mapping write once a tunnel is up
/// (`control_channel`'s `add_host_mapping`) and the proactive placeholder seed
/// written before any packet exists (`placeholder::seed_placeholder`) — both
/// need identical docker-exec read/upsert/write mechanics, a different `ip`
/// value, and the same crash-safety (locking, tagging).
pub(crate) fn upsert_container_host_entry(
    container: &str,
    name: &str,
    ip: &str,
) -> Result<(), Error> {
    let lock = hosts_file_lock(Some(container));
    let _guard = lock.lock().unwrap_or_else(PoisonError::into_inner);

    let entry = format!("{ip} {name} {HOSTS_MARKER}");
    let cat = Command::new("docker")
        .args(["exec", container, "cat", HOSTS_PATH])
        .output()
        .handle_err(location!())?;
    // Bail rather than write on a failed read: `output()` is Ok even when the
    // exec itself failed (container gone, docker hiccup), and treating the
    // empty stdout as the file's contents would truncate a live `/etc/hosts`
    // down to this one entry.
    if !cat.status.success() {
        return Err(format!(
            "reading {HOSTS_PATH} in '{container}' failed: {}",
            String::from_utf8_lossy(&cat.stderr).trim()
        ))
        .handle_err(location!());
    }
    let content = upsert_hosts_entry(&String::from_utf8_lossy(&cat.stdout), name, &entry);
    write_container_hosts(container, &content)
}

/// Remove `name`'s line from `container`'s own `/etc/hosts` via `docker exec`,
/// but only while it still points at `ip` — see `remove_hosts_entry`.
pub(crate) fn remove_container_host_entry(
    container: &str,
    name: &str,
    ip: &str,
) -> Result<(), Error> {
    let lock = hosts_file_lock(Some(container));
    let _guard = lock.lock().unwrap_or_else(PoisonError::into_inner);

    let cat = Command::new("docker")
        .args(["exec", container, "cat", HOSTS_PATH])
        .output()
        .handle_err(location!())?;
    if !cat.status.success() {
        return Err(format!(
            "reading {HOSTS_PATH} in '{container}' failed: {}",
            String::from_utf8_lossy(&cat.stderr).trim()
        ))
        .handle_err(location!());
    }
    let content = remove_hosts_entry(&String::from_utf8_lossy(&cat.stdout), name, ip);
    write_container_hosts(container, &content)
}

fn write_container_hosts(container: &str, content: &str) -> Result<(), Error> {
    let mut child = Command::new("docker")
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
        .handle_err(location!())?;
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        stdin
            .write_all(content.as_bytes())
            .handle_err(location!())?;
    }
    let _ = child.wait();
    Ok(())
}

pub(crate) fn upsert_hosts_entry(content: &str, name: &str, entry: &str) -> String {
    let mut lines: Vec<String> = content.lines().map(ToString::to_string).collect();
    let mut found = false;
    for line in &mut lines {
        if line.split_whitespace().skip(1).any(|tok| tok == name) {
            *line = entry.to_string();
            found = true;
        }
    }
    if !found {
        lines.push(entry.to_string());
    }
    lines.join("\n") + "\n"
}

/// Drop the line mapping `name`, but only while it still points at `ip`.
///
/// NET IDs are recycled, so a teardown can land after a *newer* net has
/// already re-installed the same name at a different overlay IP
/// (`upsert_hosts_entry` keys on the name alone, which is what makes the
/// replacement correct). Matching the IP too makes the late teardown a no-op
/// instead of deleting a mapping that belongs to a live tunnel.
pub(crate) fn remove_hosts_entry(content: &str, name: &str, ip: &str) -> String {
    let lines: Vec<String> = content
        .lines()
        .filter(|line| {
            let mut tokens = line.split_whitespace();
            let line_ip = tokens.next();
            !(line_ip == Some(ip) && tokens.any(|tok| tok == name))
        })
        .map(ToString::to_string)
        .collect();
    lines.join("\n") + "\n"
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

#[cfg(test)]
mod hosts_entry_tests {
    use super::*;

    #[test]
    fn upsert_appends_when_absent() {
        let out = upsert_hosts_entry("127.0.0.1 localhost\n", "redis", "203.0.113.7 redis");
        assert_eq!(out, "127.0.0.1 localhost\n203.0.113.7 redis\n");
    }

    #[test]
    fn upsert_replaces_existing_line() {
        let out = upsert_hosts_entry(
            "127.0.0.1 localhost\n203.0.113.7 redis\n",
            "redis",
            "10.0.0.5 redis",
        );
        assert_eq!(out, "127.0.0.1 localhost\n10.0.0.5 redis\n");
    }

    #[test]
    fn remove_drops_matching_line_only() {
        let out = remove_hosts_entry(
            "127.0.0.1 localhost\n10.0.0.5 redis\n10.0.0.6 billing\n",
            "redis",
            "10.0.0.5",
        );
        assert_eq!(out, "127.0.0.1 localhost\n10.0.0.6 billing\n");
    }

    #[test]
    fn remove_is_a_noop_when_ip_no_longer_matches() {
        // A late teardown for a torn-down tunnel landing after a newer tunnel
        // re-mapped the same name at a different IP must not delete it.
        let out = remove_hosts_entry("127.0.0.1 localhost\n10.0.0.9 redis\n", "redis", "10.0.0.5");
        assert_eq!(out, "127.0.0.1 localhost\n10.0.0.9 redis\n");
    }
}
