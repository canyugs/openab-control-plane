# Issue #349 — report delivery gate

## Objective

Prevent an obvious blank reviewer turn from satisfying a controller-backed
council quorum while keeping non-contract coordinator behavior unchanged.
Bound retries from the transcript, trim a reviewer after exhaustion, and let
the existing roster-change path re-evaluate the reduced quorum.

## Work slices

- [x] Add the conservative `pr_review::report_delivered` predicate and tests.
- [x] Gate controller-backed quorum votes and re-request invalid reports with a
  stable transcript prefix and per-reviewer retry limit.
- [x] Add `Action::TrimReviewer`; share the trim body between liveness and the
  action arm, preferring an eligible spare before shrinking the roster.
- [x] Extend coordinator fakes with per-bot settled content and count retry
  prompts from the runtime transcript without changing `Ctx`.
- [x] Add coordinator tests, a real SQLite/AppState wiring test, and the
  `handle_reply` echo-then-real-report integration path.
- [x] Run the purity grep, focused tests, workspace tests, formatting, and
  locked all-target/all-feature clippy.

## Constraints

- No schema changes.
- The verdict contract is keyed by the existing two-key rule: explicit review
  contract or a `controller:` trigger reference.
- Chair validation and the watchdog remain unchanged.
- Keep the four kernel files free of plugin-specific vocabulary required by CI.

## Verification evidence

- `cargo test --lib`: 375 passed.
- `cargo test --workspace`: all workspace tests passed; 2 L3 tests ignored by
  their existing real-GitHub-App gate.
- `cargo clippy --locked --all-targets --all-features --workspace -- -D warnings`:
  passed.
- `cargo fmt --all -- --check`: passed.
- Purity grep over the four kernel files: passed.
- `git diff --check`: passed.
