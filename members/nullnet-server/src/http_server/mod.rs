use crate::events::EventStore;
use crate::orchestrator::Orchestrator;
use crate::services::input::StackMap;
use axum::Router;
use axum::routing::{delete, get};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use tokio::sync::RwLock;

mod config;
mod events;
mod events_stream;
mod graph;
mod health;
mod nodes;
mod pool;
mod services;
mod sessions;
mod static_files;

const HTTP_PORT: u16 = 8080;

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) services: Arc<RwLock<StackMap>>,
    pub(crate) orchestrator: Orchestrator,
    pub(crate) events: EventStore,
}

pub async fn serve(state: AppState) {
    let app = Router::new()
        .route("/api/health", get(health::health))
        .route("/api/services/{stack}", get(services::services_handler))
        .route("/api/nodes", get(nodes::nodes_handler))
        .route("/api/pool", get(pool::pool_handler))
        .route("/api/config/{stack}", get(config::config_handler))
        .route("/api/graph/{stack}", get(graph::graph_handler))
        .route("/api/sessions", get(sessions::list_handler))
        .route("/api/sessions/{id}", delete(sessions::teardown_handler))
        .route("/api/events", get(events::events_handler))
        .route(
            "/api/events/stream",
            get(events_stream::events_stream_handler),
        )
        .fallback(get(static_files::static_handler))
        .with_state(state);

    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), HTTP_PORT);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind HTTP listener");
    axum::serve(listener, app).await.expect("HTTP server error");
}
