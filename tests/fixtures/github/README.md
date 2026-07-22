# GitHub compatibility replay fixtures

Synthetic payloads for pinning the embedded PR-review behavior before ADR 031
externalization. They contain no live delivery id, repository content, user data,
webhook secret, App identity, or token.

`src/plugins/pr_review/webhook.rs` replays these through the same parser/handler
used by production. `pull_request_opened.plan.json` is the normalized plan golden:
runtime-generated ids and timestamps are deliberately excluded.
