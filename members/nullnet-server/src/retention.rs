//! Background sweep that deletes persisted events and sessions past their
//! retention windows. Both tables grow without bound over time (issue #151);
//! this keeps their size bounded by age instead. Structurally mirrors
//! `cert_renewal.rs`.
//!
//! Sessions are kept longer than events by default: they are far lower-volume
//! and a session history is worth more weeks back than an event log is. A live
//! session is never pruned, however old — see `delete_ended_before`.
use crate::db::Db;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time::{self, MissedTickBehavior};

const SECS_PER_DAY: u64 = 86_400;

pub(crate) struct RetentionConfig {
    /// How long an event is kept before it's eligible for deletion.
    event_retention_secs: u64,
    /// How long an *ended* session is kept before it's eligible for deletion.
    session_retention_secs: u64,
    /// How often to run the deletion sweep.
    sweep_interval_secs: u64,
}

impl RetentionConfig {
    pub(crate) fn from_env() -> Self {
        Self {
            event_retention_secs: env_parsed::<u64>("EVENT_RETENTION_DAYS", 7) * SECS_PER_DAY,
            session_retention_secs: env_parsed::<u64>("SESSION_RETENTION_DAYS", 30) * SECS_PER_DAY,
            sweep_interval_secs: env_parsed("EVENT_RETENTION_SWEEP_INTERVAL_SECS", 3_600), // 1h
        }
    }
}

fn env_parsed<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Spawn the retention loop. The first pass runs immediately, then every
/// `sweep_interval_secs`.
pub(crate) fn start(db: Db, config: RetentionConfig) {
    tokio::spawn(async move {
        let mut interval = time::interval(Duration::from_secs(config.sweep_interval_secs));
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let now = now_secs();
            let event_cutoff = now - config.event_retention_secs as i64;
            match db.events().delete_older_than(event_cutoff).await {
                Ok(0) => {}
                Ok(deleted) => println!(
                    "Event retention: deleted {deleted} event(s) older than {}d",
                    config.event_retention_secs / SECS_PER_DAY
                ),
                Err(e) => eprintln!("Event retention: sweep failed: {e:?}"),
            }
            let session_cutoff = now - config.session_retention_secs as i64;
            match db.sessions().delete_ended_before(session_cutoff).await {
                Ok(0) => {}
                Ok(deleted) => println!(
                    "Session retention: deleted {deleted} session(s) ended over {}d ago",
                    config.session_retention_secs / SECS_PER_DAY
                ),
                Err(e) => eprintln!("Session retention: sweep failed: {e:?}"),
            }
        }
    });
}
