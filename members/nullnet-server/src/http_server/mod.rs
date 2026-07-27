use crate::db::Db;
use crate::events::EventStore;
use crate::orchestrator::Orchestrator;
use crate::services::input::StackMap;
use axum::Router;
use axum::routing::{delete, get, patch, post};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use tokio::sync::RwLock;

mod auth;
mod certificates;
mod chains;
mod config;
mod events;
mod events_stream;
mod graph;
mod health;
mod nodes;
mod services;
mod sessions;
mod stacks;
mod static_files;

const HTTP_PORT: u16 = 8080;

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) services: Arc<RwLock<StackMap>>,
    pub(crate) orchestrator: Orchestrator,
    pub(crate) events: EventStore,
    pub(crate) db: Db,
}

pub async fn serve(state: AppState) {
    // Guarded by `auth::require_auth`: every route here needs a valid
    // access-token cookie. Individual handlers additionally check their own
    // required scope (see `http_server::auth::require_scope`).
    let protected = Router::new()
        .route("/api/stacks", get(stacks::stacks_handler))
        .route("/api/services/{stack}", get(services::services_handler))
        .route("/api/nodes/{stack}", get(nodes::nodes_handler))
        .route(
            "/api/config/{stack}",
            get(config::config_handler)
                .post(config::save_handler)
                .delete(config::delete_handler),
        )
        .route("/api/graph/{stack}", get(graph::graph_handler))
        .route("/api/sessions/{stack}", get(sessions::list_handler))
        .route(
            "/api/sessions/{stack}/{id}",
            delete(sessions::teardown_handler),
        )
        .route("/api/chains/{stack}", get(chains::chains_handler))
        .route("/api/certificates", get(certificates::list_handler))
        .route(
            "/api/certificates/request",
            post(certificates::request_handler),
        )
        .route(
            "/api/certificates/{domain}",
            delete(certificates::delete_handler),
        )
        .route("/api/events", get(events::events_handler))
        .route(
            "/api/events/stream",
            get(events_stream::events_stream_handler),
        )
        .route("/api/auth/logout", post(auth::logout_handler))
        .route("/api/auth/me", get(auth::me_handler))
        .route("/api/auth/mfa/setup", post(auth::setup_handler))
        .route("/api/auth/mfa/confirm", post(auth::confirm_handler))
        .route("/api/auth/mfa/disable", post(auth::disable_handler))
        .route(
            "/api/auth/users",
            get(auth::list_handler).post(auth::create_handler),
        )
        .route(
            "/api/auth/users/{id}",
            patch(auth::update_handler).delete(auth::delete_handler),
        )
        .route_layer(axum::middleware::from_fn(auth::require_auth));

    // No auth required — login/refresh have no session yet by definition,
    // health is a plain liveness check, and the SPA fallback must stay open
    // so the browser can load the login page itself.
    let public = Router::new()
        .route("/api/health", get(health::health))
        .route("/api/auth/login", post(auth::login_handler))
        .route("/api/auth/mfa/verify", post(auth::mfa_verify_handler))
        .route("/api/auth/refresh", post(auth::refresh_handler))
        .fallback(get(static_files::static_handler));

    let app = protected.merge(public).with_state(state);

    // Self-signed cert, regenerated each start. The admin UI is single-origin, so
    // relative /api calls inherit HTTPS; browsers prompt to trust the cert once.
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
        .expect("failed to generate self-signed certificate");
    let config = axum_server::tls_rustls::RustlsConfig::from_pem(
        cert.cert.pem().into_bytes(),
        cert.signing_key.serialize_pem().into_bytes(),
    )
    .await
    .expect("failed to build TLS config");

    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), HTTP_PORT);
    axum_server::bind_rustls(addr, config)
        .serve(app.into_make_service())
        .await
        .expect("HTTPS server error");
}
