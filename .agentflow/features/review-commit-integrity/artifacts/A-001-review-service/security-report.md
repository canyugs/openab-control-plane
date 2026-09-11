* _2026-09-09 13:11:37 (gpt-5.6-sol/low)_

# Security review

Reviewed exact candidate commit `d6e96c32abfd766588c549e8922e3077d6abcb00` against base `16aa2d46b0be72a35100e57c8c934bf0835adef4`, restricted to the seven authorized paths under `crates/github-pr-controller/src`. No tests were run.

## Finding: transient head lookup failure permanently prevents retry enrichment

Severity: Medium (availability / fail-closed regression)

Fact: A comment-triggered review has no webhook SHA, so after opening the session the controller attempts to read the current PR head from GitHub. If that lookup fails, it deliberately continues and records the session target with `head_sha = None` (`crates/github-pr-controller/src/lib.rs:980-1016`). The candidate changes both stores so every later admission for that session must be byte-for-byte identical, including `head_sha` (`crates/github-pr-controller/src/store/sqlite.rs:538-587`; `crates/github-pr-controller/src/store/postgres.rs:799-846`). Therefore, if a later delivery resolves to the same idempotently opened session after GitHub recovers, its newly obtained `Some(valid_sha)` conflicts with the persisted `None`; the controller only logs that conflict and continues, leaving the original target permanently SHA-less. At terminal handling, a verdict on that session is consequently classified `missing_target`, so no APPROVE or REQUEST_CHANGES review can be emitted. The base implementation explicitly allowed `None` to be enriched to `Some` via `COALESCE(excluded.head_sha, session_targets.head_sha)` in both stores.

Impact: A single transient GitHub head-read failure can make an otherwise legitimate comment-triggered review session unable to produce a formal terminal review even when an idempotent retry later has authoritative SHA evidence. This does not reopen the reported SHA-authority bypass—the close still fails safely—but it breaks the requested legitimate retry behavior and can strand a review until a genuinely new session is created.

How to verify: In each store backend, call `record_session_target` for one session with identical repo, PR, reason, and reviewer count but `head_sha = None`; then call it again with a valid 40-byte SHA. The candidate returns `StoreError::Conflict` and `session_target` still returns `None` for `head_sha`. Exercise the caller by making the first `pull_head_sha` request fail and a redelivery for the same idempotently opened session succeed; terminal handling will persist a `sha_integrity_failed` / `missing_target` round rather than a verified round. No test was run during this pass.

## Coverage and conclusion

I traced terminal classification and write construction in `closing.rs`; explicit proof storage and migrations in `store.rs`, `store/sqlite.rs`, and `store/postgres.rs`; sender-side enforcement for pending and reclaimed outbox entries plus provider receipts in `lib.rs`; review submission and reconciliation identity checks in `github.rs`; and author dismiss/waive/reopen recomputation in `deciding.rs` and its caller. Apart from the retry regression above, I found no concrete source-backed route in the authorized scope that lets a missing, malformed, mismatched, or legacy-unverified chair SHA authorize APPROVE, REQUEST_CHANGES, or a success/failure authority status. Scope excludes code outside the seven authorized files, runtime/provider behavior beyond these call contracts, and dynamic verification because tests were prohibited.

Self-check: Frozen scope, output, model, effort and authority declared.
