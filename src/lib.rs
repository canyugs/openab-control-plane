//! openab-control-plane — a gateway-native conversation control plane.
//! See docs/control-plane-design.md (in the ops repo).

pub mod api;
pub mod controller;
pub mod coordinator;
pub mod council;
pub mod github_app;
pub mod github_webhook;
pub mod identity;
pub mod ops;
pub mod orchestrator;
pub mod protocol;
pub mod routing;
pub mod session;
pub mod state;
pub mod store;
pub mod ws;

use axum::routing::get;
use axum::Router;
use state::AppState;
use std::sync::Arc;

/// Full router: north REST/SSE + south `/ws` + liveness/readiness probe.
pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/ws", get(ws::ws_handler))
        .route("/healthz", get(|| async { "ok" }))
        .merge(api::router())
        .with_state(state)
}
