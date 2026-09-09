* _2026-09-09 13:08:21 (gpt-5.6-luna/max)_

Changed only the final `last_comment_id` assertion in `crates/github-pr-controller/src/lib.rs`: expected `Some(1001)` and updated the stale no-round-row message. The verified `ses_1` fixture owns comment `1001`; attacker decoy comment `900` remains excluded. Duplicate/replay and identity assertions were preserved.

Results:

- `cargo test -p github-pr-controller a_replayed_write_reconciles_instead_of_posting_twice --`: passed, 1 test passed, 0 failed.
- `cargo fmt --package github-pr-controller -- --check`: passed.
- No commit made.

Self-check: Frozen scope, output, model, effort and authority declared.
