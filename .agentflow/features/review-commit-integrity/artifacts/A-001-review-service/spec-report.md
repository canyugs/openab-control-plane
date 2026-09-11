* _2026-09-09 11:59:02 (gpt-5.6-terra/high)_

# Review-service improvement — implementation-ready Stage 1 design

## Authority, decision, and frozen scope

This is a read-only advisor specification for the disposable clone. Its only write is `spec-report.md`; it neither changes source/configuration nor runs tests, Agentflow, delegates, accesses credentials/live services, commits, or pushes. The approved programme has three stages, but this is an implementation-ready design **only for Stage 1**. Source work must wait for a later Design Go on the exact commit containing this design.

Stage 1 makes every normal-council formal review commit-bound and proves that the council's claimed findings SHA is the immutable opened target. Its small, repository-native durable proof is an additive integrity record on the existing `review_rounds` row, cross-checked by each authority-bearing outbox payload and sender. No new service, registry, dependency, cleanup, deletion, or historical backfill is proposed.

The trusted evidence is deliberately narrow:

- The controller-recorded `SessionTarget` is the only candidate target. It originated from signed GitHub ingress or the controller's pre-open GitHub read; it is not chair text.
- A target and claimed findings SHA each must be exactly 40 ASCII hex characters. Normalize valid hex to lowercase before equality; reject empty, abbreviated, placeholder, or non-hex values. Normalization never fills in absent evidence.
- A matching claim proves only input-SHA consistency, **not** that a model truly read the diff. Stage 1 must not assert the latter.

Failure categories are `missing_target`, `invalid_target`, `missing_reviewed_sha`, `invalid_reviewed_sha`, and `reviewed_sha_mismatch`. They are controller-derived categories, never copied from agent prose.

## User journey and Stage 1 behavior

On a normal `/review` or PR-triggered council close, the controller first preserves the opened target and validates it and the parsed findings SHA. A matching valid 40-hex pair records a `verified` round, imports its findings, applies its one-time waiver-fired bookkeeping, queues the normal comment, commit-targeted status, and commit-bound formal review, and later accepts only a GitHub receipt for the same review state and commit.

For either `APPROVE` or `REQUEST_CHANGES`, a missing, invalid, or mismatched target/reviewed SHA records a failed-closed round with that disposition, posts a diagnostic comment, and posts an `error` status only when the stored target is itself a valid full SHA. It imports no findings, fires no waivers, queues no formal review, and emits no success or failure status. This selects the host-corrected R-4 policy: neither verdict gets a blocking projection from unverified evidence.

`/ask` remains answer-only: it bypasses this policy and emits its sanitized comment only. Dismiss, waive, and reopen retain their authorization check, current-head read, CAS ledger mutation, waiver/reopen behavior, and reply. A decision can issue its existing success/`APPROVE` projection only where its source session has a Stage-1 `verified` round for the same trusted current full SHA; otherwise the decision is recorded/replied to but queues no authority-bearing projection and tells the author to re-run review. This prevents legacy findings from becoming an approval escape without reclassifying decision commands as terminal council results.

## Minimal design

1. **Immutable target admission.** Change `record_session_target` in both stores from update-on-conflict to insert-or-compare-equal. A repeat accepts only the same repo, PR, raw target, reason, and reviewer requirement; any difference is an error. Do not fetch a current head during terminal handling. A missing/invalid stored target is retained as historical input but cannot authorize a result.

2. **One normal-close integrity decision.** In `closing.rs`, after the existing `/ask` split and alongside the reviewer-readiness gate, classify target/findings evidence once. The valid branch uses the canonical target as (a) `ReviewRound.verified_commit_id`, (b) every imported finding's head, (c) status `sha`/`commit_id`, and (d) review payload `commit_id`. The failed branch uses `decision = "sha_integrity_failed"`, records the exact disposition, and produces only diagnostic/error writes. It must run before finding import and waiver firing. Unparseable and reviewer-insufficient closes remain their existing no-review, error behavior; neither can restore authority when paired with bad SHA.

3. **Durable proof, not an ambiguous SHA.** Add nullable `verified_commit_id` and non-null `integrity_disposition` to `review_rounds` in SQLite and Postgres. New rows use `verified`, a listed failure category, or the existing non-authority outcomes; migrated rows default to `legacy_unverified`. Retain existing `head_sha` only as historical claimed provenance—never read it as proof. Add a store query returning a session's disposition and verified commit. This is smaller and safer than a new proof table, while avoiding the current field's mixed claimed/fallback meanings.

4. **Typed authority payload checks.** New normal review payloads contain required `event` and canonical `commit_id`; success/failure statuses contain `commit_id` equal to `sha`. The outbox drain treats a normal review of either event, a normal success/failure status, and a decision success/approval as authority-bearing. Before send, it requires a valid payload commit, a `verified` source round, and exact equality to that row's `verified_commit_id`; decision writes additionally require the decision's trusted current head to equal it. Missing fields, malformed values, non-verified/legacy rounds, or disagreement are failed/parked with a durable audit error and never sent. Error statuses and comments remain sendable.

   This is intentionally stronger than adding a payload version: old `github_writes` rows lack a commit proof, and old findings/rounds lack a verified disposition, so both fail the sender check. They are not reinterpreted using a present PR head or target equality. The implementation must not mutate them in a migration, enqueue replacements under the colliding `(session_id, kind)` key, delete them, or backfill them. When a legacy row is claimed it is safely marked failed with an `unproven_legacy_authority_intent` audit/error category; a previously sent historic review is not retroactively changed.

5. **GitHub request, receipt, and reclaim proof.** Change `GitHubClient::submit_review` to accept `commit_id`, send GitHub's documented `commit_id` field with `event`/`body`, and return a receipt containing review id, returned state, and returned `commit_id`. A 2xx response missing/wrong state or missing/wrong canonical commit is a failed write. The audit request correlation records expected commit/event; the provider receipt records returned id/state/commit and whether reconciled.

   Replace marker-only review lookup with an identity-aware lookup. A reclaim may adopt only an entry that has the session/decision marker, is controller-owned under the existing login/app rule, has the expected response state for the queued event, and has the expected `commit_id`. An owned marker with a wrong event or commit is a conflict: fail/park without submitting another review. A foreign marker is not adopted; absence permits the one normal submit. Thus a lease retry can adopt exactly one matching review but cannot turn a stale or malformed intent into one.

6. **Author-decision linkage.** Pass the controller-read, full SHA into decision payloads as `commit_id`; have the decision planner take an explicit `approval_authorized` result from the store proof check rather than infer it from finding text. For a verified matching round, its status/review use that SHA and the common sender/receipt/reclaim checks. For old, SHA-failed, or different-head findings, keep the CAS mutation and human reply but omit success/review. `reopen` remains no-approve. This is the smallest explicit policy that preserves the workflow while making a legacy finding incapable of bypassing the new guard.

Rejected smaller alternatives: validating only `review_body` leaves payloads/retries unbound; accepting `SessionTarget` fallback makes missing chair evidence look valid; a payload-version flag does not prove old payloads/findings; marker plus ownership does not identify the review event or commit; fetching a new head silently changes review scope; and applying the findings parser globally would incorrectly turn `/ask` into an SHA-gated review.

## Expected edits and red-first verification

Expected code is limited to `crates/github-pr-controller/src/{closing.rs,github.rs,lib.rs,deciding.rs,store.rs,store/sqlite.rs,store/postgres.rs}` and their existing inline/unit tests. No workspace dependency, API route, configuration, or source outside this controller is required.

Start with failing tests, then implement the smallest change that passes:

- `closing.rs`: valid matching full SHA for approve and request-changes produces the expected commit-carrying writes; separately test missing block/SHA, empty, malformed, invalid target, and mismatch for both verdicts: durable category, diagnostic/error, no findings, waiver firing, formal review, success, or failure status.
- `lib.rs` terminal flow: valid output persists `verified_commit_id`; redelivery creates one round/intent; reviewer-insufficient and unparseable combinations never regain authority; `/ask` with verdict-like text still writes one answer only.
- `github.rs`: request JSON includes `commit_id`; returned state and `commit_id` must both match; reconciliation accepts only marker + owned identity + expected state + expected commit. Cover wrong event/commit marker conflicts and a matching lease-retry receipt.
- SQLite and Postgres: migration default makes existing rounds legacy; first-write/compare-equal target recording accepts equal retry and refuses changed target identity; old pending/in-flight normal review and success/failure status payloads cannot send; old findings cannot generate a decision approval. Cover valid decision approval at the same live head, stale-head refusal, and reopen's no-approve behavior.

Run after implementation:

```sh
cargo fmt --check
cargo build
cargo test --package github-pr-controller
cargo test
cargo clippy --package github-pr-controller --all-targets -- -D warnings
```

Compatibility risk is intentional fail-closed behavior: legacy pending authority writes and author decisions sourced from unproven rounds will no longer unblock a PR. Existing comments, non-authority error reports, `/ask`, and ledger CAS commands remain compatible. The implementation must document the audit categories so operators can distinguish an integrity failure, a legacy-proof failure, a GitHub response mismatch, and a reconciliation conflict.

## Coverage ledger and invariants

| Requirements | Treatment | Invariant |
|---|---|---|
| R-1–R-5 | Stage 1, steps 1–5 | INV-1: only a valid, equal immutable target and claimed SHA authorizes either formal-review event; GitHub must echo event state and commit. |
| R-6–R-8 | Stage 1, steps 3–5 | INV-2: intent, audit, response, and reclaim agree on one canonical commit/event; redelivery creates at most one intent. |
| R-9 | Stage 1, step 2 | INV-3: reviewer readiness and SHA integrity compose fail-closed. |
| R-10 | Stage 1, journey | INV-4: asks never create rounds, findings, statuses, or reviews. |
| R-11 | Stage 1, step 6 | INV-5: authorized current-head decisions preserve CAS semantics; unproven/legacy findings cannot emit approval. |
| R-12 | Stage 1, steps 1 and 4 | INV-6: no close/retry substitutes a later PR head. |
| R-13–R-18 | Deferred to Stage 2 | INV-7: metrics are inspectable observational data; no quality claim from gates, no behavioral effect. |
| R-19–R-23 | Deferred to Stage 3 | INV-8: offline frozen-input comparison has no GitHub/live effects; independent arm gets exactly one synthesis, never a combined-arm synthesis. |

## Later-stage outcomes and gates

**Stage 2 outcome:** a later exact design defines the four inspectable metrics and a weekly report, including human finding validity/usefulness and confirmed escapes as quality—not SHA/reviewer gates—plus latency completion, actual-or-unknown cost, and reliable completion versus visible failure/supersession. Gate: Stage 1 is implemented and verified; then inspect actual durable timestamps/cost sources, report consumer/time window, and retention/access before selecting any schema or schedule. No dashboard or mandatory new storage is approved here.

**Stage 3 outcome:** a later exact design runs an offline, frozen same-PR/head comparison between the existing council and independently prompted reviewers, followed by exactly one synthesis over the independent-reviewer arm only. Gate: select permissible immutable evidence and redaction/retention rules, prove plan-only isolation from the normal outbox, and define artifact identity/conflict handling. Immutable local artifacts and input manifests remain the baseline; no comparison database/service or production routing is authorized here.

## Production canary and rollout gate

After code verification and a separate security scan of the approval-authority change, release only to a canary repository with normal GitHub credentials. Exercise matching approve and request-changes, every SHA failure category, an intentional wrong receipt/reconcile fixture where feasible, lease recovery, `/ask`, and an authorized decision on a verified current-head round. Confirm GitHub shows the exact requested `commit_id`, audit records contain expected/returned commit and state, and no legacy authority row was sent. A human must review these observations and explicitly approve expansion; this design neither changes branch protection nor authorizes rollout.

Self-check: Frozen scope, output, model, effort and authority declared.
