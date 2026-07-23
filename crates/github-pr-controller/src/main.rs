use anyhow::Context;
use github_pr_controller::{config::Config, router, AppState};
use std::sync::Arc;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config = Config::from_env();
    let addr = config.addr.clone();
    let state = Arc::new(AppState::from_config(config));
    if let Some(error) = state.store_error.as_deref() {
        tracing::error!(%error, "controller product store unavailable; starting not-ready");
    }
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("bind GitHub controller to {addr}"))?;
    tracing::info!(%addr, mode = "plan_only", "GitHub PR controller listening");
    axum::serve(listener, router(state))
        .await
        .context("serve GitHub PR controller")
}
