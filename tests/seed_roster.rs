use openab_control_plane::store::{SqliteStore, Store};
use openab_control_plane::{identity, ops::seed_roster};

struct EnvGuard {
    key: &'static str,
    old: Option<String>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let old = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, old }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(value) = &self.old {
            std::env::set_var(self.key, value);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

#[test]
fn seed_roster_is_first_boot_only() {
    let _bots = EnvGuard::set("OABCP_BOTS", "existing:reviewer,newbie:reviewer");
    let store = SqliteStore::memory().unwrap();
    identity::seed(&store, "existing", "reviewer").unwrap();

    seed_roster(&store).unwrap();

    assert!(store.bot("existing").unwrap().is_some());
    assert!(store.bot("newbie").unwrap().is_none());
}
