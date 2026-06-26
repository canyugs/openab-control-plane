use openab_control_plane::store::{SqliteStore, Store};
use openab_control_plane::{build_router, identity, state::AppState};
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let db = std::env::var("OABCP_DB").unwrap_or_else(|_| "plane.db".into());
    let addr = std::env::var("OABCP_ADDR").unwrap_or_else(|_| "0.0.0.0:8090".into());

    // ponytail: SQLite default — the simple path that works out of the box.
    // Swap this one line for a networked Store impl when scale needs it (§6c).
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open(&db)?);
    seed_roster(store.as_ref())?;
    let state = AppState::new(store);
    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("control plane listening on {addr} (db={db})");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

/// Register the initial bot roster from `OABCP_BOTS` so pods can connect with no
/// manual `POST /v1/bots`. Format: `name:role,name:role` (role defaults to
/// `reviewer`). Idempotent — restarts and existing bots are skipped.
fn seed_roster(store: &dyn Store) -> anyhow::Result<()> {
    let Ok(spec) = std::env::var("OABCP_BOTS") else { return Ok(()) };
    for entry in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let (name, role) = entry.split_once(':').unwrap_or((entry, "reviewer"));
        let (name, role) = (name.trim(), role.trim());
        if identity::seed(store, name, role)? {
            tracing::info!(bot = name, role, "seeded from OABCP_BOTS");
        }
    }
    Ok(())
}

/// Factor IX disposability: drain on SIGTERM/Ctrl-C. Bots reconnect (1–30s
/// backoff) once the plane is back; committed state is in the store.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = term => {} }
    tracing::info!("shutdown signal received, draining");
}
