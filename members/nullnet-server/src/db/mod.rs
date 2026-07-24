//! SQLite-backed storage for the server. All database access (schema,
//! migrations, repositories) is contained in this module; callers only see
//! [`Db`] and the repository types it hands out.
//!
//! `certs.rs`/`services/input.rs` still own the on-disk file storage they
//! always have; nothing calls into these repositories yet, so most of this
//! module's API surface is unused until that migration happens.
#![allow(dead_code)]

mod certs;
mod models;
mod schema;
mod services;

pub(crate) use certs::CertRepository;
pub(crate) use services::ServiceRepository;

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
/// [`CertRepository`]/[`ServiceRepository`] instances for typed access.
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

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub(crate) fn certs(&self) -> CertRepository {
        CertRepository::new(self.conn.clone())
    }

    pub(crate) fn services(&self) -> ServiceRepository {
        ServiceRepository::new(self.conn.clone())
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
