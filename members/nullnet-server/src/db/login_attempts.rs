use crate::db::AsyncSqlite;
use crate::db::models::{LoginAttempt, NewLoginAttempt};
use crate::db::schema::login_attempts;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use nullnet_liberror::{Error, ErrorHandler, Location, location};
use std::sync::Arc;
use tokio::sync::Mutex;

/// After this many consecutive failures, the account is locked out.
const MAX_FAILED_ATTEMPTS: i32 = 5;
/// Lockout duration once the threshold is hit.
const LOCKOUT_SECS: i64 = 15 * 60;

/// Typed access to the `login_attempts` table — a simple per-username
/// failed-login counter/lockout, keyed by username (not IP) since this is a
/// small internal admin tool rather than a public-facing service.
pub(crate) struct LoginAttemptRepository {
    conn: Arc<Mutex<AsyncSqlite>>,
}

impl LoginAttemptRepository {
    pub(super) fn new(conn: Arc<Mutex<AsyncSqlite>>) -> Self {
        Self { conn }
    }

    /// `true` if `username` is currently locked out.
    pub(crate) async fn is_locked(&self, username: &str) -> Result<bool, Error> {
        let mut conn = self.conn.lock().await;
        let row = login_attempts::table
            .find(username)
            .first::<LoginAttempt>(&mut *conn)
            .await
            .optional()
            .handle_err(location!())?;
        Ok(row
            .and_then(|r| r.locked_until)
            .is_some_and(|until| until > super::now()))
    }

    /// Record a failed attempt; locks the account once the threshold is hit.
    pub(crate) async fn record_failure(&self, username: &str) -> Result<(), Error> {
        let mut conn = self.conn.lock().await;
        let existing = login_attempts::table
            .find(username)
            .first::<LoginAttempt>(&mut *conn)
            .await
            .optional()
            .handle_err(location!())?;

        let now = super::now();
        let failed_count = existing.map_or(0, |r| r.failed_count) + 1;
        let locked_until = if failed_count >= MAX_FAILED_ATTEMPTS {
            Some(now + LOCKOUT_SECS)
        } else {
            None
        };

        let new_row = NewLoginAttempt {
            username,
            failed_count,
            locked_until,
            updated_at: now,
        };
        diesel::insert_into(login_attempts::table)
            .values(&new_row)
            .on_conflict(login_attempts::username)
            .do_update()
            .set(&new_row)
            .execute(&mut *conn)
            .await
            .handle_err(location!())?;
        Ok(())
    }

    /// Reset the counter on a successful login.
    pub(crate) async fn clear(&self, username: &str) -> Result<(), Error> {
        let mut conn = self.conn.lock().await;
        diesel::delete(login_attempts::table.find(username))
            .execute(&mut *conn)
            .await
            .handle_err(location!())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::db::Db;

    async fn test_db() -> Db {
        static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "nullnet-server-login-attempts-test-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Db::open(dir.join("test.db").to_str().unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn locks_out_after_threshold() {
        let db = test_db().await;
        let repo = db.login_attempts();

        assert!(!repo.is_locked("alice").await.unwrap());
        for _ in 0..4 {
            repo.record_failure("alice").await.unwrap();
            assert!(!repo.is_locked("alice").await.unwrap());
        }
        repo.record_failure("alice").await.unwrap();
        assert!(repo.is_locked("alice").await.unwrap());
    }

    #[tokio::test]
    async fn clear_resets_counter() {
        let db = test_db().await;
        let repo = db.login_attempts();
        for _ in 0..5 {
            repo.record_failure("alice").await.unwrap();
        }
        assert!(repo.is_locked("alice").await.unwrap());

        repo.clear("alice").await.unwrap();
        assert!(!repo.is_locked("alice").await.unwrap());
        // failure count restarts from zero
        repo.record_failure("alice").await.unwrap();
        assert!(!repo.is_locked("alice").await.unwrap());
    }
}
