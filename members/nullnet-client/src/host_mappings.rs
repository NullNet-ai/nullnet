use nullnet_grpc_lib::nullnet_grpc::HostMapping;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
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

    for container in all_containers() {
        let lock = hosts_file_lock(Some(&container));
        let _guard = lock.lock().unwrap_or_else(PoisonError::into_inner);
        if let Ok(true) = edit_container_hosts(&container, strip_marked) {
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

/// Every container docker knows about, running or not.
///
/// `-a` rather than `-q` alone: a paused container is listed either way, but a
/// stopped one only with `-a`, and both are reachable now that we edit the
/// host-side file instead of exec'ing into the container.
fn all_containers() -> Vec<String> {
    let Ok(out) = Command::new("docker")
        .args(["ps", "-aq", "--no-trunc"])
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

/// Host-side path of the file docker bind-mounts at the container's
/// `/etc/hosts`, straight from docker rather than assembled from a hardcoded
/// data-root (which a custom `data-root` or a snap install would break).
///
/// `docker inspect` answers for paused and stopped containers alike — the whole
/// point of going through the file: `docker exec` refuses on a paused container
/// ("is paused, unpause the container before exec"), so the exec-based version
/// of this silently skipped exactly the containers whose entries most needed
/// removing.
///
/// `None` when docker has no path for it (a container sharing the host's
/// network namespace has no bind-mounted hosts file at all).
fn container_hosts_path(container: &str) -> Option<PathBuf> {
    let out = Command::new("docker")
        .args(["inspect", "-f", "{{.HostsPath}}", container])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!path.is_empty()).then(|| PathBuf::from(path))
}

/// Read-modify-write a container's hosts file from the host side, reporting
/// whether the contents actually changed.
///
/// One resolved path for both halves, so a container replaced under the same
/// name mid-operation cannot have us read one file and write another. A failed
/// read is an error rather than empty contents — treating a missing file as
/// empty would truncate a live `/etc/hosts` down to whatever `edit` returns.
///
/// The write is in place, deliberately: the container sees this file through a
/// bind mount on the *inode*, so the usual write-temp-then-rename would leave
/// it looking at the old, now-detached file with every change silently
/// invisible.
pub fn edit_container_hosts(
    container: &str,
    edit: impl FnOnce(&str) -> String,
) -> Result<bool, String> {
    let path = container_hosts_path(container)
        .ok_or_else(|| format!("no hosts file for container '{container}'"))?;
    edit_hosts_file(&path, edit)
}

/// The file half of [`edit_container_hosts`], split out so the read-modify-write
/// — and in particular its refusal to write anything when the read failed — is
/// testable without a running docker.
fn edit_hosts_file(path: &Path, edit: impl FnOnce(&str) -> String) -> Result<bool, String> {
    let current = std::fs::read_to_string(path)
        .map_err(|e| format!("reading {} failed: {e}", path.display()))?;
    let updated = edit(&current);
    if updated == current {
        return Ok(false);
    }
    std::fs::write(path, &updated)
        .map_err(|e| format!("writing {} failed: {e}", path.display()))?;
    Ok(true)
}

#[cfg(test)]
mod edit_tests {
    use super::{HOSTS_MARKER, edit_hosts_file, strip_marked};
    use std::path::PathBuf;

    fn temp_file(name: &str, content: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("nullnet-hosts-test-{name}"));
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn reports_no_change_and_leaves_the_file_alone() {
        let original = "127.0.0.1 localhost\n";
        let path = temp_file("noop", original);

        assert_eq!(edit_hosts_file(&path, ToString::to_string), Ok(false));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn writes_in_place_and_reports_the_change() {
        let path = temp_file(
            "change",
            &format!("127.0.0.1 localhost\n10.0.0.2 api {HOSTS_MARKER}\n"),
        );
        // Same inode before and after: the container sees this file through a
        // bind mount, so replacing it would detach their view.
        let before = std::fs::metadata(&path).unwrap();

        assert_eq!(edit_hosts_file(&path, strip_marked), Ok(true));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "127.0.0.1 localhost\n"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(before.ino(), std::fs::metadata(&path).unwrap().ino());
        }
        let _ = std::fs::remove_file(&path);
    }

    /// A missing file must be an error, never "empty contents" — treating it as
    /// empty would write the edit of nothing over a live `/etc/hosts`.
    #[test]
    fn a_failed_read_writes_nothing() {
        let path = std::env::temp_dir().join("nullnet-hosts-test-absent");
        let _ = std::fs::remove_file(&path);

        assert!(edit_hosts_file(&path, |_| "clobbered\n".to_string()).is_err());
        assert!(!path.exists(), "must not create the file it failed to read");
    }
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
