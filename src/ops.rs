use crate::{identity, store::Store};

/// Register the initial bot roster from `OABCP_BOTS` so pods can connect with no
/// manual `POST /v1/bots`. Format: `name:role,name:role` (role defaults to
/// `reviewer`). First-boot only: once the registry has any bot, membership
/// changes go through the API instead of resurrecting deleted env-seeded rows.
pub fn seed_roster(store: &dyn Store) -> anyhow::Result<()> {
    let Ok(spec) = std::env::var("OABCP_BOTS") else {
        return Ok(());
    };
    if !store.list_bots()?.is_empty() {
        tracing::info!("bots table non-empty; OABCP_BOTS ignored (first-boot seeding only)");
        return Ok(());
    }
    for entry in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let (name, role) = entry.split_once(':').unwrap_or((entry, "reviewer"));
        let (name, role) = (name.trim(), role.trim());
        if identity::seed(store, name, role)? {
            tracing::info!(bot = name, role, "seeded from OABCP_BOTS");
        }
    }
    Ok(())
}
