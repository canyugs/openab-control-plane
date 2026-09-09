* _2026-09-09 13:16:14 (gpt-5.6-terra/high)_

# Stage 1 acceptance — review commit integrity

Reviewed implementation commit `d6e96c32abfd766588c549e8922e3077d6abcb00` against base `16aa2d4` (resolved as `16aa2d46b0be72a35100e57c8c934bf0835adef4`). HEAD is `ad926e9788b67a4d506653b0f29f945796f6b01a`; all seven reviewed source blobs at HEAD exactly equal the implementation commit. Later HEAD differences are workflow records only. The reviewed source diff is exactly the frozen seven paths, with 1,616 additions and 201 deletions (1,817 changed lines).

## Outcome

No concrete Stage 1 failure was found. A normal matching 40-ASCII-hex (case-normalized) target/findings pair creates a verified proof and commit-carrying status/review intent. Missing, invalid, or mismatched evidence produces a durable failed-closed round, diagnostic comment, and only a valid-target `error` status—never findings, waiver effects, success/failure status, or a formal review. This holds for both APPROVE and REQUEST_CHANGES.

The sender independently rejects authority-bearing normal and decision status/review rows unless the payload has a canonical commit (and matching status SHA), the source round is `verified`, and its durable `verified_commit_id` matches. Legacy/unproven rows are audited and parked before provider send. GitHub review submission includes `commit_id`, validates the returned state, id, and canonical returned commit; replay reconciliation additionally requires controller ownership, marker, event state, and commit identity. The author decision path retains ledger/CAS behavior but withholds approval unless the live head equals the source verified proof. `/ask` remains comment-only.

The security report's None-to-Some observation is reproduced by source inspection: a comment-triggered session initially stored without a head cannot be enriched by a later redelivery, so it closes `missing_target` and needs a new review round. This is an availability limitation, not an approval bypass. It conforms to the Design-Go immutable-target rule that every repeated target admission must be identical; allowing a later live head would violate INV-6. It should remain an explicit operational limitation/proposal for pre-open recovery, not a Stage 1 defect.

Outcome: PASS

## Requirement coverage

| Requirement | Result | Direct evidence |
|---|---|---|
| R-1 | Covered | `canonical_commit_id` and `classify_integrity` require equal full target/claim before either review event. |
| R-2 | Covered | Eight `reviewed_sha` tests cover matching and missing/empty/malformed/mismatch failures for both verdicts. |
| R-3 | Covered | Failed plan persists disposition and queues diagnostic plus valid-target error status only. |
| R-4 | Covered | The approved policy withholds both APPROVE and REQUEST_CHANGES on SHA failure. |
| R-5 | Covered | Request carries `commit_id`; receipt validates returned state, id, and commit. |
| R-6 | Covered | Canonical commit is in authority payloads and expected/provider-receipt audit data. |
| R-7 | Covered | Reclaim lookup requires owned marker plus expected event and commit; conflicts park the write. |
| R-8 | Covered | First-time round gate and unique outbox intent preserve terminal redelivery; replay fixtures exercise adoption. |
| R-9 | Covered | Reviewer-readiness close remains independent and fail-closed, with no formal review. |
| R-10 | Covered | Ask branch queues one sanitized comment and returns before round/findings/status/review persistence. |
| R-11 | Covered | Decisions retain authorized current-head/CAS flow; legacy, failed, and wrong-head proof cannot emit approval. |
| R-12 | Covered | Target admission is insert-or-compare-equal; terminal handling does not re-read a newer head. |

R-13 through R-23 and INV-7 through INV-8 are later gated stages and were correctly not evaluated as missing Stage 1 work.

## Invariant coverage

| Invariant (SPEC meaning) | Result | Evidence |
|---|---|---|
| INV-1 — only valid, equal immutable target/claim authorizes either review event; GitHub echoes state and commit | Covered | Close classifier, payload construction, sender guard, and client response validation. |
| INV-2 — intent, audit, response, and reclaim agree on one canonical commit/event; redelivery creates at most one intent | Covered | Durable proof/payload/audit fields, receipt, identity-aware reconciliation, and redelivery tests. |
| INV-3 — reviewer readiness and SHA integrity compose fail-closed | Covered | Readiness plan is no-review; terminal flow cannot turn either failed gate into authority. |
| INV-4 — asks create no rounds, findings, statuses, or reviews | Covered | Dedicated ask branch and controller/closing tests. |
| INV-5 — authorized current-head decisions preserve CAS semantics; unproven/legacy findings cannot emit approval | Covered | Explicit proof-plus-live-head authorization and decision tests. |
| INV-6 — no close/retry substitutes a later PR head | Covered | Immutable target stores in SQLite/Postgres; reviewed None-to-Some limit enforces this boundary. |

## Minimality and added concepts

Each added concept has a present boundary need: canonical SHA validation; explicit round proof/disposition; immutable target comparison; typed authority payload enforcement; provider receipt/reconciliation identity; and verified-round linkage for author decisions. SQLite and PostgreSQL receive the same additive migration, with existing rows defaulting to `legacy_unverified`; neither migration backfills authority. No new dependency, service, database, or work outside the seven source paths was introduced. Inline tests are within those paths and exercise normal, failure, migration, authority, replay, receipt, `/ask`, and decision compatibility paths.

Minimality: PASS

## Verification

Commands were run sequentially using the supplied local PostgreSQL test setup and `/tmp/ocp-integrity-target` build output:

| Command | Result |
|---|---|
| `cargo test --package github-pr-controller` | PASS — 167 unit + 3 boundary tests. |
| `cargo test` | PASS — 327 tests across the workspace. |
| `cargo test -p github-pr-controller --lib reviewed_sha -- --nocapture` | PASS — 8 focused SHA tests. |
| `cargo fmt --package github-pr-controller -- --check` | PASS. |
| `cargo clippy --package github-pr-controller --all-targets -- -D warnings` | PASS. |
| Additional `cargo test -p github-pr-controller postgres::tests -- --nocapture` | PASS — 16 PostgreSQL tests, including migrations and immutable-target/proof; no skip message occurred, so the supplied database was actually connected. |

The inherited host verification also records a successful `cargo build`. Package formatting passed. Workspace-wide `cargo fmt --check` is not claimed: its three reported differences are the known untouched `src/state.rs`, root `src/store/postgres.rs`, and `tests/second_consumer.rs`; each has identical base, implementation, and HEAD Git blobs. This is a baseline formatting limitation, not an implementation regression.

## Limits and records

No live GitHub API, deployment, credentials, or production PR was accessed. Local fake-GitHub tests prove request/response and reconciliation handling, but real-provider behavior remains a canary/rollout verification item. No cosmetic record defect affects the substantive verdict; later workflow records do not alter the exact reviewed source blobs.

Conformance: PASS

Overall: PASS

Self-check: Frozen scope, output, model, effort and authority declared.
