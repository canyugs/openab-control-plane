//! openab-control-plane — a gateway-native conversation control plane.
//! See docs/control-plane-design.md (in the ops repo).

pub mod api;
pub mod controller;
pub mod coordinator;
pub mod github_app;
pub mod identity;
pub mod ops;
pub mod orchestrator;
pub mod plugins;
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
        // Build identity — lets ops confirm which control-plane build is live
        // without shelling into the pod. git_sha is "unknown" unless the image
        // build passes GIT_SHA at compile time. (SEI-787)
        .route(
            "/version",
            get(|| async {
                axum::Json(serde_json::json!({
                    "name": env!("CARGO_PKG_NAME"),
                    "version": env!("CARGO_PKG_VERSION"),
                    "git_sha": option_env!("GIT_SHA").unwrap_or("unknown"),
                }))
            }),
        )
        .merge(api::router())
        .with_state(state)
}
