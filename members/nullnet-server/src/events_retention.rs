//! Background sweep that deletes persisted events past the retention window.
//! Event volume is unbounded over time (issue #151); this keeps the `events`
//! table's size bounded by age instead. Structurally mirrors `cert_renewal.rs`.
use crate::db::Db;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time::{self, MissedTickBehavior};

const SECS_PER_DAY: u64 = 86_400;

pub(crate) struct RetentionConfig {
    /// How long an event is kept before it's eligible for deletion.
    retention_secs: u64,
    /// How often to run the deletion sweep.
    sweep_interval_secs: u64,
}

impl RetentionConfig {
    pub(crate) fn from_env() -> Self {
        Self {
            retention_secs: env_parsed::<u64>("EVENT_RETENTION_DAYS", 7) * SECS_PER_DAY,
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
            let cutoff = now_secs() - config.retention_secs as i64;
            match db.events().delete_older_than(cutoff).await {
                Ok(0) => {}
                Ok(deleted) => println!(
                    "Event retention: deleted {deleted} event(s) older than {}d",
                    config.retention_secs / SECS_PER_DAY
                ),
                Err(e) => eprintln!("Event retention: sweep failed: {e:?}"),
            }
        }
    });
}
