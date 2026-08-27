//! openab-control-plane — a gateway-native conversation control plane.
//! See docs/control-plane-design.md (in the ops repo).

pub mod api;
pub mod controller;
pub mod controller_api;
pub mod controller_events;
pub mod coordinator;
pub mod identity;
pub mod ops;
pub mod orchestrator;
pub mod plugins;
pub mod protocol;
pub mod routing;
pub mod session;
pub mod state;
pub mod store;
pub mod turn_failure;
pub mod ws;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::Router;
use state::AppState;
use std::sync::Arc;

/// Seconds the store heartbeat may go stale before `/readyz` fails (and, in
/// prod, the Zeabur health check restarts the pod). Default 90 — three missed
/// 30s watchdog ticks, well clear of any single slow turn. Tunable via
/// `OABCP_READYZ_STALL_SECS` (the physical-world calibration knob).
pub fn readyz_stall_secs() -> u64 {
    std::env::var("OABCP_READYZ_STALL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(90)
}

/// Deep readiness (SEI-962): 503 when the store heartbeat is stale, i.e. the
/// background watchdog can no longer complete a store read — the wedge the
/// static `/healthz` cannot see. Store-free, so it answers during a wedge.
async fn readyz(State(state): State<Arc<AppState>>) -> (StatusCode, axum::Json<serde_json::Value>) {
    let stalled = state.store_stalled_secs();
    let threshold = readyz_stall_secs();
    let ready = stalled <= threshold;
    let code = if ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        code,
        axum::Json(serde_json::json!({
            "status": if ready { "ready" } else { "stalled" },
            "store_last_ok_secs_ago": stalled,
            "threshold_secs": threshold,
        })),
    )
}

/// Full router: north REST/SSE + south `/ws` + liveness/readiness probe.
pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/ws", get(ws::ws_handler))
        .route("/healthz", get(|| async { "ok" }))
        .route("/readyz", get(readyz))
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
