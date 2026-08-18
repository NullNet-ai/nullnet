//! SQLite-backed storage for the server. All database access (schema,
//! migrations, repositories) is contained in this module; callers only see
//! [`Db`] and the repository types it hands out.
//!
//! `certs.rs` still owns the on-disk file storage it always has; nothing
//! calls into `CertRepository` yet, so that part of this module's API
//! surface is unused until that migration happens. `stacks`/`services`/
//! `service_triggers`/`service_dependencies`/`routes` back per-stack
//! service config (issue #140) as normalized rows — the on-disk
//! `./services/*.toml` files are now legacy, auto-imported on startup by
//! `services::migrate::migrate_legacy_toml`. The auth repositories
//! (`users`/`user_scopes`/`refresh_tokens`/`login_attempts`) back the
//! server's JWT auth system and are fully wired up, as is `events` (durable
//! storage for `crate::events::Event`, pruned by `events_retention.rs`).
#![allow(dead_code)]

mod certs;
mod events;
mod login_attempts;
mod models;
mod refresh_tokens;
mod schema;
mod stacks;
mod user_scopes;
mod users;

pub(crate) use certs::CertRepository;
pub(crate) use events::EventRepository;
pub(crate) use login_attempts::LoginAttemptRepository;
pub(crate) use models::{RouteRow, ServiceDependencyRow, ServiceRow, ServiceTriggerRow};
pub(crate) use refresh_tokens::RefreshTokenRepository;
pub(crate) use stacks::{RouteInsert, ServiceInsert, StackRepository};
pub(crate) use user_scopes::ScopeRepository;
pub(crate) use users::UserRepository;

use diesel::Connection;
use diesel::sqlite::SqliteConnection;
use diesel_async::sync_connection_wrapper::SyncConnectionWrapper;
use diesel_async::{AsyncConnection, RunQueryDsl};
use diesel_migrations::{EmbeddedMigrations, MigrationHarness, embed_migrations};
use nullnet_liberror::{Error, ErrorHandler, Location, location};
use std::sync::Arc;
use tokio::sync::Mutex;

const MIGRATIONS: EmbeddedMigrations = embed_migrations!("src/db/migrations");

type AsyncSqlite = SyncConnectionWrapper<SqliteConnection>;

/// Shared handle to the server's SQLite database. Cloning is cheap (an
/// `Arc` around a single mutex-guarded connection) and hands out
/// [`CertRepository`]/[`StackRepository`] instances for typed access.
#[derive(Clone)]
pub(crate) struct Db {
    conn: Arc<Mutex<AsyncSqlite>>,
}

impl Db {
    /// Open (creating if absent) the SQLite database at `database_url`,
    /// running any pending migrations before the connection is handed out.
    pub(crate) async fn open(database_url: &str) -> Result<Self, Error> {
        if let Some(parent) = std::path::Path::new(database_url).parent()
            && !parent.as_os_str().is_empty()
        {
            tokio::fs::create_dir_all(parent)
                .await
                .handle_err(location!())?;
        }

        // Migrations run through a plain synchronous connection: diesel_migrations'
        // `MigrationHarness` is only implemented for synchronous `diesel::Connection`s.
        let url = database_url.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut conn = SqliteConnection::establish(&url).handle_err(location!())?;
            conn.run_pending_migrations(MIGRATIONS)
                .handle_err(location!())?;
            Ok::<(), Error>(())
        })
        .await
        .handle_err(location!())??;

        let mut conn = AsyncSqlite::establish(database_url)
            .await
            .handle_err(location!())?;
        // one writer at a time (serialized by `conn`'s mutex) + concurrent readers
        diesel::sql_query("PRAGMA journal_mode = WAL;")
            .execute(&mut conn)
            .await
            .handle_err(location!())?;
        // SQLite ignores FK constraints per-connection unless told otherwise;
        // the auth tables' ON DELETE CASCADE (and dns_credentials' existing
        // one) depend on this being set.
        diesel::sql_query("PRAGMA foreign_keys = ON;")
            .execute(&mut conn)
            .await
            .handle_err(location!())?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub(crate) fn certs(&self) -> CertRepository {
        CertRepository::new(self.conn.clone())
    }

    pub(crate) fn stacks(&self) -> StackRepository {
        StackRepository::new(self.conn.clone())
    }

    pub(crate) fn users(&self) -> UserRepository {
        UserRepository::new(self.conn.clone())
    }

    pub(crate) fn scopes(&self) -> ScopeRepository {
        ScopeRepository::new(self.conn.clone())
    }

    pub(crate) fn refresh_tokens(&self) -> RefreshTokenRepository {
        RefreshTokenRepository::new(self.conn.clone())
    }

    pub(crate) fn login_attempts(&self) -> LoginAttemptRepository {
        LoginAttemptRepository::new(self.conn.clone())
    }

    pub(crate) fn events(&self) -> EventRepository {
        EventRepository::new(self.conn.clone())
    }
}

/// Unix seconds, for `updated_at` columns.
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::Db;

    #[tokio::test]
    async fn open_runs_migrations_and_is_reusable() {
        let dir = tempfile_dir();
        let path = dir.join("test.db");
        let db = Db::open(path.to_str().unwrap()).await.unwrap();
        // reopening the same file should be a no-op migration-wise
        drop(db);
        let db = Db::open(path.to_str().unwrap()).await.unwrap();
        assert!(db.certs().domains().await.unwrap().is_empty());
    }

    fn tempfile_dir() -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("nullnet-server-db-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
