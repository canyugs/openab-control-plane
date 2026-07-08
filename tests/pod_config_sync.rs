//! Guards the pod-owned config channel (ADR 010 B2 demotion execution).
//!
//! The Zeabur templates mount `docs/pod-config/pod-config-{chair,reviewer}.toml`
//! into every bot pod at `/etc/openab/config.toml` (the upstream image-CMD
//! convention), replacing the `/bot-config` HTTP fetch. The TOML is pasted into
//! each template as a YAML block scalar (chair anchored once, reviewer anchored
//! once + aliased once), so it can silently drift from the source files — these
//! tests make that drift a hard failure, and pin the two invariants the design
//! rests on: the config carries no secret and names no agent.

const REVIEWER: &str = include_str!("../docs/pod-config/pod-config-reviewer.toml");
const CHAIR: &str = include_str!("../docs/pod-config/pod-config-chair.toml");
const APP_TEMPLATE: &str = include_str!("../zeabur-template-app-1E1Y97.yaml");
const PAT_TEMPLATE: &str = include_str!("../zeabur-template-pat-Z7TQIR.yaml");
const BOT_CONFIG_GOLDEN: &str =
    include_str!("golden/bot_config/chair-claude-externalized.toml");

/// The doc as it appears inside the templates' `template: |` block scalar
/// (14-space indent, blank lines stay empty). Same convention as
/// tests/steering_sync.rs.
fn as_block_scalar(doc: &str) -> String {
    doc.lines()
        .map(|l| {
            if l.is_empty() {
                String::new()
            } else {
                format!("              {l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn templates_carry_current_pod_configs() {
    for (name, tmpl) in [("app", APP_TEMPLATE), ("pat", PAT_TEMPLATE)] {
        for (role, doc) in [("chair", CHAIR), ("reviewer", REVIEWER)] {
            assert!(
                tmpl.contains(&as_block_scalar(doc)),
                "zeabur-template-{name}: the /etc/openab/config.toml block drifted \
                 from docs/pod-config/pod-config-{role}.toml — edit the doc, then \
                 re-paste it into the template's anchored block"
            );
        }
    }
}

#[test]
fn pod_config_is_anchored_once_and_reviewer_aliased() {
    for (name, tmpl) in [("app", APP_TEMPLATE), ("pat", PAT_TEMPLATE)] {
        assert_eq!(
            tmpl.matches("template: &pod_config_chair |").count(),
            1,
            "{name}: expected exactly one anchored chair config block"
        );
        assert_eq!(
            tmpl.matches("template: &pod_config_reviewer |").count(),
            1,
            "{name}: expected exactly one anchored reviewer config block"
        );
        assert_eq!(
            tmpl.matches("template: *pod_config_reviewer").count(),
            1,
            "{name}: expected the second reviewer pod to alias the anchor"
        );
    }
}

/// Chair is reviewer plus the GitHub-App pre_boot hook — nothing else may
/// diverge, so a shared edit cannot land in one file and miss the other.
#[test]
fn chair_config_extends_reviewer_config() {
    assert!(
        CHAIR.starts_with(REVIEWER),
        "pod-config-chair.toml must be pod-config-reviewer.toml plus a hooks \
         suffix — shared sections drifted"
    );
    let suffix = &CHAIR[REVIEWER.len()..];
    assert!(
        suffix.contains("[hooks.pre_boot]"),
        "chair suffix lost the pre_boot hook"
    );
    assert!(
        suffix.contains("\"$HOME/bin/get-gh-app-token.sh\""),
        "chair hook must resolve the App-token minter via the pod's own $HOME"
    );
    assert!(
        !suffix.contains("export HOME="),
        "chair hook must inherit HOME from the image, never bake a path \
         (one script serves every image variant)"
    );
}

/// The two design invariants: no secret material, and no agent identity —
/// the image's own OPENAB_AGENT_COMMAND / HOME decide the agent, so a config
/// can never pin a CLI the image does not contain (the failure that killed
/// the prod lane in 2026-07).
#[test]
fn pod_config_names_no_agent_and_carries_no_secret() {
    for (role, doc) in [("chair", CHAIR), ("reviewer", REVIEWER)] {
        let agent_section: String = doc
            .split("[agent]")
            .nth(1)
            .expect("has [agent] section")
            .split("\n[")
            .next()
            .unwrap()
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n");
        for banned in ["command =", "args =", "working_dir ="] {
            assert!(
                !agent_section.contains(banned),
                "{role}: [agent] must not set `{banned}` — the pod image owns \
                 its agent (ADR 010 B2 demotion)"
            );
        }
        assert!(
            doc.contains("token = \"${OABCP_BOT_TOKEN}\""),
            "{role}: gateway token must stay an env reference (ADR 016)"
        );
        assert!(
            doc.contains("bot_username = \"${OABCP_BOT_NAME}\""),
            "{role}: bot identity comes from pod env, not from the shared file"
        );
    }
}

/// While /bot-config still serves the local-dogfood path, the two delivery
/// channels must agree on the pinned gateway/pool/reaction behavior. The
/// frozen S2 golden is the reference; drift in either direction fails here.
#[test]
fn pod_config_pins_match_the_bot_config_golden() {
    for pin in [
        "platform = \"feishu\"",
        "allow_all_users = true",
        "allow_bot_messages = true",
        "streaming = true",
        "message_processing_mode = \"per-thread\"",
        "max_sessions = 4",
        "session_ttl_hours = 2",
        "remove_after_reply = false",
    ] {
        assert!(
            BOT_CONFIG_GOLDEN.contains(pin),
            "golden lost pin `{pin}` — regenerate story changed?"
        );
        assert!(
            REVIEWER.contains(pin),
            "pod config lost pin `{pin}` present in the /bot-config golden"
        );
    }
    // The inherit_env whitelist must be byte-identical across both channels.
    let golden_inherit = BOT_CONFIG_GOLDEN
        .lines()
        .find(|l| l.starts_with("inherit_env = "))
        .expect("golden has inherit_env");
    assert!(
        REVIEWER.contains(golden_inherit),
        "pod config inherit_env whitelist drifted from the /bot-config golden"
    );
}

/// The templates' bot pods no longer fetch config from the plane.
#[test]
fn templates_do_not_fetch_bot_config() {
    for (name, tmpl) in [("app", APP_TEMPLATE), ("pat", PAT_TEMPLATE)] {
        assert!(
            !tmpl.contains("8090/bot-config/"),
            "{name}: a bot pod still boots from the plane's /bot-config"
        );
        assert_eq!(
            tmpl.matches("- /etc/openab/config.toml").count(),
            3,
            "{name}: all three bot pods must boot from the mounted config"
        );
        for bot in ["chair", "rev1", "rev2"] {
            assert!(
                tmpl.contains(&format!("            default: {bot}\n")),
                "{name}: pod `{bot}` lost its OABCP_BOT_NAME env"
            );
        }
    }
}
