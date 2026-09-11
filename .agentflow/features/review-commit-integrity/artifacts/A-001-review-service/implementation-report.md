* _2026-09-09 13:04:22 (gpt-5.6-luna/max)_

# Stage 1 implementation report

The approved Stage 1 review-commit-integrity changes are implemented in the
declared source files. The result is for inspection/import by the parent
workflow; it makes no production-readiness claim.

## Coverage

- R-1: Both verdict paths require a target and reviewed commit ID that are
  exactly 40 ASCII hexadecimal characters and compare canonically before
  findings, waivers, formal reviews, or success effects.
- R-2: Missing, malformed, and mismatched reviewed IDs fail closed with a
  durable diagnostic; a failure status is emitted only when the immutable
  target itself is valid.
- R-3: SQLite and PostgreSQL persist `verified_commit_id` and
  `integrity_disposition`, default legacy rows to `legacy_unverified`, and
  expose a proof-only lookup.
- R-4: Integrity failure prevents both approval and change-request formal
  review effects; `/ask`, unparseable input, and reviewer-failure behavior
  remain on their existing paths.
- R-5: Formal review POSTs include the verified commit and require the
  expected returned state and commit in the receipt.
- R-6: Status/review outbox payloads and write audits carry the commit ID;
  provider receipts and reconciliation are commit-bound.
- R-7: Reconciliation requires the owned controller marker, owner identity,
  expected state, and expected commit; conflicts stop before a duplicate POST.
- R-8: Existing durable outbox retry/parking is reused, with authority
  writes guarded on every send, including legacy replay.
- R-9: Reviewer approval remains gated on the verified current PR head.
- R-10: `/ask` bypasses the review-integrity policy while retaining sanitized
  comment behavior.
- R-11: Decision ledger CAS and replies are retained for unverified sources,
  but approval/status authority is withheld and the author is told to
  re-run review.
- R-12: Session targets are insert-or-compare-equal immutable records; later
  head changes cannot substitute for the recorded target.

- INV-1: Any authority write requires one canonical commit ID shared by the
  verified round proof, payload, status SHA where applicable, and provider
  receipt.
- INV-2: A missing, legacy, unverified, or conflicting proof cannot authorize
  an approval or status write.
- INV-3: Session-target retries accept exact equality only and durably reject
  changes to repository, PR, head, reason, or reviewer quorum.
- INV-4: Owned markers are checked together with owner, expected state, and
  commit; a payload-version flag is not used as the guard.
- INV-5: Decision ledger CAS/reply behavior survives authority withholding,
  while unauthorized sources emit neither approval nor status.
- INV-6: Authority rejection and retry/parking outcomes are durably audited
  with classified failure categories.

## Changed files

Source changes are limited to:

- `crates/github-pr-controller/src/closing.rs`
- `crates/github-pr-controller/src/deciding.rs`
- `crates/github-pr-controller/src/github.rs`
- `crates/github-pr-controller/src/lib.rs`
- `crates/github-pr-controller/src/store.rs`
- `crates/github-pr-controller/src/store/sqlite.rs`
- `crates/github-pr-controller/src/store/postgres.rs`

The only additional result files are `red-regression.patch` and this report.
No other files were changed.

## Red-first evidence

Before production edits, the tests-only diff was saved as
`red-regression.patch` and this focused command was run:

```text
CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 cargo test --package github-pr-controller --lib reviewed_sha -- --nocapture
```

Old source result: 7 tests ran; 1 positive-control test passed and 6 tests
for missing, malformed, and mismatched reviewed SHAs failed with assertion
mismatches. The code compiled; these were real behavioral assertion failures,
not compilation failures.

## Verification

- `cargo fmt --package github-pr-controller -- --check` — passed.
- `cargo fmt --check` — failed only on pre-existing formatting in
  unauthorized files `src/state.rs:643`, `src/store/postgres.rs:4123`, and
  `tests/second_consumer.rs:416`; none were edited.
- `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 cargo build` — passed.
- Focused final reviewed-SHA test — 8 passed, 0 failed, 159 filtered.
- `TEST_POSTGRES_URL` set,
  `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 cargo test --package github-pr-controller` — 134 passed, 33 failed, 0 ignored. The 33 failures were environment `PermissionDenied` errors from sandbox-blocked localhost fake servers and PostgreSQL connections; there were no assertion failures and PostgreSQL tests were attempted, not skipped.
- `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 cargo test` — 259 root-package tests passed and 21 failed with the same sandbox `PermissionDenied` category (localhost/PostgreSQL); the other workspace test groups passed.
- `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 cargo clippy --package github-pr-controller --all-targets -- -D warnings` — passed.
- Final `git diff --check` — passed.

## Known limits

The disposable execution environment denied TCP/listen operations, so live
GitHub HTTP fake-server tests and PostgreSQL runtime tests could not complete;
the PostgreSQL paths compiled and their tests reached the connection attempt.
The workspace-wide format check remains blocked by untouched out-of-scope
baseline files. No live GitHub behavior, deployment, migration, or production
canary was exercised, and this report makes no production claim.

Self-check: Exact approved design, authority, scope, result paths, red-first and compatibility gates declared.
