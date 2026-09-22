use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::Notify;

/// How long a `Pending` entry survives before it can be retriggered.
/// Bounds the time we wait for a server `VxlanSetup` that may never arrive
/// (e.g., server returned ok without dispatching a setup, or the message
/// was lost).
///
/// Kept at the listeners' `TRIGGER_TIMEOUT + STEER_TIMEOUT/ACTIVE_TIMEOUT`
/// (30s + 5s): an entry must outlive one handler's full wait, or a second
/// packet expires it and re-triggers while the first build is still in flight.
const PENDING_TIMEOUT: Duration = Duration::from_secs(35);

/// Port component of the key for egress triggers. Egress fires once per initiator
/// container (steering matches every destination), so all its flows share one
/// entry. Backend triggers only ever key on real listener ports (>0), so this
/// sentinel never collides with them in the shared `TriggersState`.
pub const EGRESS_TRIGGER_PORT: u16 = 0;

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
    Installed {
        ready: bool,
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
    /// Setup is in flight.
    Pending,
    /// Chain is up — listener can verdict ACCEPT immediately.
    Active,
}

pub enum TriggerClaim {
    Start(Arc<Notify>),
    Pending(Arc<Notify>),
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
            Some(Lifecycle::Installed { ready: true, .. }) => TriggerState::Active,
            Some(Lifecycle::Installed { ready: false, .. }) => TriggerState::Pending,
            Some(Lifecycle::Pending { since, .. }) if since.elapsed() < PENDING_TIMEOUT => {
                TriggerState::Pending
            }
            _ => TriggerState::Fresh,
        }
    }

    /// Elect one sender while preserving a setup that has already activated.
    pub fn claim(&self, container: &str, port: u16, container_ip: Ipv4Addr) -> TriggerClaim {
        let mut by_key = self.by_key.lock().unwrap();
        let key = (container.to_string(), port);
        match by_key.get(&key) {
            Some(Lifecycle::Installed { ready: true, .. }) => return TriggerClaim::Active,
            Some(Lifecycle::Installed {
                ready: false,
                notify,
                ..
            }) => {
                return TriggerClaim::Pending(notify.clone());
            }
            Some(Lifecycle::Pending { since, notify, .. }) if since.elapsed() < PENDING_TIMEOUT => {
                return TriggerClaim::Pending(notify.clone());
            }
            _ => {}
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
        TriggerClaim::Start(notify)
    }

    #[cfg(test)]
    fn mark_pending(&self, container: &str, port: u16, container_ip: Ipv4Addr) -> Arc<Notify> {
        match self.claim(container, port, container_ip) {
            TriggerClaim::Start(notify) | TriggerClaim::Pending(notify) => notify,
            TriggerClaim::Active => panic!("test expected a pending trigger"),
        }
    }

    /// Read the claimed container IP before installing scoped steering.
    /// Installed entries retain it until teardown, including before NetReady.
    pub fn peek_container_ip(&self, container: &str, port: u16) -> Ipv4Addr {
        let by_key = self.by_key.lock().unwrap();
        match by_key.get(&(container.to_string(), port)) {
            Some(Lifecycle::Pending { container_ip, .. })
            | Some(Lifecycle::Installed { container_ip, .. }) => *container_ip,
            None => Ipv4Addr::UNSPECIFIED,
        }
    }

    /// Record installed local steering without releasing packets. The server
    /// sends NetReady only after both endpoints acknowledge setup.
    pub fn mark_prepared(
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
            Some(Lifecycle::Pending { notify, .. } | Lifecycle::Installed { notify, .. }) => notify,
            None => Arc::new(Notify::new()),
        };
        by_key.insert(
            key,
            Lifecycle::Installed {
                ready: false,
                vxlan_id,
                overlay_ip,
                container_ip,
                notify,
            },
        );
    }

    pub fn mark_ready(&self, vxlan_id: u32) -> bool {
        let mut by_key = self.by_key.lock().unwrap();
        for lifecycle in by_key.values_mut() {
            if let Lifecycle::Installed {
                vxlan_id: id,
                ready,
                notify,
                ..
            } = lifecycle
                && *id == vxlan_id
            {
                *ready = true;
                notify.notify_waiters();
                return true;
            }
        }
        false
    }

    #[cfg(test)]
    pub fn mark_active(
        &self,
        container: &str,
        port: u16,
        vxlan_id: u32,
        overlay_ip: Ipv4Addr,
        container_ip: Ipv4Addr,
    ) {
        self.mark_prepared(container, port, vxlan_id, overlay_ip, container_ip);
        assert!(self.mark_ready(vxlan_id));
    }

    /// Drop only the failed caller's pending claim. Installed steering must
    /// survive RPC failure so teardown can still remove the exact DNAT rule.
    pub fn forget_pending(&self, container: &str, port: u16, owner: &Arc<Notify>) {
        let mut by_key = self.by_key.lock().unwrap();
        let key = (container.to_string(), port);
        if matches!(by_key.get(&key), Some(Lifecycle::Pending { notify, .. }) if Arc::ptr_eq(notify, owner))
        {
            by_key.remove(&key);
        }
    }

    /// Find the installed entry for `vxlan_id`, remove it, and return
    /// `(container, port, overlay_ip, container_ip)` so the caller can tear
    /// down DNAT with the matching `-s`.
    pub fn remove_by_vxlan(&self, vxlan_id: u32) -> Option<(String, u16, Ipv4Addr, Ipv4Addr)> {
        let mut by_key = self.by_key.lock().unwrap();
        let key = by_key.iter().find_map(|((c, p), lc)| match lc {
            Lifecycle::Installed { vxlan_id: v, .. } if *v == vxlan_id => Some((c.clone(), *p)),
            _ => None,
        })?;
        // The lock is held across the `iter().find_map` and the `remove`
        // below, so the removed entry is guaranteed to be the same installed
        // one we just matched on. A `Pending` here would mean the lock
        // protection was broken.
        match by_key.remove(&key) {
            Some(Lifecycle::Installed {
                overlay_ip,
                container_ip,
                ..
            }) => Some((key.0, key.1, overlay_ip, container_ip)),
            Some(Lifecycle::Pending { .. }) | None => {
                unreachable!("find_map matched Installed for {key:?}; lock held across remove")
            }
        }
    }
}

#[cfg(test)]
mod tests {

    #[tokio::test]
    async fn installed_trigger_waits_for_both_endpoints() {
        let state = TriggersState::default();
        let owner = state.mark_pending("c1", 80, IP);
        let notified = owner.notified();
        tokio::pin!(notified);
        assert!(!notified.as_mut().enable());
        state.mark_prepared("c1", 80, 42, OVERLAY, IP);
        assert!(matches!(state.state("c1", 80), TriggerState::Pending));
        assert!(matches!(
            state.claim("c1", 80, IP),
            TriggerClaim::Pending(_)
        ));
        assert!(!notified.as_mut().enable());
        assert!(state.mark_ready(42));
        notified.await;
        assert!(matches!(state.claim("c1", 80, IP), TriggerClaim::Active));
    }

    #[test]
    fn prepared_steering_survives_rpc_failure_and_is_cleaned_before_activation() {
        let state = TriggersState::default();
        let owner = state.mark_pending("c1", 80, IP);
        state.mark_prepared("c1", 80, 42, OVERLAY, IP);
        state.forget_pending("c1", 80, &owner);
        assert_eq!(
            state.remove_by_vxlan(42),
            Some(("c1".into(), 80, OVERLAY, IP))
        );
        assert!(!state.mark_ready(42));
        state.mark_prepared("c1", 80, 43, OVERLAY, IP);
        assert!(!state.mark_ready(42));
        assert!(matches!(state.state("c1", 80), TriggerState::Pending));
        assert!(state.mark_ready(43));
    }

    use super::*;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::time::timeout;

    #[test]
    fn atomic_claim_elects_one_sender_and_preserves_active() {
        let state = TriggersState::default();
        let TriggerClaim::Start(owner) = state.claim("c1", 80, IP) else {
            panic!("first claim");
        };
        let TriggerClaim::Pending(waiter) = state.claim("c1", 80, IP) else {
            panic!("second claim");
        };
        assert!(Arc::ptr_eq(&owner, &waiter));
        state.mark_active("c1", 80, 42, OVERLAY, IP);
        assert!(matches!(state.claim("c1", 80, IP), TriggerClaim::Active));
    }

    #[test]
    fn old_failure_cannot_forget_replacement() {
        let state = TriggersState::default();
        let old = state.mark_pending("c1", 80, IP);
        state.forget_pending("c1", 80, &old);
        let replacement = state.mark_pending("c1", 80, IP);
        state.forget_pending("c1", 80, &old);
        let TriggerClaim::Pending(current) = state.claim("c1", 80, IP) else {
            panic!("replacement lost");
        };
        assert!(Arc::ptr_eq(&current, &replacement));
    }

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
    async fn state_recheck_catches_mark_active_that_fired_before_enable() {
        // The listener race: `mark_active` runs (and its
        // `notify_waiters()` wakes zero waiters because we haven't called
        // `notified()` yet) between `mark_pending` returning and the
        // listener registering the Notified future. The recovery is the
        // synchronous state recheck after `.enable()` — it must observe
        // the Active state set by mark_active and short-circuit the await.
        let state = Arc::new(TriggersState::default());
        let notify = state.mark_pending("c1", 80, IP);

        // mark_active fires BEFORE the listener registers a waiter — this
        // is exactly the lost-wake race.
        state.mark_active("c1", 80, 42, OVERLAY, IP);

        // The listener's race-protected pattern.
        let notified = notify.notified();
        tokio::pin!(notified);
        let trip_via_enable = notified.as_mut().enable();
        let trip_via_state = matches!(state.state("c1", 80), TriggerState::Active);
        assert!(
            trip_via_enable || trip_via_state,
            "race-fix must observe Active even when mark_active fired before enable"
        );

        // Demonstrate the wake really was lost from the Notify's POV: if
        // the listener had skipped the recheck and just awaited the
        // Notified, it would time out. This is why the recheck is load-
        // bearing — `notify_waiters()` doesn't store a permit for late
        // registrants the way `notify_one()` would.
        let lost = timeout(Duration::from_millis(50), notified).await.is_err();
        assert!(
            lost,
            "without the recheck, the Notify wake would already be lost"
        );
    }

    #[tokio::test]
    async fn enabled_notified_wakes_on_subsequent_mark_active() {
        // The other half of the fix: once `.enable()` has registered the
        // Notified future, any future `notify_waiters()` from mark_active
        // is delivered. This covers the case where mark_active fires
        // during `backend_trigger`'s round-trip — after enable, before the
        // listener resumes awaiting.
        let state = Arc::new(TriggersState::default());
        let notify = state.mark_pending("c1", 80, IP);

        let notified = notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        assert!(matches!(state.state("c1", 80), TriggerState::Pending));

        // mark_active fires AFTER enable but before await.
        state.mark_active("c1", 80, 42, OVERLAY, IP);

        let result = timeout(Duration::from_millis(50), notified).await;
        assert!(
            result.is_ok(),
            "an enabled Notified must wake on notify_waiters"
        );
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
        assert!(matches!(state.state("c1", 80), TriggerState::Pending));
        assert!(matches!(state.state("c2", 80), TriggerState::Pending));
        state.mark_active("c1", 80, 7, OVERLAY, IP);
        assert!(matches!(state.state("c1", 80), TriggerState::Active));
        assert!(matches!(state.state("c2", 80), TriggerState::Pending));
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
    async fn forget_pending_drops_pending_entry() {
        let state = TriggersState::default();
        let owner = state.mark_pending("c1", 80, IP);
        state.forget_pending("c1", 80, &owner);
        assert!(matches!(state.state("c1", 80), TriggerState::Fresh));
    }

    #[tokio::test]
    async fn forget_pending_keeps_an_active_entry() {
        // The production race: the trigger RPC times out, but the VxlanSetup
        // already landed and promoted the entry. Forgetting here would lose the
        // only record that steering is live and wedge the container's egress.
        let state = TriggersState::default();
        let owner = state.mark_pending("c1", 80, IP);
        state.mark_active("c1", 80, 42, OVERLAY, IP);
        state.forget_pending("c1", 80, &owner);
        assert!(
            matches!(state.state("c1", 80), TriggerState::Active),
            "forget_pending must not drop an entry that mark_active promoted"
        );
    }
}
