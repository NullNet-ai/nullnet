use crate::db::AsyncSqlite;
use crate::db::models::{NewStackConfig, StackConfig};
use crate::db::schema::stack_configs;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use nullnet_liberror::{Error, ErrorHandler, Location, location};
use std::sync::Arc;
use tokio::sync::Mutex;

/// A stack's config record: `stack` is the name, `config_toml` its raw stack
/// TOML text — the same bytes `services/input.rs` parses, just sourced from
/// the DB instead of `./services/<stack>.toml`.
pub(crate) struct StackConfigRecord {
    pub(crate) stack: String,
    pub(crate) config_toml: String,
}

impl From<StackConfig> for StackConfigRecord {
    fn from(row: StackConfig) -> Self {
        Self {
            stack: row.stack,
            config_toml: row.config_toml,
        }
    }
}

/// Typed access to the `stack_configs` table — the config store `services/
/// input.rs` and the `/api/config`/`/api/routes` HTTP handlers read from and
/// write to, replacing the old per-stack TOML files.
pub(crate) struct StackConfigRepository {
    conn: Arc<Mutex<AsyncSqlite>>,
}

impl StackConfigRepository {
    pub(super) fn new(conn: Arc<Mutex<AsyncSqlite>>) -> Self {
        Self { conn }
    }

    pub(crate) async fn put(&self, stack: &str, config_toml: &str) -> Result<(), Error> {
        let new_config = NewStackConfig {
            stack,
            config_toml,
            updated_at: super::now(),
        };
        let mut conn = self.conn.lock().await;
        diesel::insert_into(stack_configs::table)
            .values(&new_config)
            .on_conflict(stack_configs::stack)
            .do_update()
            .set(&new_config)
            .execute(&mut *conn)
            .await
            .handle_err(location!())?;
        Ok(())
    }

    pub(crate) async fn get(&self, stack: &str) -> Result<Option<StackConfigRecord>, Error> {
        let mut conn = self.conn.lock().await;
        let row = stack_configs::table
            .find(stack)
            .first::<StackConfig>(&mut *conn)
            .await
            .optional()
            .handle_err(location!())?;
        Ok(row.map(StackConfigRecord::from))
    }

    pub(crate) async fn list(&self) -> Result<Vec<StackConfigRecord>, Error> {
        let mut conn = self.conn.lock().await;
        let rows = stack_configs::table
            .load::<StackConfig>(&mut *conn)
            .await
            .handle_err(location!())?;
        Ok(rows.into_iter().map(StackConfigRecord::from).collect())
    }

    pub(crate) async fn delete(&self, stack: &str) -> Result<(), Error> {
        let mut conn = self.conn.lock().await;
        diesel::delete(stack_configs::table.find(stack))
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
            "nullnet-server-stack-configs-test-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Db::open(dir.join("test.db").to_str().unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn put_get_list_delete_round_trip() {
        let db = test_db().await;
        let repo = db.stack_configs();

        assert!(repo.get("stack-a").await.unwrap().is_none());

        repo.put("stack-a", "[[services]]\nname = \"web\"\n")
            .await
            .unwrap();
        let fetched = repo.get("stack-a").await.unwrap().unwrap();
        assert_eq!(fetched.stack, "stack-a");
        assert_eq!(fetched.config_toml, "[[services]]\nname = \"web\"\n");

        repo.put("stack-a", "[[services]]\nname = \"web2\"\n")
            .await
            .unwrap();
        assert_eq!(
            repo.get("stack-a").await.unwrap().unwrap().config_toml,
            "[[services]]\nname = \"web2\"\n"
        );

        repo.put("stack-b", "").await.unwrap();
        assert_eq!(repo.list().await.unwrap().len(), 2);

        repo.delete("stack-a").await.unwrap();
        assert!(repo.get("stack-a").await.unwrap().is_none());
        assert_eq!(repo.list().await.unwrap().len(), 1);
    }
}
