use openab_control_plane::store::{now_ms, SqliteStore, Store};
use openab_control_plane::{
    build_router, identity, ops::seed_roster, orchestrator, state::AppState,
};
use std::sync::Arc;
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let db = std::env::var("OABCP_DB").unwrap_or_else(|_| "plane.db".into());
    let addr = std::env::var("OABCP_ADDR").unwrap_or_else(|_| "0.0.0.0:8090".into());

    // ponytail: SQLite default — the simple path that works out of the box.
    // Swap this one line for a networked Store impl when scale needs it (§6c).
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open(&db)?);
    identity::resolve_externalize_default(store.as_ref())?;
    seed_roster(store.as_ref())?;
    store.purge_terminal_outbox()?;
    tracing::info!("terminal/null outbox backstop sweep completed");
    let state = AppState::new(store);
    spawn_watchdog(state.clone());
    spawn_liveness(state.clone());
    spawn_review_catchup(state.clone());
    openab_control_plane::controller_events::spawn_dispatcher(state.clone());
    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("control plane listening on {addr} (db={db})");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

/// Liveness watchdog: periodically force-close sessions stuck past the deadline,
/// so a silent/dead reviewer can't hang a council forever (the one guarantee
/// prose can't make — see design "what OCP actually guarantees").
/// ponytail: deadline is anchored on `created_at`, no last-activity reset — bump
/// `OABCP_SESSION_TIMEOUT_SECS` or add activity tracking if long councils are
/// legitimate. Default 600s (10 min); scan every 30s.
fn spawn_watchdog(state: Arc<AppState>) {
    let timeout_secs: i64 = std::env::var("OABCP_SESSION_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(600);
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(30));
        loop {
            tick.tick().await;
            let cutoff = now_ms() - timeout_secs * 1000;
            match state.store.active_sessions_before(cutoff) {
                Ok(ids) => {
                    for id in ids {
                        if let Err(e) = orchestrator::force_close_timeout(&state, &id) {
                            tracing::error!("watchdog close {id} failed: {e}");
                        }
                    }
                }
                Err(e) => tracing::error!("watchdog scan failed: {e}"),
            }
        }
    });
}

/// SEI-819 cap catch-up: convene reviews the hourly cap dropped once the
/// window clears. 60s tick — the wait is up to an hour, sub-minute precision
/// buys nothing. `OABCP_REVIEW_CATCHUP_SECS=0` disables.
fn spawn_review_catchup(state: Arc<AppState>) {
    let tick_secs: u64 = std::env::var("OABCP_REVIEW_CATCHUP_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);
    if tick_secs == 0 {
        tracing::info!("review cap catch-up disabled (OABCP_REVIEW_CATCHUP_SECS=0)");
        return;
    }
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(tick_secs));
        loop {
            tick.tick().await;
            if let Err(e) =
                openab_control_plane::plugins::pr_review::council::sweep_pending_reviews(&state)
                    .await
            {
                tracing::error!("review catch-up sweep failed: {e}");
            }
        }
    });
}

/// Liveness policy sweep (A3): disconnected roster member past the grace window
/// → health flip → replace from inventory, or trim + shrink quorum
/// (`orchestrator::sweep_liveness`). Grace must exceed the OAB reconnect backoff
/// (1–30s) so a plane or pod bounce isn't misread as death. Default 60s;
/// `OABCP_LIVENESS_GRACE_SECS=0` disables the sweep.
fn spawn_liveness(state: Arc<AppState>) {
    let grace_secs: i64 = std::env::var("OABCP_LIVENESS_GRACE_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);
    if grace_secs <= 0 {
        tracing::info!("liveness sweep disabled (OABCP_LIVENESS_GRACE_SECS=0)");
        return;
    }
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(30));
        loop {
            tick.tick().await;
            if let Err(e) = orchestrator::sweep_liveness(&state, grace_secs * 1000) {
                tracing::error!("liveness sweep failed: {e}");
            }
        }
    });
}

/// Factor IX disposability: drain on SIGTERM/Ctrl-C. Bots reconnect (1–30s
/// backoff) once the plane is back; committed state is in the store.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = term => {} }
    tracing::info!("shutdown signal received, draining");
}
