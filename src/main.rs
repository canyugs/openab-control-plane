use openab_control_plane::{build_router, state::AppState, store::Store};
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

    let store = Arc::new(Store::open(&db)?);
    let state = AppState::new(store);
    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("control plane listening on {addr} (db={db})");
    axum::serve(listener, app).await?;
    Ok(())
}
