use crate::db::AsyncSqlite;
use crate::db::models::NewUserScope;
use crate::db::schema::user_scopes;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use nullnet_liberror::{Error, ErrorHandler, Location, location};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Typed access to the `user_scopes` table — the explicit per-resource
/// read/write grants for `user`-role accounts. `admin`-role accounts never
/// need rows here; callers should treat admin as implicitly having every
/// scope rather than querying this table for them.
pub(crate) struct ScopeRepository {
    conn: Arc<Mutex<AsyncSqlite>>,
}

impl ScopeRepository {
    pub(super) fn new(conn: Arc<Mutex<AsyncSqlite>>) -> Self {
        Self { conn }
    }

    pub(crate) async fn for_user(&self, user_id: &str) -> Result<Vec<String>, Error> {
        let mut conn = self.conn.lock().await;
        user_scopes::table
            .filter(user_scopes::user_id.eq(user_id))
            .select(user_scopes::scope)
            .load(&mut *conn)
            .await
            .handle_err(location!())
    }

    /// Replace `user_id`'s full scope set with exactly `scopes` (delete-all,
    /// then insert) — simplest correct semantics for "an admin edited this
    /// user's scope checkboxes and saved."
    pub(crate) async fn set_for_user(&self, user_id: &str, scopes: &[String]) -> Result<(), Error> {
        let mut conn = self.conn.lock().await;
        diesel::delete(user_scopes::table.filter(user_scopes::user_id.eq(user_id)))
            .execute(&mut *conn)
            .await
            .handle_err(location!())?;
        // SQLite (via diesel-async's SyncConnectionWrapper) doesn't support a
        // single multi-row VALUES(...),(...) insert, so insert one row at a
        // time — the scope count per user is tiny (at most 8), so this is fine.
        for scope in scopes {
            diesel::insert_into(user_scopes::table)
                .values(&NewUserScope { user_id, scope })
                .execute(&mut *conn)
                .await
                .handle_err(location!())?;
        }
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
            "nullnet-server-scopes-test-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(dir.join("test.db").to_str().unwrap())
            .await
            .unwrap();
        db.users()
            .create("id-1", "alice", "hash", "user")
            .await
            .unwrap();
        db
    }

    #[tokio::test]
    async fn set_and_read_scopes() {
        let db = test_db().await;
        let repo = db.scopes();

        assert!(repo.for_user("id-1").await.unwrap().is_empty());

        repo.set_for_user(
            "id-1",
            &["certificates:read".to_string(), "events:read".to_string()],
        )
        .await
        .unwrap();
        let mut scopes = repo.for_user("id-1").await.unwrap();
        scopes.sort();
        assert_eq!(scopes, vec!["certificates:read", "events:read"]);

        // replace, not append
        repo.set_for_user("id-1", &["config:write".to_string()])
            .await
            .unwrap();
        assert_eq!(repo.for_user("id-1").await.unwrap(), vec!["config:write"]);

        // empty replace clears everything
        repo.set_for_user("id-1", &[]).await.unwrap();
        assert!(repo.for_user("id-1").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn deleting_user_cascades_scopes() {
        let db = test_db().await;
        db.scopes()
            .set_for_user("id-1", &["nodes:read".to_string()])
            .await
            .unwrap();
        db.users().delete("id-1").await.unwrap();
        assert!(db.scopes().for_user("id-1").await.unwrap().is_empty());
    }
}
