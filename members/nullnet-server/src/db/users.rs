use crate::db::AsyncSqlite;
use crate::db::models::{NewUser, User, UserUpdate};
use crate::db::schema::users;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use nullnet_liberror::{Error, ErrorHandler, Location, location};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Typed access to the `users` table. Callers outside `db` never see the
/// internal `NewUser`/`User` Diesel models — only plain scalar args and the
/// returned `User` row (also `pub(crate)`, but callers should treat it as
/// data, not construct one themselves).
pub(crate) struct UserRepository {
    conn: Arc<Mutex<AsyncSqlite>>,
}

impl UserRepository {
    pub(super) fn new(conn: Arc<Mutex<AsyncSqlite>>) -> Self {
        Self { conn }
    }

    /// Create a new user with no MFA configured yet. `id` is the caller's
    /// choice (a uuid v4 in practice) so it can be returned immediately
    /// without a round-trip read-back.
    pub(crate) async fn create(
        &self,
        id: &str,
        username: &str,
        password_hash: &str,
        role: &str,
    ) -> Result<(), Error> {
        let now = super::now();
        let new_user = NewUser {
            id,
            username,
            password_hash,
            role,
            mfa_secret_enc: None,
            mfa_confirmed_at: None,
            created_at: now,
            updated_at: now,
        };
        let mut conn = self.conn.lock().await;
        diesel::insert_into(users::table)
            .values(&new_user)
            .execute(&mut *conn)
            .await
            .handle_err(location!())?;
        Ok(())
    }

    pub(crate) async fn by_username(&self, username: &str) -> Result<Option<User>, Error> {
        let mut conn = self.conn.lock().await;
        users::table
            .filter(users::username.eq(username))
            .first::<User>(&mut *conn)
            .await
            .optional()
            .handle_err(location!())
    }

    pub(crate) async fn by_id(&self, id: &str) -> Result<Option<User>, Error> {
        let mut conn = self.conn.lock().await;
        users::table
            .find(id)
            .first::<User>(&mut *conn)
            .await
            .optional()
            .handle_err(location!())
    }

    pub(crate) async fn list(&self) -> Result<Vec<User>, Error> {
        let mut conn = self.conn.lock().await;
        users::table
            .load::<User>(&mut *conn)
            .await
            .handle_err(location!())
    }

    /// Total user count — used to decide whether bootstrap needs to run.
    pub(crate) async fn count(&self) -> Result<i64, Error> {
        let mut conn = self.conn.lock().await;
        users::table
            .count()
            .get_result(&mut *conn)
            .await
            .handle_err(location!())
    }

    /// Number of `admin`-role users — used to guard against deleting the last one.
    pub(crate) async fn count_admins(&self) -> Result<i64, Error> {
        let mut conn = self.conn.lock().await;
        users::table
            .filter(users::role.eq("admin"))
            .count()
            .get_result(&mut *conn)
            .await
            .handle_err(location!())
    }

    /// Partial update: only fields passed as `Some` are changed; `updated_at` is
    /// always bumped. Plain scalar args (rather than a `db::models` type) so
    /// callers outside this module — the Users admin API — don't need to
    /// name an internal Diesel model type.
    pub(crate) async fn update(
        &self,
        id: &str,
        username: Option<&str>,
        role: Option<&str>,
        password_hash: Option<&str>,
    ) -> Result<(), Error> {
        let changes = UserUpdate {
            username,
            role,
            password_hash,
            updated_at: Some(super::now()),
        };
        let mut conn = self.conn.lock().await;
        diesel::update(users::table.find(id))
            .set(&changes)
            .execute(&mut *conn)
            .await
            .handle_err(location!())?;
        Ok(())
    }

    pub(crate) async fn delete(&self, id: &str) -> Result<(), Error> {
        let mut conn = self.conn.lock().await;
        diesel::delete(users::table.find(id))
            .execute(&mut *conn)
            .await
            .handle_err(location!())?;
        Ok(())
    }

    /// Store a freshly generated (unconfirmed) TOTP secret, encrypted at rest.
    pub(crate) async fn set_mfa_pending(&self, id: &str, secret_enc: &str) -> Result<(), Error> {
        let mut conn = self.conn.lock().await;
        diesel::update(users::table.find(id))
            .set((
                users::mfa_secret_enc.eq(Some(secret_enc)),
                users::mfa_confirmed_at.eq(None::<i64>),
                users::updated_at.eq(super::now()),
            ))
            .execute(&mut *conn)
            .await
            .handle_err(location!())?;
        Ok(())
    }

    /// Mark the pending secret as confirmed — MFA is now enabled.
    pub(crate) async fn confirm_mfa(&self, id: &str) -> Result<(), Error> {
        let mut conn = self.conn.lock().await;
        diesel::update(users::table.find(id))
            .set((
                users::mfa_confirmed_at.eq(Some(super::now())),
                users::updated_at.eq(super::now()),
            ))
            .execute(&mut *conn)
            .await
            .handle_err(location!())?;
        Ok(())
    }

    /// Disable/reset MFA entirely (self-service disable, or an admin reset).
    pub(crate) async fn clear_mfa(&self, id: &str) -> Result<(), Error> {
        let mut conn = self.conn.lock().await;
        diesel::update(users::table.find(id))
            .set((
                users::mfa_secret_enc.eq(None::<String>),
                users::mfa_confirmed_at.eq(None::<i64>),
                users::updated_at.eq(super::now()),
            ))
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
            "nullnet-server-users-test-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Db::open(dir.join("test.db").to_str().unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn create_get_list_delete_round_trip() {
        let db = test_db().await;
        let repo = db.users();

        assert_eq!(repo.count().await.unwrap(), 0);
        repo.create("id-1", "alice", "hash", "admin").await.unwrap();

        let fetched = repo.by_username("alice").await.unwrap().unwrap();
        assert_eq!(fetched.id, "id-1");
        assert_eq!(repo.by_id("id-1").await.unwrap().unwrap().username, "alice");
        assert_eq!(repo.count().await.unwrap(), 1);
        assert_eq!(repo.count_admins().await.unwrap(), 1);
        assert_eq!(repo.list().await.unwrap().len(), 1);

        repo.delete("id-1").await.unwrap();
        assert!(repo.by_id("id-1").await.unwrap().is_none());
        assert_eq!(repo.count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn partial_update_only_touches_given_fields() {
        let db = test_db().await;
        let repo = db.users();
        repo.create("id-1", "alice", "hash", "admin").await.unwrap();

        repo.update("id-1", Some("alice2"), None, None)
            .await
            .unwrap();
        let fetched = repo.by_id("id-1").await.unwrap().unwrap();
        assert_eq!(fetched.username, "alice2");
        assert_eq!(fetched.role, "admin", "role should be untouched");
    }

    #[tokio::test]
    async fn mfa_lifecycle() {
        let db = test_db().await;
        let repo = db.users();
        repo.create("id-1", "alice", "hash", "admin").await.unwrap();

        let fetched = repo.by_id("id-1").await.unwrap().unwrap();
        assert!(fetched.mfa_confirmed_at.is_none());

        repo.set_mfa_pending("id-1", "encrypted-secret")
            .await
            .unwrap();
        let fetched = repo.by_id("id-1").await.unwrap().unwrap();
        assert_eq!(fetched.mfa_secret_enc.as_deref(), Some("encrypted-secret"));
        assert!(fetched.mfa_confirmed_at.is_none(), "not yet confirmed");

        repo.confirm_mfa("id-1").await.unwrap();
        assert!(
            repo.by_id("id-1")
                .await
                .unwrap()
                .unwrap()
                .mfa_confirmed_at
                .is_some()
        );

        repo.clear_mfa("id-1").await.unwrap();
        let fetched = repo.by_id("id-1").await.unwrap().unwrap();
        assert!(fetched.mfa_secret_enc.is_none());
        assert!(fetched.mfa_confirmed_at.is_none());
    }
}
