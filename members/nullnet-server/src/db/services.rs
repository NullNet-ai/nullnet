use crate::db::AsyncSqlite;
use crate::db::models::{NewService, Service};
use crate::db::schema::services;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use nullnet_liberror::{Error, ErrorHandler, Location, location};
use std::sync::Arc;
use tokio::sync::Mutex;

/// A service-stack record: `stack` is the name, `service_json` the serialized
/// `Vec<ServiceInfo>` (kept as opaque JSON for now to minimize migration risk).
pub(crate) struct ServiceRecord {
    pub(crate) stack: String,
    pub(crate) service_json: String,
}

impl From<Service> for ServiceRecord {
    fn from(row: Service) -> Self {
        Self {
            stack: row.stack,
            service_json: row.service_json,
        }
    }
}

/// Typed access to the `services` table, mirroring `services/input.rs`'s
/// current per-stack-TOML-file storage.
pub(crate) struct ServiceRepository {
    conn: Arc<Mutex<AsyncSqlite>>,
}

impl ServiceRepository {
    pub(super) fn new(conn: Arc<Mutex<AsyncSqlite>>) -> Self {
        Self { conn }
    }

    pub(crate) async fn put(&self, stack: &str, service_json: &str) -> Result<(), Error> {
        let new_service = NewService {
            stack,
            service_json,
            updated_at: super::now(),
        };
        let mut conn = self.conn.lock().await;
        diesel::insert_into(services::table)
            .values(&new_service)
            .on_conflict(services::stack)
            .do_update()
            .set(&new_service)
            .execute(&mut *conn)
            .await
            .handle_err(location!())?;
        Ok(())
    }

    pub(crate) async fn get(&self, stack: &str) -> Result<Option<ServiceRecord>, Error> {
        let mut conn = self.conn.lock().await;
        let row = services::table
            .find(stack)
            .first::<Service>(&mut *conn)
            .await
            .optional()
            .handle_err(location!())?;
        Ok(row.map(ServiceRecord::from))
    }

    pub(crate) async fn list(&self) -> Result<Vec<ServiceRecord>, Error> {
        let mut conn = self.conn.lock().await;
        let rows = services::table
            .load::<Service>(&mut *conn)
            .await
            .handle_err(location!())?;
        Ok(rows.into_iter().map(ServiceRecord::from).collect())
    }

    pub(crate) async fn delete(&self, stack: &str) -> Result<(), Error> {
        let mut conn = self.conn.lock().await;
        diesel::delete(services::table.find(stack))
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
            "nullnet-server-services-test-{}-{n}",
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
        let repo = db.services();

        assert!(repo.get("stack-a").await.unwrap().is_none());

        repo.put("stack-a", r#"[{"name":"web"}]"#).await.unwrap();
        let fetched = repo.get("stack-a").await.unwrap().unwrap();
        assert_eq!(fetched.stack, "stack-a");
        assert_eq!(fetched.service_json, r#"[{"name":"web"}]"#);

        repo.put("stack-a", r#"[{"name":"web2"}]"#).await.unwrap();
        assert_eq!(
            repo.get("stack-a").await.unwrap().unwrap().service_json,
            r#"[{"name":"web2"}]"#
        );

        repo.put("stack-b", r#"[]"#).await.unwrap();
        assert_eq!(repo.list().await.unwrap().len(), 2);

        repo.delete("stack-a").await.unwrap();
        assert!(repo.get("stack-a").await.unwrap().is_none());
        assert_eq!(repo.list().await.unwrap().len(), 1);
    }
}
