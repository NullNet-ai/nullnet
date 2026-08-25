use crate::db::AsyncSqlite;
use crate::db::models::{
    NewRouteRow, NewServiceDependencyRow, NewServiceRow, NewServiceTriggerRow, NewStack, RouteRow,
    ServiceDependencyRow, ServiceRow, ServiceTriggerRow,
};
use crate::db::schema::{routes, service_dependencies, service_triggers, services, stacks};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use nullnet_liberror::{Error, ErrorHandler, Location, location};
use std::sync::Arc;
use tokio::sync::Mutex;

/// One service's row data for a [`StackRepository::put_services`] call: its
/// own columns plus its already-JSON-encoded trigger and dependency-branch
/// children — encoding a chain to JSON is the caller's job (it knows what a
/// "chain" means; this module just stores TEXT columns).
pub(crate) struct ServiceInsert<'a> {
    pub(crate) name: &'a str,
    pub(crate) docker_container: Option<&'a str>,
    pub(crate) process_path: Option<&'a str>,
    pub(crate) port: Option<i32>,
    pub(crate) timeout: Option<i64>,
    pub(crate) max_networks: Option<i32>,
    pub(crate) protocol: Option<&'a str>,
    pub(crate) listen_port: Option<i32>,
    pub(crate) egress_blocked_countries: Option<String>,
    pub(crate) egress_allowed_countries: Option<String>,
    pub(crate) ingress_blocked_countries: Option<String>,
    pub(crate) ingress_allowed_countries: Option<String>,
    /// `(port, JSON-encoded chain)`, one per `[[services.triggers]]` entry.
    pub(crate) triggers: Vec<(i32, String)>,
    /// JSON-encoded chain, one per `proxy_dependencies` branch, in order.
    pub(crate) dependencies: Vec<String>,
}

pub(crate) struct RouteInsert<'a> {
    pub(crate) host: &'a str,
    pub(crate) path: &'a str,
    pub(crate) target_kind: &'a str,
    pub(crate) target_service: Option<&'a str>,
    pub(crate) strip_prefix: bool,
    pub(crate) redirect_to: Option<&'a str>,
    pub(crate) redirect_status: Option<i32>,
    pub(crate) preserve_path: bool,
    pub(crate) preserve_query: bool,
}

/// Typed access to a stack's normalized config: `stacks` (existence),
/// `services`/`service_triggers`/`service_dependencies`, and `routes`. A
/// stack is the atomic unit the rest of the app always loads/saves as a
/// whole (mirrors `ParsedStack`/the whole-list-replace HTTP contract), so
/// `put_services`/`put_routes` each replace their stack's full list in one
/// held connection lock — the same "acquire once, do several statements"
/// pattern `RefreshTokenRepository::rotate` already uses.
pub(crate) struct StackRepository {
    conn: Arc<Mutex<AsyncSqlite>>,
}

impl StackRepository {
    pub(super) fn new(conn: Arc<Mutex<AsyncSqlite>>) -> Self {
        Self { conn }
    }

    pub(crate) async fn list_stacks(&self) -> Result<Vec<String>, Error> {
        let mut conn = self.conn.lock().await;
        stacks::table
            .select(stacks::name)
            .load(&mut *conn)
            .await
            .handle_err(location!())
    }

    pub(crate) async fn exists(&self, stack: &str) -> Result<bool, Error> {
        let mut conn = self.conn.lock().await;
        let found: Option<String> = stacks::table
            .find(stack)
            .select(stacks::name)
            .first(&mut *conn)
            .await
            .optional()
            .handle_err(location!())?;
        Ok(found.is_some())
    }

    /// Insert the stack if it's new, otherwise just bump `updated_at` — this
    /// runs on every services/routes save (not only on first creation), so
    /// it doubles as the stack's "last modified" timestamp.
    async fn ensure_exists(conn: &mut AsyncSqlite, stack: &str) -> Result<(), Error> {
        let new_row = NewStack {
            name: stack,
            updated_at: super::now(),
        };
        diesel::insert_into(stacks::table)
            .values(&new_row)
            .on_conflict(stacks::name)
            .do_update()
            .set(&new_row)
            .execute(conn)
            .await
            .handle_err(location!())?;
        Ok(())
    }

    pub(crate) async fn services_for(&self, stack: &str) -> Result<Vec<ServiceRow>, Error> {
        let mut conn = self.conn.lock().await;
        services::table
            .filter(services::stack.eq(stack))
            .load(&mut *conn)
            .await
            .handle_err(location!())
    }

    pub(crate) async fn triggers_for(
        &self,
        service_ids: &[i32],
    ) -> Result<Vec<ServiceTriggerRow>, Error> {
        let mut conn = self.conn.lock().await;
        service_triggers::table
            .filter(service_triggers::service_id.eq_any(service_ids))
            .load(&mut *conn)
            .await
            .handle_err(location!())
    }

    pub(crate) async fn dependencies_for(
        &self,
        service_ids: &[i32],
    ) -> Result<Vec<ServiceDependencyRow>, Error> {
        let mut conn = self.conn.lock().await;
        service_dependencies::table
            .filter(service_dependencies::service_id.eq_any(service_ids))
            .order((
                service_dependencies::service_id,
                service_dependencies::branch_index,
            ))
            .load(&mut *conn)
            .await
            .handle_err(location!())
    }

    pub(crate) async fn routes_for(&self, stack: &str) -> Result<Vec<RouteRow>, Error> {
        let mut conn = self.conn.lock().await;
        routes::table
            .filter(routes::stack.eq(stack))
            .load(&mut *conn)
            .await
            .handle_err(location!())
    }

    /// Whole-list replace: ensure the stack exists (so this also doubles as
    /// "create a stack"), delete its current `services` rows (cascades
    /// their `service_triggers`/`service_dependencies` rows), and insert
    /// `new_services`.
    pub(crate) async fn put_services(
        &self,
        stack: &str,
        new_services: &[ServiceInsert<'_>],
    ) -> Result<(), Error> {
        let mut conn = self.conn.lock().await;
        Self::ensure_exists(&mut conn, stack).await?;
        diesel::delete(services::table.filter(services::stack.eq(stack)))
            .execute(&mut *conn)
            .await
            .handle_err(location!())?;

        for s in new_services {
            let new_row = NewServiceRow {
                stack,
                name: s.name,
                docker_container: s.docker_container,
                process_path: s.process_path,
                port: s.port,
                timeout: s.timeout,
                max_networks: s.max_networks,
                protocol: s.protocol,
                listen_port: s.listen_port,
                egress_blocked_countries: s.egress_blocked_countries.clone(),
                egress_allowed_countries: s.egress_allowed_countries.clone(),
                ingress_blocked_countries: s.ingress_blocked_countries.clone(),
                ingress_allowed_countries: s.ingress_allowed_countries.clone(),
            };
            let service_id: i32 = diesel::insert_into(services::table)
                .values(&new_row)
                .returning(services::id)
                .get_result(&mut *conn)
                .await
                .handle_err(location!())?;

            // SQLite doesn't support diesel's multi-row batch insert (a
            // single INSERT with several VALUES tuples) the way Postgres
            // does, so each row is its own statement — fine at this scale
            // (a handful of triggers/branches per service, an admin-time
            // operation, not a hot path).
            for (port, chain) in &s.triggers {
                diesel::insert_into(service_triggers::table)
                    .values(NewServiceTriggerRow {
                        service_id,
                        port: *port,
                        chain: chain.clone(),
                    })
                    .execute(&mut *conn)
                    .await
                    .handle_err(location!())?;
            }
            for (i, chain) in s.dependencies.iter().enumerate() {
                diesel::insert_into(service_dependencies::table)
                    .values(NewServiceDependencyRow {
                        service_id,
                        branch_index: i32::try_from(i).unwrap_or(i32::MAX),
                        chain: chain.clone(),
                    })
                    .execute(&mut *conn)
                    .await
                    .handle_err(location!())?;
            }
        }
        Ok(())
    }

    /// Whole-list replace for `routes`, same shape as `put_services`.
    pub(crate) async fn put_routes(
        &self,
        stack: &str,
        new_routes: &[RouteInsert<'_>],
    ) -> Result<(), Error> {
        let mut conn = self.conn.lock().await;
        Self::ensure_exists(&mut conn, stack).await?;
        diesel::delete(routes::table.filter(routes::stack.eq(stack)))
            .execute(&mut *conn)
            .await
            .handle_err(location!())?;

        for r in new_routes {
            diesel::insert_into(routes::table)
                .values(NewRouteRow {
                    stack,
                    host: r.host,
                    path: r.path,
                    target_kind: r.target_kind,
                    target_service: r.target_service,
                    strip_prefix: r.strip_prefix,
                    redirect_to: r.redirect_to,
                    redirect_status: r.redirect_status,
                    preserve_path: r.preserve_path,
                    preserve_query: r.preserve_query,
                })
                .execute(&mut *conn)
                .await
                .handle_err(location!())?;
        }
        Ok(())
    }

    /// Delete the stack; cascades to its services (and their
    /// triggers/dependencies) and routes.
    pub(crate) async fn delete(&self, stack: &str) -> Result<(), Error> {
        let mut conn = self.conn.lock().await;
        diesel::delete(stacks::table.find(stack))
            .execute(&mut *conn)
            .await
            .handle_err(location!())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    async fn test_db() -> Db {
        static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "nullnet-server-stacks-test-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Db::open(dir.join("test.db").to_str().unwrap())
            .await
            .unwrap()
    }

    fn service(name: &str) -> ServiceInsert<'_> {
        ServiceInsert {
            name,
            docker_container: Some("my-app_web"),
            process_path: None,
            port: Some(8080),
            timeout: Some(0),
            max_networks: None,
            protocol: None,
            listen_port: None,
            egress_blocked_countries: None,
            egress_allowed_countries: None,
            ingress_blocked_countries: None,
            ingress_allowed_countries: None,
            triggers: vec![(5555, "[\"worker\"]".to_string())],
            dependencies: vec!["[\"db\",\"cache\"]".to_string()],
        }
    }

    #[tokio::test]
    async fn put_services_round_trips_service_triggers_and_dependencies() {
        let db = test_db().await;
        let repo = db.stacks();

        repo.put_services("alpha", &[service("web")]).await.unwrap();

        assert!(repo.exists("alpha").await.unwrap());
        let services = repo.services_for("alpha").await.unwrap();
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].name, "web");
        assert_eq!(services[0].docker_container.as_deref(), Some("my-app_web"));

        let ids: Vec<i32> = services.iter().map(|s| s.id).collect();
        let triggers = repo.triggers_for(&ids).await.unwrap();
        assert_eq!(triggers.len(), 1);
        assert_eq!(triggers[0].port, 5555);
        assert_eq!(triggers[0].chain, "[\"worker\"]");

        let deps = repo.dependencies_for(&ids).await.unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].chain, "[\"db\",\"cache\"]");
    }

    #[tokio::test]
    async fn put_services_replaces_the_full_list() {
        let db = test_db().await;
        let repo = db.stacks();

        repo.put_services("alpha", &[service("web"), service("api")])
            .await
            .unwrap();
        assert_eq!(repo.services_for("alpha").await.unwrap().len(), 2);

        // Second call replaces, not appends: only "web" remains, and its old
        // service_id's triggers/dependencies are gone too (cascade).
        repo.put_services("alpha", &[service("web")]).await.unwrap();
        let services = repo.services_for("alpha").await.unwrap();
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].name, "web");
        let triggers = repo.triggers_for(&[services[0].id]).await.unwrap();
        assert_eq!(triggers.len(), 1); // re-created for the new row, not doubled
    }

    #[tokio::test]
    async fn put_routes_round_trips_and_replaces() {
        let db = test_db().await;
        let repo = db.stacks();

        let route = RouteInsert {
            host: "ops.example.com",
            path: "/",
            target_kind: "service",
            target_service: Some("web"),
            strip_prefix: true,
            redirect_to: None,
            redirect_status: None,
            preserve_path: false,
            preserve_query: false,
        };
        repo.put_routes("alpha", &[route]).await.unwrap();
        let routes = repo.routes_for("alpha").await.unwrap();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].host, "ops.example.com");
        assert_eq!(routes[0].target_service.as_deref(), Some("web"));
        assert!(routes[0].strip_prefix);

        repo.put_routes("alpha", &[]).await.unwrap();
        assert!(repo.routes_for("alpha").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn delete_cascades_services_triggers_dependencies_and_routes() {
        let db = test_db().await;
        let repo = db.stacks();

        repo.put_services("alpha", &[service("web")]).await.unwrap();
        let redirect = RouteInsert {
            host: "ops.example.com",
            path: "/",
            target_kind: "redirect",
            target_service: None,
            strip_prefix: false,
            redirect_to: Some("https://elsewhere.example.com"),
            redirect_status: Some(301),
            preserve_path: false,
            preserve_query: false,
        };
        repo.put_routes("alpha", &[redirect]).await.unwrap();

        let service_id = repo.services_for("alpha").await.unwrap()[0].id;

        repo.delete("alpha").await.unwrap();

        assert!(!repo.exists("alpha").await.unwrap());
        assert!(repo.services_for("alpha").await.unwrap().is_empty());
        assert!(repo.routes_for("alpha").await.unwrap().is_empty());
        assert!(repo.triggers_for(&[service_id]).await.unwrap().is_empty());
        assert!(
            repo.dependencies_for(&[service_id])
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn put_services_creates_the_stack_row_for_a_new_name() {
        let db = test_db().await;
        let repo = db.stacks();
        assert!(!repo.exists("brand-new").await.unwrap());
        repo.put_services("brand-new", &[]).await.unwrap();
        assert!(repo.exists("brand-new").await.unwrap());
        assert!(repo.services_for("brand-new").await.unwrap().is_empty());
    }
}
