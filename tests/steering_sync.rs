//! Guards the deployment-owned steering channel and the release-drift class.
//!
//! The Zeabur templates preload `docs/steering/pr-review.md` into every bot pod
//! as `/home/node/AGENTS.md` (design.md: agent steering belongs to the bot
//! deployer, never to the plane's `/bot-config`). The doc content is pasted
//! into each template as a YAML block scalar (anchored once, aliased twice),
//! so it can silently drift from the source file — these tests make that
//! drift a hard failure instead.

const DOC: &str = include_str!("../docs/steering/pr-review.md");
const APP_TEMPLATE: &str = include_str!("../zeabur-template-app-1E1Y97.yaml");
const PAT_TEMPLATE: &str = include_str!("../zeabur-template-pat-Z7TQIR.yaml");
const CHAIR_TASK: &str = include_str!("../scripts/pr-review-chair-task.tmpl");
const REVIEWER_TASK: &str = include_str!("../scripts/pr-review-reviewer-task.tmpl");

/// The doc as it appears inside the templates' `template: |` block scalar
/// (14-space indent, blank lines stay empty).
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
fn app_template_carries_current_steering_doc() {
    assert!(
        APP_TEMPLATE.contains(&as_block_scalar(DOC)),
        "zeabur-template-app: the AGENTS.md config block drifted from \
         docs/steering/pr-review.md — re-paste the doc into the anchored \
         `template: &steering_doc |` block"
    );
}

#[test]
fn pat_template_carries_current_steering_doc() {
    assert!(
        PAT_TEMPLATE.contains(&as_block_scalar(DOC)),
        "zeabur-template-pat: the AGENTS.md config block drifted from \
         docs/steering/pr-review.md — re-paste the doc into the anchored \
         `template: &steering_doc |` block"
    );
}

#[test]
fn steering_doc_is_anchored_once_and_aliased_per_bot() {
    for (name, tmpl) in [("app", APP_TEMPLATE), ("pat", PAT_TEMPLATE)] {
        assert_eq!(
            tmpl.matches("template: &steering_doc |").count(),
            1,
            "{name}: expected exactly one anchored steering block"
        );
        assert_eq!(
            tmpl.matches("template: *steering_doc").count(),
            2,
            "{name}: expected the two other bot pods to alias the anchor"
        );
        assert_eq!(
            tmpl.matches("path: /home/node/AGENTS.md").count(),
            3,
            "{name}: expected all three bot pods to mount the steering doc"
        );
    }
}

/// Templates must pin the image tag matching the crate version, so a Cargo
/// bump without a template repin (or vice versa) fails CI instead of shipping
/// a stale binary from a fresh template install (the 0.1.12 incident).
#[test]
fn templates_pin_current_cargo_version() {
    let want = format!(
        "image: ghcr.io/canyugs/openab-control-plane:{}",
        env!("CARGO_PKG_VERSION")
    );
    assert!(
        APP_TEMPLATE.contains(&want),
        "zeabur-template-app pins a different control-plane tag than Cargo.toml ({want})"
    );
    assert!(
        PAT_TEMPLATE.contains(&want),
        "zeabur-template-pat pins a different control-plane tag than Cargo.toml ({want})"
    );
}

#[test]
fn task_prefixes_stay_in_sync_with_role_resolution() {
    let chair_prefix = "Task: manage the GitHub PR status comment";
    let reviewer_prefix = "Task: review GitHub PR";

    assert!(
        CHAIR_TASK
            .lines()
            .next()
            .is_some_and(|line| line.starts_with(chair_prefix)),
        "chair task first line must keep the role-resolution prefix"
    );
    assert!(
        REVIEWER_TASK
            .lines()
            .next()
            .is_some_and(|line| line.starts_with(reviewer_prefix)),
        "reviewer task first line must keep the role-resolution prefix"
    );
    assert!(
        DOC.contains(chair_prefix),
        "steering doc must mention the chair task prefix"
    );
    assert!(
        DOC.contains(reviewer_prefix),
        "steering doc must mention the reviewer task prefix"
    );
}
