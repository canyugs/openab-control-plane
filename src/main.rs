use controller_protocol::audit::{AuditCorrelation, AuditEvent, AuditOutcome, AUDIT_EVENT_VERSION};
use openab_control_plane::store::{now_ms, Store};
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
    // A postgres:// OABCP_DB selects the networked Store impl (ADR 033 §6c).
    let store: Arc<dyn Store> = openab_control_plane::store::open_store(&db)?;
    identity::resolve_externalize_default(store.as_ref())?;
    seed_roster(store.as_ref())?;
    store.purge_terminal_outbox()?;
    tracing::info!("terminal/null outbox backstop sweep completed");
    let state = AppState::new(store);
    spawn_watchdog(state.clone());
    spawn_liveness(state.clone());
    spawn_audit_retention(state.clone());
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
///
/// The deadline is anchored on the session's last message, so "stuck" means the
/// **session** produced nothing — not that a particular bot went quiet. A dead
/// reviewer whose peers keep posting does not trip this; detecting one absent
/// member is `sweep_liveness`'s job (trim/replace), and the watchdog is only the
/// whole-session backstop behind it. The trade this anchor buys: a long-lived
/// `solo` ticket session, reopened by every staff follow-up, is no longer cut
/// off mid-turn merely for being old. The cost: a session that chatters forever
/// without converging is never force-closed, where a creation anchor would have
/// capped it — accepted because no such session has been observed and the old
/// anchor broke a live one every day.
///
/// Default 600s (10 min) of silence; scan every 30s. Bump
/// `OABCP_SESSION_TIMEOUT_SECS` for slower bots.
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
                        if let Err(e) = orchestrator::force_close_timeout(&state, &id, cutoff) {
                            tracing::error!("watchdog close {id} failed: {e}");
                        }
                    }
                }
                Err(e) => tracing::error!("watchdog scan failed: {e}"),
            }
        }
    });
}

/// Keep investigation history independently from domain retention. Ordinary
/// events use the configurable 90-day window; failures, security/configuration
/// facts, dead letters, and uncertain/reconciled external effects use the
/// configurable extended window. The aggregate is written after the delete so
/// the sweep itself remains visible outside the removed range.
fn spawn_audit_retention(state: Arc<AppState>) {
    let retention_days = configured_positive_days(
        "OABCP_AUDIT_RETENTION_DAYS",
        controller_protocol::audit::DEFAULT_RETENTION_DAYS,
    );
    let extended_days = configured_positive_days(
        "OABCP_AUDIT_EXTENDED_RETENTION_DAYS",
        controller_protocol::audit::EXTENDED_RETENTION_DAYS,
    )
    .max(retention_days);
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(60 * 60));
        loop {
            tick.tick().await;
            let now = now_ms();
            let before = now.saturating_sub(retention_days.saturating_mul(86_400_000));
            let extended_before = now.saturating_sub(extended_days.saturating_mul(86_400_000));
            match state.store.prune_audit_events(before, extended_before) {
                Ok(pruned) if pruned > 0 => {
                    let event_key = format!("audit.retention_pruned:{now}:{pruned}");
                    let event = AuditEvent {
                        version: AUDIT_EVENT_VERSION,
                        event_id: format!("aud:openab-control-plane:{event_key}"),
                        event_key,
                        occurred_at: now,
                        recorded_at: now,
                        service: "openab-control-plane".into(),
                        kind: "audit.retention_pruned".into(),
                        outcome: AuditOutcome::Succeeded,
                        caused_by: None,
                        correlation: AuditCorrelation::default(),
                        actor: None,
                        target: None,
                        detail: serde_json::json!({
                            "pruned": pruned,
                            "retention_days": retention_days,
                            "extended_retention_days": extended_days,
                            "before": before,
                            "extended_before": extended_before,
                        }),
                        error: None,
                    };
                    if let Err(error) = state.store.append_audit_event(&event) {
                        tracing::warn!(%error, pruned, "audit retention evidence append failed");
                    } else {
                        tracing::info!(pruned, "pruned expired audit events");
                    }
                }
                Ok(_) => {}
                Err(error) => tracing::warn!(%error, "audit retention sweep failed"),
            }
        }
    });
}

fn configured_positive_days(name: &str, default: i64) -> i64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|days: &i64| *days > 0)
        .unwrap_or(default)
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
