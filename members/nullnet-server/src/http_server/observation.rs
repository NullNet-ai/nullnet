use super::AppState;
use super::auth::{AuthContext, require_scope};
use super::config::{apply_loaded, rejected, valid_stack_name};
use crate::auth::Scope;
use crate::db::{ObservationRow, ObservedEdge};
use crate::events::Event;
use crate::services::input::{ServiceToml, ServicesToml, observation_backends};
use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde::Serialize;
use std::collections::HashSet;

#[derive(Serialize)]
pub(super) struct ObservationJson {
    id: i64,
    started_at: i64,
    ended_at: Option<i64>,
    applied: bool,
    services: Vec<String>,
    saved_links: Vec<Link>,
    counts: Vec<ObservedEdge>,
    added: Vec<Link>,
    removed: Vec<Link>,
}

#[derive(Serialize)]
struct Link {
    source: String,
    destination: String,
}

#[derive(Serialize)]
pub(super) struct ObservationList {
    stack: String,
    observations: Vec<ObservationJson>,
    conflicts: Vec<String>,
    trigger_count: usize,
}

fn internal(error: impl std::fmt::Debug) -> Response {
    eprintln!("Observation: {error:?}");
    rejected(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Could not update or load observation",
    )
}

pub(super) async fn require_inactive(state: &AppState, stack: &str) -> Result<(), Response> {
    if state
        .db
        .observations()
        .active(stack)
        .await
        .map_err(internal)?
        .is_some()
    {
        return Err(rejected(
            StatusCode::CONFLICT,
            "Stop observation before changing configuration",
        ));
    }
    Ok(())
}

async fn saved_services(state: &AppState, stack: &str) -> Result<Vec<ServiceToml>, Response> {
    ServicesToml::stack_services_from_db(&state.db, stack)
        .await
        .map_err(internal)?
        .ok_or_else(|| rejected(StatusCode::NOT_FOUND, "Stack not found"))
}

fn links(services: &[ServiceToml]) -> Vec<Link> {
    services
        .iter()
        .flat_map(|s| {
            s.backends.iter().map(|peer| Link {
                source: s.name.clone(),
                destination: peer.clone(),
            })
        })
        .collect()
}

fn suggested(mut services: Vec<ServiceToml>, counts: &[ObservedEdge]) -> Vec<ServiceToml> {
    let names: HashSet<_> = services.iter().map(|s| s.name.clone()).collect();
    for service in &mut services {
        service.backends = counts
            .iter()
            .filter(|edge| {
                edge.source == service.name
                    && edge.destination != service.name
                    && names.contains(&edge.destination)
                    && edge.count > 0
            })
            .map(|edge| edge.destination.clone())
            .collect();
    }
    services
}

async fn response(state: &AppState, row: &ObservationRow) -> Result<ObservationJson, Response> {
    let saved: Vec<ServiceToml> = serde_json::from_str(&row.saved_config).map_err(internal)?;
    let counts = state
        .db
        .observations()
        .counts(row)
        .await
        .map_err(internal)?;
    let saved_links = links(&saved);
    let next = links(&suggested(saved.clone(), &counts));
    let pairs = |links: &[Link]| -> HashSet<_> {
        links
            .iter()
            .map(|l| (l.source.clone(), l.destination.clone()))
            .collect()
    };
    let before = pairs(&saved_links);
    let after = pairs(&next);
    let added = next
        .into_iter()
        .filter(|l| !before.contains(&(l.source.clone(), l.destination.clone())))
        .collect();
    let removed = links(&saved)
        .into_iter()
        .filter(|l| !after.contains(&(l.source.clone(), l.destination.clone())))
        .collect();
    Ok(ObservationJson {
        id: row.id,
        started_at: row.started_at,
        ended_at: row.ended_at,
        applied: row.applied,
        services: saved.into_iter().map(|s| s.name).collect(),
        saved_links,
        counts,
        added,
        removed,
    })
}

pub(super) async fn list_handler(
    Extension(ctx): Extension<AuthContext>,
    Path(stack): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ObservationList>, Response> {
    require_scope(&ctx, Scope::ConfigRead)?;
    require_scope(&ctx, Scope::SessionsRead)?;
    let _guard = state.config_lock.lock().await;
    let mut services = saved_services(&state, &stack).await?;
    let trigger_count = services.len() * services.len().saturating_sub(1);
    let conflicts = observation_backends(&mut services)
        .err()
        .unwrap_or_default();
    let mut observations = Vec::new();
    for row in state
        .db
        .observations()
        .list(&stack)
        .await
        .map_err(internal)?
    {
        observations.push(response(&state, &row).await?);
    }
    Ok(Json(ObservationList {
        stack,
        observations,
        conflicts,
        trigger_count,
    }))
}

pub(super) async fn start_handler(
    Extension(ctx): Extension<AuthContext>,
    Path(stack): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ObservationJson>, Response> {
    tokio::spawn(async move {
        require_scope(&ctx, Scope::ConfigWrite)?;
        require_scope(&ctx, Scope::SessionsRead)?;
        if !valid_stack_name(&stack) {
            return Err(rejected(StatusCode::BAD_REQUEST, "Invalid stack name"));
        }
        let _guard = state.config_lock.lock().await;
        require_inactive(&state, &stack).await?;
        let mut saved = saved_services(&state, &stack).await?;
        let snapshot = serde_json::to_string(&saved).map_err(internal)?;
        observation_backends(&mut saved)
            .map_err(|errors| rejected(StatusCode::UNPROCESSABLE_ENTITY, errors.join("\n")))?;
        let (parsed, _, _) = ServicesToml::validate_new_services(&state.db, &stack, saved)
            .await
            .map_err(internal)?;
        let (mut loaded, index, routes) = ServicesToml::load(&state.db).await.map_err(internal)?;
        loaded.insert(stack.clone(), parsed);
        let row = state
            .db
            .observations()
            .start(&stack, &snapshot)
            .await
            .map_err(internal)?;
        apply_loaded(&state, loaded, index, routes).await;
        state
            .events
            .emit(Event::observation_changed(stack, row.id, "started"))
            .await;
        Ok(Json(response(&state, &row).await?))
    })
    .await
    .map_err(internal)?
}

pub(super) async fn stop_handler(
    Extension(ctx): Extension<AuthContext>,
    Path((stack, id)): Path<(String, i64)>,
    State(state): State<AppState>,
) -> Result<Json<ObservationJson>, Response> {
    tokio::spawn(async move {
        require_scope(&ctx, Scope::ConfigWrite)?;
        require_scope(&ctx, Scope::SessionsRead)?;
        let _guard = state.config_lock.lock().await;
        let row = state
            .db
            .observations()
            .find(&stack, id)
            .await
            .map_err(internal)?
            .ok_or_else(|| rejected(StatusCode::NOT_FOUND, "Observation not found"))?;
        if row.ended_at.is_some() {
            return Ok(Json(response(&state, &row).await?));
        }
        let saved = saved_services(&state, &stack).await?;
        let (parsed, _, _) = ServicesToml::validate_new_services(&state.db, &stack, saved)
            .await
            .map_err(internal)?;
        let (mut loaded, index, routes) = ServicesToml::load(&state.db).await.map_err(internal)?;
        loaded.insert(stack.clone(), parsed);
        let row = state.db.observations().stop(&row).await.map_err(internal)?;
        apply_loaded(&state, loaded, index, routes).await;
        state
            .events
            .emit(Event::observation_changed(stack, id, "stopped"))
            .await;
        Ok(Json(response(&state, &row).await?))
    })
    .await
    .map_err(internal)?
}

pub(super) async fn apply_handler(
    Extension(ctx): Extension<AuthContext>,
    Path((stack, id)): Path<(String, i64)>,
    State(state): State<AppState>,
) -> Result<Json<ObservationJson>, Response> {
    tokio::spawn(async move {
    require_scope(&ctx, Scope::ConfigWrite)?;
    require_scope(&ctx, Scope::SessionsRead)?;
    let _guard = state.config_lock.lock().await;
    require_inactive(&state, &stack).await?;
    let row = state.db.observations().find(&stack, id).await.map_err(internal)?
        .ok_or_else(|| rejected(StatusCode::NOT_FOUND, "Observation not found"))?;
    if row.ended_at.is_none() { return Err(rejected(StatusCode::CONFLICT, "Stop observation before applying results")); }
    if row.applied { return Ok(Json(response(&state, &row).await?)); }
    let saved = saved_services(&state, &stack).await?;
    let snapshot: serde_json::Value = serde_json::from_str(&row.saved_config).map_err(internal)?;
    if serde_json::to_value(&saved).map_err(internal)? != snapshot {
        return Err(rejected(StatusCode::CONFLICT, "Configuration has changed since this observation; start a new observation before applying suggestions"));
    }
    let counts = state.db.observations().counts(&row).await.map_err(internal)?;
    let next = suggested(saved, &counts);
    let (parsed, _, _) = ServicesToml::validate_new_services(&state.db, &stack, next.clone()).await.map_err(internal)?;
    let (mut loaded, index, routes) = ServicesToml::load(&state.db).await.map_err(internal)?;
    loaded.insert(stack.clone(), parsed);
    state.db.observations().apply(&row, &next).await.map_err(internal)?;
    apply_loaded(&state, loaded, index, routes).await;
    state.events.emit(Event::observation_changed(stack, id, "applied")).await;
    Ok(Json(response(&state, &ObservationRow { applied: true, ..row }).await?))
    }).await.map_err(internal)?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Role;
    use crate::db::{Db, SessionGeo};
    use crate::orchestrator::Orchestrator;
    use crate::services::input::{services_to_inserts, stack_services};
    use std::sync::Arc;
    use tokio::sync::{Mutex, Notify, RwLock};

    fn admin() -> Extension<AuthContext> {
        Extension(AuthContext {
            user_id: "test".into(),
            role: Role::Admin,
            scopes: vec![],
        })
    }

    async fn state() -> AppState {
        let path =
            std::env::temp_dir().join(format!("nullnet-observation-{}.db", uuid::Uuid::new_v4()));
        let db = Db::open(path.to_str().unwrap()).await.unwrap();
        let services = stack_services(
            r#"
[[services]]
name = "web"
docker_container = "web"
port = 8001
timeout = 0
backends = ["old"]
proxy_dependencies = [["old"]]
[[services]]
name = "old"
docker_container = "old"
port = 8002
[[services]]
name = "new"
docker_container = "new"
port = 8003
"#,
        )
        .unwrap();
        db.stacks()
            .put_services("mesh", &services_to_inserts(&services))
            .await
            .unwrap();
        let (services, index, routes) = ServicesToml::load(&db).await.unwrap();
        let orchestrator = Orchestrator::new();
        AppState {
            config_lock: Arc::new(Mutex::new(())),
            services: Arc::new(RwLock::new(services)),
            routes: Arc::new(RwLock::new(routes)),
            match_index: Arc::new(RwLock::new(index)),
            events: orchestrator.events.clone(),
            sessions: orchestrator.sessions.clone(),
            orchestrator,
            db,
            config_changed: Arc::new(Notify::new()),
            port_mappings_changed: Arc::new(Notify::new()),
            http_routes_changed: Arc::new(Notify::new()),
        }
    }

    #[tokio::test]
    async fn lifecycle_keeps_saved_config_and_proxy_deps_and_applies_only_observed_backends() {
        let state = state().await;
        let baseline = ServicesToml::export_toml(&state.db, "mesh").await.unwrap();
        let Json(started) = start_handler(admin(), Path("mesh".into()), State(state.clone()))
            .await
            .unwrap();
        assert_eq!(
            state.services.read().await["mesh"]
                .values()
                .map(|s| s.triggers().len())
                .sum::<usize>(),
            6
        );
        assert_eq!(
            state.services.read().await["mesh"]["web"].proxy_deps(),
            &[vec!["old".to_string()]]
        );
        assert_eq!(
            ServicesToml::export_toml(&state.db, "mesh").await.unwrap(),
            baseline
        );
        let (restart, _, _) = ServicesToml::load(&state.db).await.unwrap();
        assert_eq!(restart["mesh"]["web"].triggers().len(), 2);
        assert_eq!(
            require_inactive(&state, "mesh").await.unwrap_err().status(),
            StatusCode::CONFLICT
        );
        assert!(require_inactive(&state, "unrelated").await.is_ok());

        for i in 0..503 {
            state
                .db
                .sessions()
                .open(
                    "backend",
                    "mesh",
                    "web",
                    i,
                    "new",
                    &SessionGeo::default(),
                    false,
                    "{}",
                    started.started_at,
                )
                .await
                .unwrap();
        }
        state
            .db
            .sessions()
            .open(
                "ingress",
                "mesh",
                "web",
                999,
                "old",
                &SessionGeo::default(),
                false,
                "{}",
                started.started_at,
            )
            .await
            .unwrap();
        state
            .db
            .sessions()
            .close_all_open(started.started_at + 1)
            .await
            .unwrap();
        state
            .db
            .sessions()
            .delete_ended_before(started.started_at + 2)
            .await
            .unwrap();
        let Json(running) = list_handler(admin(), Path("mesh".into()), State(state.clone()))
            .await
            .unwrap();
        assert_eq!(running.observations[0].counts[0].count, 503);
        let Json(stopped) = stop_handler(
            admin(),
            Path(("mesh".into(), started.id)),
            State(state.clone()),
        )
        .await
        .unwrap();
        assert_eq!(
            stopped.counts,
            vec![ObservedEdge {
                source: "web".into(),
                destination: "new".into(),
                count: 503
            }]
        );
        assert_eq!(stopped.added[0].destination, "new");
        assert_eq!(stopped.removed[0].destination, "old");
        assert_eq!(
            state.services.read().await["mesh"]["web"].triggers().len(),
            1
        );
        assert_eq!(
            ServicesToml::export_toml(&state.db, "mesh").await.unwrap(),
            baseline
        );
        state
            .db
            .sessions()
            .close_all_open(started.started_at + 1)
            .await
            .unwrap();
        state
            .db
            .sessions()
            .delete_ended_before(started.started_at + 2)
            .await
            .unwrap();
        let Json(history) = list_handler(admin(), Path("mesh".into()), State(state.clone()))
            .await
            .unwrap();
        assert_eq!(history.observations[0].counts[0].count, 503);
        let Json(applied) = apply_handler(
            admin(),
            Path(("mesh".into(), started.id)),
            State(state.clone()),
        )
        .await
        .unwrap();
        assert!(applied.applied);
        let saved = saved_services(&state, "mesh").await.unwrap();
        assert_eq!(
            saved.iter().find(|s| s.name == "web").unwrap().backends,
            vec!["new"]
        );
        assert_eq!(
            state.services.read().await["mesh"]["web"].proxy_deps(),
            &[vec!["old".to_string()]]
        );
        assert_eq!(
            state.services.read().await["mesh"]["web"].triggers()[&8003],
            "new"
        );
    }

    #[tokio::test]
    async fn conflicting_destinations_cannot_activate_and_leave_no_observation() {
        let state = state().await;
        let mut services = saved_services(&state, "mesh").await.unwrap();
        let mut json = serde_json::to_value(&services).unwrap();
        let peer = json
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|s| s["name"] == "new")
            .unwrap();
        peer["port"] = serde_json::json!(8002);
        services = serde_json::from_value(json).unwrap();
        state
            .db
            .stacks()
            .put_services("mesh", &services_to_inserts(&services))
            .await
            .unwrap();
        let result = start_handler(admin(), Path("mesh".into()), State(state.clone())).await;
        assert_eq!(
            result.err().unwrap().status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert!(
            state
                .db
                .observations()
                .list("mesh")
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn concurrent_starts_are_serialized_and_stale_results_cannot_overwrite_edits() {
        let state = state().await;
        let (a, b) = tokio::join!(
            start_handler(admin(), Path("mesh".into()), State(state.clone())),
            start_handler(admin(), Path("mesh".into()), State(state.clone()))
        );
        assert_ne!(a.is_ok(), b.is_ok());
        let Json(started) = a.or(b).unwrap();
        stop_handler(
            admin(),
            Path(("mesh".into(), started.id)),
            State(state.clone()),
        )
        .await
        .unwrap();
        let mut services = saved_services(&state, "mesh").await.unwrap();
        services
            .iter_mut()
            .find(|s| s.name == "web")
            .unwrap()
            .backends
            .clear();
        state
            .db
            .stacks()
            .put_services("mesh", &services_to_inserts(&services))
            .await
            .unwrap();
        let error = apply_handler(
            admin(),
            Path(("mesh".into(), started.id)),
            State(state.clone()),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(error.status(), StatusCode::CONFLICT);
        assert!(
            !state
                .db
                .observations()
                .find("mesh", started.id)
                .await
                .unwrap()
                .unwrap()
                .applied
        );
    }

    #[tokio::test]
    async fn counters_seed_open_sessions_survive_retention_and_freeze_on_stop() {
        let state = state().await;
        let repo = state.db.sessions();
        let geo = SessionGeo::default();
        repo.open("backend", "mesh", "web", 1, "old", &geo, false, "{}", 1)
            .await
            .unwrap();
        let ended = repo
            .open("backend", "mesh", "web", 2, "new", &geo, false, "{}", 1)
            .await
            .unwrap();
        repo.close_backend(ended, 2).await.unwrap();
        repo.open("backend", "another", "web", 3, "new", &geo, false, "{}", 1)
            .await
            .unwrap();
        repo.open("egress", "mesh", "web", 4, "1.1.1.1", &geo, false, "{}", 1)
            .await
            .unwrap();
        let Json(started) = start_handler(admin(), Path("mesh".into()), State(state.clone()))
            .await
            .unwrap();
        assert_eq!(
            started.counts,
            vec![ObservedEdge {
                source: "web".into(),
                destination: "old".into(),
                count: 1
            }]
        );
        repo.close_all_open(3).await.unwrap();
        repo.delete_ended_before(4).await.unwrap();
        let Json(stopped) = stop_handler(
            admin(),
            Path(("mesh".into(), started.id)),
            State(state.clone()),
        )
        .await
        .unwrap();
        assert_eq!(stopped.counts, started.counts);
        repo.open(
            "backend",
            "mesh",
            "web",
            5,
            "new",
            &geo,
            false,
            "{}",
            started.started_at,
        )
        .await
        .unwrap();
        let Json(next) = start_handler(admin(), Path("mesh".into()), State(state.clone()))
            .await
            .unwrap();
        assert_eq!(
            next.counts,
            vec![ObservedEdge {
                source: "web".into(),
                destination: "new".into(),
                count: 1
            }]
        );
        let Json(history) = list_handler(admin(), Path("mesh".into()), State(state.clone()))
            .await
            .unwrap();
        assert_eq!(history.observations[1].counts, stopped.counts);
    }

    #[tokio::test]
    async fn active_observation_rejects_every_config_write_path_and_unauthorized_start() {
        let state = state().await;
        let reader = Extension(AuthContext {
            user_id: "reader".into(),
            role: Role::User,
            scopes: vec!["config:read".into(), "sessions:read".into()],
        });
        assert_eq!(
            start_handler(reader, Path("mesh".into()), State(state.clone()))
                .await
                .err()
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        start_handler(admin(), Path("mesh".into()), State(state.clone()))
            .await
            .unwrap();
        let body = serde_json::from_value(
            serde_json::json!({"services": saved_services(&state, "mesh").await.unwrap()}),
        )
        .unwrap();
        let save = super::super::service_config::save_handler(
            admin(),
            Path("mesh".into()),
            State(state.clone()),
            Json(body),
        )
        .await;
        let import = super::super::service_config::import_handler(
            admin(),
            Path("mesh".into()),
            State(state.clone()),
            "invalid toml".into(),
        )
        .await;
        let delete = super::super::service_config::delete_handler(
            admin(),
            Path("mesh".into()),
            State(state.clone()),
        )
        .await;
        let routes = super::super::routes::save_handler(
            admin(),
            Path("mesh".into()),
            State(state.clone()),
            Json(vec![]),
        )
        .await;
        for result in [save, import, delete, routes] {
            assert_eq!(result.status(), StatusCode::CONFLICT);
        }
        assert!(
            state
                .db
                .observations()
                .active("mesh")
                .await
                .unwrap()
                .is_some()
        );
    }
}
