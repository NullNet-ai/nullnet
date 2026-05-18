use crate::orchestrator::Orchestrator;
use crate::services::service_info::ServiceInfo;
use axum::Router;
use axum::routing::get;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use tokio::sync::RwLock;

mod config;
mod graph;
mod health;
mod nodes;
mod pool;
mod services;
mod static_files;

const HTTP_PORT: u16 = 8080;

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) services: Arc<RwLock<HashMap<String, ServiceInfo>>>,
    pub(crate) orchestrator: Orchestrator,
}

pub async fn serve(state: AppState) {
    let app = Router::new()
        .route("/api/health", get(health::health))
        .route("/api/services", get(services::services_handler))
        .route("/api/nodes", get(nodes::nodes_handler))
        .route("/api/pool", get(pool::pool_handler))
        .route("/api/config", get(config::config_handler))
        .route("/api/graph", get(graph::graph_handler))
        .fallback(get(static_files::static_handler))
        .with_state(state);

    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), HTTP_PORT);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind HTTP listener");
    axum::serve(listener, app).await.expect("HTTP server error");
}
