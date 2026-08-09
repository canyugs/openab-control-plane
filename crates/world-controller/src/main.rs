use anyhow::Context;
use std::sync::Arc;
use std::time::Duration;
use tracing_subscriber::EnvFilter;
use world_controller::{config::Config, router, spawn_scheduler_tick, AppState};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config = Config::from_env();
    let addr = config.addr.clone();
    let state = Arc::new(AppState::from_config(config).context("world controller config")?);
    spawn_scheduler_tick(state.clone(), Duration::from_secs(30));
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("bind world controller to {addr}"))?;
    tracing::info!(%addr, "world controller listening");
    axum::serve(listener, router(state))
        .await
        .context("serve world controller")
}
