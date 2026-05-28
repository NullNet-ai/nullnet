use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::Notify;

/// How long a `Pending` entry survives before it can be retriggered.
/// Bounds the time we wait for a server `VxlanSetup` that may never arrive
/// (e.g., server returned ok without dispatching a setup, or the message
/// was lost).
const PENDING_TIMEOUT: Duration = Duration::from_secs(10);

/// Per-(initiator_container, port) lifecycle. `container_ip` is the bridge IP
/// the NFQUEUE listener observed when the trigger fired; it is carried through
/// to `Active` so DNAT install/remove can match the right `-s` source. Legacy
/// callers that don't know the container IP pass `Ipv4Addr::UNSPECIFIED`,
/// which the dnat module treats as "no source filter".
pub enum Lifecycle {
    Pending {
        since: Instant,
        notify: Arc<Notify>,
        container_ip: Ipv4Addr,
    },
    Active {
        vxlan_id: u32,
        overlay_ip: Ipv4Addr,
        container_ip: Ipv4Addr,
        notify: Arc<Notify>,
    },
}

/// View returned by `state()` for the listener to decide what to do.
pub enum TriggerState {
    /// No entry — listener should mark pending and fire `backend_trigger`.
    Fresh,
    /// Setup is in flight — listener should `await notify.notified()`.
    Pending(Arc<Notify>),
    /// Chain is up — listener can verdict ACCEPT immediately.
    Active,
}

#[derive(Default)]
pub struct TriggersState {
    by_key: Mutex<HashMap<(String, u16), Lifecycle>>,
}

impl TriggersState {
    /// Snapshot the state for `(container, port)`. The lock is dropped before
    /// returning so callers can `.await` on the returned `Notify` without
    /// holding it.
    pub fn state(&self, container: &str, port: u16) -> TriggerState {
        let by_key = self.by_key.lock().unwrap();
        match by_key.get(&(container.to_string(), port)) {
            Some(Lifecycle::Active { .. }) => TriggerState::Active,
            Some(Lifecycle::Pending { since, notify, .. }) if since.elapsed() < PENDING_TIMEOUT => {
                TriggerState::Pending(notify.clone())
            }
            _ => TriggerState::Fresh,
        }
    }

    /// Insert (or refresh) a `Pending` entry and return the `Notify` the
    /// caller awaits. If an in-flight `Pending` already exists, its `Notify`
    /// is returned so concurrent fires share one wake-up.
    pub fn mark_pending(&self, container: &str, port: u16, container_ip: Ipv4Addr) -> Arc<Notify> {
        let mut by_key = self.by_key.lock().unwrap();
        let key = (container.to_string(), port);
        if let Some(Lifecycle::Pending { since, notify, .. }) = by_key.get(&key)
            && since.elapsed() < PENDING_TIMEOUT
        {
            return notify.clone();
        }
        let notify = Arc::new(Notify::new());
        by_key.insert(
            key,
            Lifecycle::Pending {
                since: Instant::now(),
                notify: notify.clone(),
                container_ip,
            },
        );
        notify
    }

    /// Read the `container_ip` stashed at `mark_pending` time without
    /// mutating state. Used by `control_channel` to install DNAT *before*
    /// promoting to `Active` — installing first ensures the held packet
    /// (which wakes on `mark_active`'s `notify_waiters`) finds the DNAT rule
    /// live by the time it traverses `nat PREROUTING`. Returns
    /// `Ipv4Addr::UNSPECIFIED` if no entry exists.
    pub fn peek_container_ip(&self, container: &str, port: u16) -> Ipv4Addr {
        let by_key = self.by_key.lock().unwrap();
        match by_key.get(&(container.to_string(), port)) {
            Some(Lifecycle::Pending { container_ip, .. })
            | Some(Lifecycle::Active { container_ip, .. }) => *container_ip,
            None => Ipv4Addr::UNSPECIFIED,
        }
    }

    /// Promote to `Active` (storing `container_ip` so teardown can match the
    /// DNAT rule's `-s`) and wake every handler awaiting the transition.
    /// The caller is expected to install DNAT *before* calling this — the
    /// wake-up is what allows the held packet to traverse the chain.
    pub fn mark_active(
        &self,
        container: &str,
        port: u16,
        vxlan_id: u32,
        overlay_ip: Ipv4Addr,
        container_ip: Ipv4Addr,
    ) {
        let mut by_key = self.by_key.lock().unwrap();
        let key = (container.to_string(), port);
        let notify = match by_key.remove(&key) {
            Some(Lifecycle::Pending { notify, .. } | Lifecycle::Active { notify, .. }) => notify,
            None => Arc::new(Notify::new()),
        };
        by_key.insert(
            key,
            Lifecycle::Active {
                vxlan_id,
                overlay_ip,
                container_ip,
                notify: notify.clone(),
            },
        );
        // Hold the lock while waking so concurrent `state()` callers see
        // `Active` before they have a chance to register a new waiter that
        // would never get woken.
        notify.notify_waiters();
    }

    /// Drop the entry so the next observed packet on this `(container, port)` retriggers.
    pub fn forget(&self, container: &str, port: u16) {
        self.by_key
            .lock()
            .unwrap()
            .remove(&(container.to_string(), port));
    }

    /// Find the `Active` entry for `vxlan_id`, remove it, and return
    /// `(container, port, overlay_ip, container_ip)` so the caller can tear
    /// down DNAT with the matching `-s`.
    pub fn remove_by_vxlan(&self, vxlan_id: u32) -> Option<(String, u16, Ipv4Addr, Ipv4Addr)> {
        let mut by_key = self.by_key.lock().unwrap();
        let key = by_key.iter().find_map(|((c, p), lc)| match lc {
            Lifecycle::Active { vxlan_id: v, .. } if *v == vxlan_id => Some((c.clone(), *p)),
            _ => None,
        })?;
        // The lock is held across the `iter().find_map` and the `remove`
        // below, so the removed entry is guaranteed to be the same `Active`
        // one we just matched on. A `Pending` here would mean the lock
        // protection was broken.
        match by_key.remove(&key) {
            Some(Lifecycle::Active {
                overlay_ip,
                container_ip,
                ..
            }) => Some((key.0, key.1, overlay_ip, container_ip)),
            Some(Lifecycle::Pending { .. }) | None => {
                unreachable!("find_map matched Active for {key:?}; lock held across remove")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::time::timeout;

    const IP: Ipv4Addr = Ipv4Addr::new(172, 17, 0, 5);
    const OVERLAY: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 1);

    #[tokio::test]
    async fn pending_then_active_wakes_waiter() {
        let state = Arc::new(TriggersState::default());
        let notify = state.mark_pending("c1", 80, IP);
        assert_eq!(state.peek_container_ip("c1", 80), IP);

        let state_clone = state.clone();
        let waiter = tokio::spawn(async move {
            notify.notified().await;
            matches!(state_clone.state("c1", 80), TriggerState::Active)
        });

        tokio::time::sleep(Duration::from_millis(20)).await;
        // The control-channel pattern: peek for install, install DNAT, then
        // mark_active to wake. peek above already returned IP for the install.
        state.mark_active("c1", 80, 42, OVERLAY, IP);

        let ok = timeout(Duration::from_secs(1), waiter)
            .await
            .expect("waiter timed out")
            .expect("join failed");
        assert!(ok, "state should be Active after mark_active");
    }

    #[tokio::test]
    async fn concurrent_pending_share_notify() {
        let state = Arc::new(TriggersState::default());
        let n1 = state.mark_pending("c1", 80, IP);
        let n2 = state.mark_pending("c1", 80, IP);
        assert!(Arc::ptr_eq(&n1, &n2), "same key must reuse the Notify");
    }

    #[tokio::test]
    async fn distinct_containers_are_independent() {
        let state = TriggersState::default();
        let _ = state.mark_pending("c1", 80, IP);
        let _ = state.mark_pending("c2", 80, Ipv4Addr::new(172, 17, 0, 6));
        assert!(matches!(state.state("c1", 80), TriggerState::Pending(_)));
        assert!(matches!(state.state("c2", 80), TriggerState::Pending(_)));
        state.mark_active("c1", 80, 7, OVERLAY, IP);
        assert!(matches!(state.state("c1", 80), TriggerState::Active));
        assert!(matches!(state.state("c2", 80), TriggerState::Pending(_)));
    }

    #[tokio::test]
    async fn remove_by_vxlan_returns_container_port_and_ip() {
        let state = TriggersState::default();
        let _ = state.mark_pending("c1", 80, IP);
        state.mark_active("c1", 80, 42, OVERLAY, IP);
        let removed = state.remove_by_vxlan(42).expect("entry should exist");
        assert_eq!(removed, ("c1".to_string(), 80, OVERLAY, IP));
        assert!(matches!(state.state("c1", 80), TriggerState::Fresh));
    }

    #[tokio::test]
    async fn peek_returns_unspecified_when_absent() {
        let state = TriggersState::default();
        assert_eq!(state.peek_container_ip("c1", 80), Ipv4Addr::UNSPECIFIED);
    }

    #[tokio::test]
    async fn forget_drops_entry() {
        let state = TriggersState::default();
        let _ = state.mark_pending("c1", 80, IP);
        state.forget("c1", 80);
        assert!(matches!(state.state("c1", 80), TriggerState::Fresh));
    }
}
