use openab_control_plane::build_router;
use openab_control_plane::identity::hash_token;
use openab_control_plane::state::AppState;
use openab_control_plane::store::{SqliteStore, Store};
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::net::TcpListener;

const FIXED_TOKEN: &str = "oabct_fixedfixedfixedfixedfixedfix";
const FIXED_WS_URL: &str = "ws://control-plane.test/ws";
const GOLDEN_DIR: &str = "tests/golden/bot_config";
const REGEN_CMD: &str = "OABCP_REGEN_GOLDEN=1 cargo test --test bot_config_freeze";

#[derive(Clone, Copy)]
struct Combo {
    role: &'static str,
    agent: &'static str,
    token_mode: TokenMode,
}

#[derive(Clone, Copy)]
enum TokenMode {
    Legacy,
    Externalized,
}

impl TokenMode {
    fn as_str(self) -> &'static str {
        match self {
            TokenMode::Legacy => "legacy",
            TokenMode::Externalized => "externalized",
        }
    }
}

#[tokio::test]
async fn bot_config_response_body_matches_golden_snapshots() {
    // Keep all env mutation in one test body so this binary cannot race itself.
    std::env::set_var("OABCP_WS_URL", FIXED_WS_URL);
    std::env::remove_var("OABCP_AGENT_COMMAND");
    std::env::remove_var("OABCP_AGENT_INHERIT_ENV");
    std::env::remove_var("OABCP_AGENT_PROFILES");
    std::env::remove_var("OABCP_AGENT_WORKING_DIR");

    let regen = std::env::var("OABCP_REGEN_GOLDEN")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    for role in ["chair", "reviewer"] {
        for agent in ["claude", "codex", "kiro"] {
            for token_mode in [TokenMode::Legacy, TokenMode::Externalized] {
                let combo = Combo {
                    role,
                    agent,
                    token_mode,
                };
                let body = render_bot_config(combo).await;
                assert_or_regen_golden(combo, &body, regen);
            }
        }
    }
}

async fn render_bot_config(combo: Combo) -> Vec<u8> {
    match combo.token_mode {
        // S15 flipped the unset default to externalized; pin legacy explicitly so
        // this combo keeps rendering the legacy (plaintext-token) golden.
        TokenMode::Legacy => std::env::set_var("OABCP_EXTERNALIZE_TOKENS", "0"),
        TokenMode::Externalized => std::env::set_var("OABCP_EXTERNALIZE_TOKENS", "1"),
    }

    let store: Arc<dyn Store> = Arc::new(SqliteStore::memory().unwrap());
    let plaintext = match combo.token_mode {
        TokenMode::Legacy => FIXED_TOKEN,
        TokenMode::Externalized => "",
    };
    store
        .seed_bot(
            combo.role,
            combo.role,
            combo.role,
            &hash_token(FIXED_TOKEN),
            plaintext,
        )
        .unwrap();
    let state = AppState::new_with_options(
        store,
        None,
        None,
        None,
        None,
        "http://control-plane.test".to_string(),
        None,
    );
    let addr = spawn_server(state).await;
    let response = reqwest::get(format!(
        "http://{addr}/bot-config/{}?agent={}",
        combo.role, combo.agent
    ))
    .await
    .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    response.bytes().await.unwrap().to_vec()
}

async fn spawn_server(state: Arc<AppState>) -> SocketAddr {
    let app = build_router(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

fn assert_or_regen_golden(combo: Combo, actual: &[u8], regen: bool) {
    let path = golden_path(combo);
    if regen {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, actual).unwrap();
        return;
    }

    let expected = fs::read(&path).unwrap_or_else(|err| {
        panic!(
            "failed to read golden file {}: {err}\n/bot-config render is frozen per ADR 010 B2; regenerate deliberately with: {REGEN_CMD}",
            display_path(&path)
        )
    });
    assert_eq!(
        expected,
        actual,
        "golden file {} does not match\n/bot-config render is frozen per ADR 010 B2; regenerate deliberately with: {REGEN_CMD}",
        display_path(&path)
    );
}

fn golden_path(combo: Combo) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(GOLDEN_DIR)
        .join(format!(
            "{}-{}-{}.toml",
            combo.role,
            combo.agent,
            combo.token_mode.as_str()
        ))
}

fn display_path(path: &Path) -> String {
    path.strip_prefix(env!("CARGO_MANIFEST_DIR"))
        .unwrap_or(path)
        .display()
        .to_string()
}
