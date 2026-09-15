use super::AsyncSqlite;
use diesel::prelude::*;
use diesel::sql_types::{BigInt, Bool, Nullable, Text};
use diesel_async::{AsyncConnection, RunQueryDsl};
use nullnet_liberror::{Error, ErrorHandler, Location, location};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(QueryableByName)]
pub(crate) struct ObservationRow {
    #[diesel(sql_type = BigInt)]
    pub(crate) id: i64,
    #[diesel(sql_type = Text)]
    pub(crate) stack: String,
    #[diesel(sql_type = BigInt)]
    pub(crate) started_at: i64,
    #[diesel(sql_type = Nullable<BigInt>)]
    pub(crate) ended_at: Option<i64>,
    #[diesel(sql_type = Bool)]
    pub(crate) applied: bool,
    #[diesel(sql_type = Text)]
    pub(crate) saved_config: String,
}

#[derive(QueryableByName, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub(crate) struct ObservedEdge {
    #[diesel(sql_type = Text)]
    pub(crate) source: String,
    #[diesel(sql_type = Text)]
    pub(crate) destination: String,
    #[diesel(sql_type = BigInt)]
    pub(crate) count: i64,
}

pub(crate) struct ObservationRepository {
    conn: Arc<Mutex<AsyncSqlite>>,
}

impl ObservationRepository {
    pub(super) fn new(conn: Arc<Mutex<AsyncSqlite>>) -> Self {
        Self { conn }
    }

    pub(crate) async fn list(&self, stack: &str) -> Result<Vec<ObservationRow>, Error> {
        diesel::sql_query("SELECT * FROM observations WHERE stack = ? ORDER BY id DESC")
            .bind::<Text, _>(stack)
            .load(&mut *self.conn.lock().await)
            .await
            .handle_err(location!())
    }

    pub(crate) async fn active(&self, stack: &str) -> Result<Option<ObservationRow>, Error> {
        diesel::sql_query("SELECT * FROM observations WHERE stack = ? AND ended_at IS NULL")
            .bind::<Text, _>(stack)
            .get_result(&mut *self.conn.lock().await)
            .await
            .optional()
            .handle_err(location!())
    }

    pub(crate) async fn find(&self, stack: &str, id: i64) -> Result<Option<ObservationRow>, Error> {
        diesel::sql_query("SELECT * FROM observations WHERE stack = ? AND id = ?")
            .bind::<Text, _>(stack)
            .bind::<BigInt, _>(id)
            .get_result(&mut *self.conn.lock().await)
            .await
            .optional()
            .handle_err(location!())
    }

    pub(crate) async fn start(
        &self,
        stack: &str,
        saved_config: &str,
    ) -> Result<ObservationRow, Error> {
        diesel::sql_query("INSERT INTO observations (stack, started_at, saved_config) VALUES (?, ?, ?) RETURNING *")
            .bind::<Text, _>(stack).bind::<BigInt, _>(super::now()).bind::<Text, _>(saved_config)
            .get_result(&mut *self.conn.lock().await).await.handle_err(location!())
    }

    pub(crate) async fn counts(&self, row: &ObservationRow) -> Result<Vec<ObservedEdge>, Error> {
        diesel::sql_query("SELECT source, destination, count FROM observation_counts WHERE observation_id = ? ORDER BY source, destination")
            .bind::<BigInt, _>(row.id).load(&mut *self.conn.lock().await).await.handle_err(location!())
    }

    pub(crate) async fn stop(&self, row: &ObservationRow) -> Result<ObservationRow, Error> {
        diesel::sql_query(
            "UPDATE observations SET ended_at = ? WHERE id = ? AND ended_at IS NULL RETURNING *",
        )
        .bind::<BigInt, _>(super::now())
        .bind::<BigInt, _>(row.id)
        .get_result(&mut *self.conn.lock().await)
        .await
        .handle_err(location!())
    }

    /// Keep the saved services intact; replace only triggers, atomically with the result marker.
    pub(crate) async fn apply(
        &self,
        row: &ObservationRow,
        services: &[crate::services::input::ServiceToml],
    ) -> Result<(), Error> {
        let mut conn = self.conn.lock().await;
        conn.transaction::<_, diesel::result::Error, _>(async move |conn| {
            diesel::sql_query("DELETE FROM service_triggers WHERE service_id IN (SELECT id FROM services WHERE stack = ?)")
                .bind::<Text, _>(&row.stack).execute(conn).await?;
            for service in services {
                for peer in &service.backends {
                    diesel::sql_query("INSERT INTO service_triggers (service_id, peer) SELECT id, ? FROM services WHERE stack = ? AND name = ?")
                        .bind::<Text, _>(peer).bind::<Text, _>(&row.stack).bind::<Text, _>(&service.name).execute(conn).await?;
                }
            }
            diesel::sql_query("UPDATE observations SET applied = 1 WHERE id = ?")
                .bind::<BigInt, _>(row.id).execute(conn).await?;
            Ok(())
        }).await.handle_err(location!())
    }
}
