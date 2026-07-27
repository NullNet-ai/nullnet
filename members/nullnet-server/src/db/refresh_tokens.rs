use crate::db::AsyncSqlite;
use crate::db::models::{NewRefreshToken, RefreshToken};
use crate::db::schema::refresh_tokens;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use nullnet_liberror::{Error, ErrorHandler, Location, location};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Typed access to the `refresh_tokens` table. Callers pass/receive only the
/// SHA-256 hash of the raw opaque token — the raw value itself never touches
/// storage, only the cookie the browser holds.
pub(crate) struct RefreshTokenRepository {
    conn: Arc<Mutex<AsyncSqlite>>,
}

impl RefreshTokenRepository {
    pub(super) fn new(conn: Arc<Mutex<AsyncSqlite>>) -> Self {
        Self { conn }
    }

    pub(crate) async fn insert(
        &self,
        token_hash: &str,
        user_id: &str,
        expires_at: i64,
    ) -> Result<(), Error> {
        let new_token = NewRefreshToken {
            token_hash,
            user_id,
            expires_at,
            created_at: super::now(),
        };
        let mut conn = self.conn.lock().await;
        diesel::insert_into(refresh_tokens::table)
            .values(&new_token)
            .execute(&mut *conn)
            .await
            .handle_err(location!())?;
        Ok(())
    }

    /// The token row for `token_hash`, only if it's neither revoked nor expired.
    pub(crate) async fn find_active(
        &self,
        token_hash: &str,
    ) -> Result<Option<RefreshToken>, Error> {
        let now = super::now();
        let mut conn = self.conn.lock().await;
        refresh_tokens::table
            .find(token_hash)
            .filter(refresh_tokens::revoked_at.is_null())
            .filter(refresh_tokens::expires_at.gt(now))
            .first::<RefreshToken>(&mut *conn)
            .await
            .optional()
            .handle_err(location!())
    }

    pub(crate) async fn revoke(&self, token_hash: &str) -> Result<(), Error> {
        let mut conn = self.conn.lock().await;
        diesel::update(refresh_tokens::table.find(token_hash))
            .set(refresh_tokens::revoked_at.eq(Some(super::now())))
            .execute(&mut *conn)
            .await
            .handle_err(location!())?;
        Ok(())
    }

    /// Revoke every active token for `user_id` — used when an admin deletes
    /// or demotes a user, or a password changes, to kill existing sessions.
    pub(crate) async fn revoke_all_for_user(&self, user_id: &str) -> Result<(), Error> {
        let mut conn = self.conn.lock().await;
        diesel::update(
            refresh_tokens::table
                .filter(refresh_tokens::user_id.eq(user_id))
                .filter(refresh_tokens::revoked_at.is_null()),
        )
        .set(refresh_tokens::revoked_at.eq(Some(super::now())))
        .execute(&mut *conn)
        .await
        .handle_err(location!())?;
        Ok(())
    }

    /// Rotate-on-use: revoke `old_hash` and insert a fresh row for `new_hash`
    /// in one critical section (the shared connection is already serialized
    /// by its mutex, so no explicit transaction is needed for atomicity here).
    pub(crate) async fn rotate(
        &self,
        old_hash: &str,
        new_hash: &str,
        user_id: &str,
        new_expires_at: i64,
    ) -> Result<(), Error> {
        let new_token = NewRefreshToken {
            token_hash: new_hash,
            user_id,
            expires_at: new_expires_at,
            created_at: super::now(),
        };
        let mut conn = self.conn.lock().await;
        diesel::update(refresh_tokens::table.find(old_hash))
            .set(refresh_tokens::revoked_at.eq(Some(super::now())))
            .execute(&mut *conn)
            .await
            .handle_err(location!())?;
        diesel::insert_into(refresh_tokens::table)
            .values(&new_token)
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
            "nullnet-server-refresh-tokens-test-{}-{n}",
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
    async fn insert_find_revoke() {
        let db = test_db().await;
        let repo = db.refresh_tokens();
        let future = super::super::now() + 3600;

        repo.insert("hash-a", "id-1", future).await.unwrap();
        assert!(repo.find_active("hash-a").await.unwrap().is_some());

        repo.revoke("hash-a").await.unwrap();
        assert!(repo.find_active("hash-a").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn expired_token_is_not_active() {
        let db = test_db().await;
        let repo = db.refresh_tokens();
        let past = super::super::now() - 10;

        repo.insert("hash-a", "id-1", past).await.unwrap();
        assert!(repo.find_active("hash-a").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn rotate_revokes_old_and_creates_new() {
        let db = test_db().await;
        let repo = db.refresh_tokens();
        let future = super::super::now() + 3600;

        repo.insert("hash-old", "id-1", future).await.unwrap();
        repo.rotate("hash-old", "hash-new", "id-1", future)
            .await
            .unwrap();

        assert!(repo.find_active("hash-old").await.unwrap().is_none());
        assert!(repo.find_active("hash-new").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn revoke_all_for_user() {
        let db = test_db().await;
        let repo = db.refresh_tokens();
        let future = super::super::now() + 3600;

        repo.insert("hash-a", "id-1", future).await.unwrap();
        repo.insert("hash-b", "id-1", future).await.unwrap();
        repo.revoke_all_for_user("id-1").await.unwrap();

        assert!(repo.find_active("hash-a").await.unwrap().is_none());
        assert!(repo.find_active("hash-b").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn deleting_user_cascades_tokens() {
        let db = test_db().await;
        let repo = db.refresh_tokens();
        let future = super::super::now() + 3600;
        repo.insert("hash-a", "id-1", future).await.unwrap();

        db.users().delete("id-1").await.unwrap();
        assert!(repo.find_active("hash-a").await.unwrap().is_none());
    }
}
