use crate::auth::{Role, password};
use crate::db::Db;
use nullnet_liberror::Error;
use uuid::Uuid;

/// If any user already exists, this is a no-op. Otherwise creates the first
/// admin account from `bootstrap_username`/`bootstrap_password` if both are
/// non-empty; if either is missing, warns loudly and continues rather than
/// failing startup (mirrors the `INGRESS_ALLOW_TCP_PORTS.is_empty()`
/// warn-not-crash pattern in `main.rs`) — the server starts with zero users,
/// and login rejects everyone until an operator sets
/// `ADMIN_BOOTSTRAP_USERNAME`/`ADMIN_BOOTSTRAP_PASSWORD` and restarts.
/// Idempotent: safe to leave those env vars set permanently, since this only
/// ever runs while the `users` table is empty.
///
/// Takes the candidate username/password as plain params (rather than
/// reading the env vars itself) so callers — `main.rs` in production, tests
/// here — control them directly instead of mutating shared process env state.
pub(crate) async fn ensure_admin_exists(
    db: &Db,
    bootstrap_username: Option<&str>,
    bootstrap_password: Option<&str>,
) -> Result<(), Error> {
    let users = db.users();
    if users.count().await? > 0 {
        return Ok(());
    }

    let username = bootstrap_username.filter(|s| !s.trim().is_empty());
    let plain_password = bootstrap_password.filter(|s| !s.is_empty());
    let (Some(username), Some(plain_password)) = (username, plain_password) else {
        println!(
            "WARNING: no users exist and ADMIN_BOOTSTRAP_USERNAME/ADMIN_BOOTSTRAP_PASSWORD are \
             not both set — the admin UI will be inaccessible until an initial admin is created \
             (set these env vars and restart)."
        );
        return Ok(());
    };

    let id = Uuid::new_v4().to_string();
    let password_hash = password::hash(plain_password)?;
    users
        .create(&id, username, &password_hash, Role::Admin.as_str())
        .await?;
    println!("Bootstrapped initial admin account '{username}'.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::ensure_admin_exists;
    use crate::db::Db;
    use std::sync::atomic::{AtomicUsize, Ordering};

    async fn test_db() -> Db {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "nullnet-server-bootstrap-test-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Db::open(dir.join("test.db").to_str().unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn does_nothing_if_bootstrap_args_absent() {
        let db = test_db().await;
        ensure_admin_exists(&db, None, None).await.unwrap();
        assert_eq!(db.users().count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn creates_admin_when_bootstrap_args_set() {
        let db = test_db().await;
        ensure_admin_exists(&db, Some("root"), Some("hunter22222"))
            .await
            .unwrap();

        let user = db.users().by_username("root").await.unwrap().unwrap();
        assert_eq!(user.role, "admin");
        assert_ne!(user.password_hash, "hunter22222", "must be hashed");
    }

    #[tokio::test]
    async fn is_idempotent_once_a_user_exists() {
        let db = test_db().await;
        db.users()
            .create("id-1", "someone", "hash", "user")
            .await
            .unwrap();

        ensure_admin_exists(&db, Some("root"), Some("hunter22222"))
            .await
            .unwrap();
        assert!(db.users().by_username("root").await.unwrap().is_none());
    }
}
