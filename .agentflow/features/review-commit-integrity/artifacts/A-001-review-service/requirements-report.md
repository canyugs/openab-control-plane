* _2026-09-09 11:51:26 (gpt-5.6-terra/high)_

# OCP code-review improvement requirements

## Authority and outcome

This is a read-only requirements artifact for the independent disposable clone. The only change made is this file. It specifies three approved stages; it does not contain an implementation, migration, test execution result, production assertion, rollout decision, or human-quality adjudication.

The governing scope is the approved three-stage code-review improvement: (1) reviewed-SHA integrity and commit-bound formal reviews, (2) four metrics with a weekly report, and (3) offline, same-PR/head comparison of the current council with independent reviewers and one synthesis. Stage 1 is deliberately the smallest priority patch. Later stages must first inspect their boundaries and produce an exact design before coding.

Relevant local evidence: the controller persists the opening target SHA in `SessionTarget`; `plan_close` currently treats the chair findings-block SHA as provenance but falls back to the target SHA; it queues a formal review without a commit identifier; the outbox reconciles marker-bearing reviews after a lease-expiry retry. The controller already has a durable round/findings/outbox store, signed audit events, plan-only shadow comparison, and explicit `/ask`, dismiss, waive, and reopen paths. ADR 031 assigns provider ingress, product state, and provider side effects to this external controller and requires plan-only comparison plus exactly one active side-effect owner.

## Requirements

### Stage 1 — SHA integrity (priority: small patch)

- R-1. For a normal council round, an approving formal GitHub review is authorized only when the terminal result contains a findings block with a reviewed SHA that is syntactically valid under the chosen Git object identifier rule, is non-empty, and exactly equals the immutable SHA recorded for that session at open.
- R-2. Missing findings block, missing reviewed SHA, invalid reviewed SHA, or a reviewed SHA that differs from the session target SHA must fail closed for approval: no `APPROVE` review and no success status may be emitted from that terminal result.
- R-3. The failure projection must be durable and observable: record the round/result state and emit a diagnostic comment plus an error status when the recorded target SHA is available. The diagnostic must identify the guard category (missing, invalid, or mismatch) without presenting agent-provided text as authority.
- R-4. A blocking result may retain its safe blocking projection only after the exact policy is confirmed in design. It must never be used as a loophole to produce an approval, overwrite a valid result for another SHA, or make the mismatch appear successful.
- R-5. The formal review submission must be bound to the same verified target commit, not merely describe a SHA in its body. The postcondition is that GitHub records the submitted review against that exact commit; an API response that does not prove this is a failed write.
- R-6. The bound SHA must be carried durably in the review outbox payload and in the write audit correlation/receipt, so a queued write is not reinterpreted from current PR state later.
- R-7. Retry and crash recovery must preserve the SHA guard. Reclaim reconciliation may adopt only a controller-owned, marker-bearing formal review whose event and commit identity both equal the queued, verified intent. A marker match by itself is insufficient.
- R-8. Redelivery must remain idempotent: one terminal event produces at most one round and at most one terminal formal-review intent; a lease-expiry retry must neither duplicate a review nor bypass validation.
- R-9. Existing reviewer-readiness failure closure remains intact and composes with SHA integrity: either failed gate prevents an approval.
- R-10. `/ask` is explicitly outside the SHA-verdict gate. A legitimate ask still posts exactly its sanitized answer comment and never creates a round, finding, status, or formal review.
- R-11. Finding-dismiss, waive, and reopen workflows remain legitimate, head-CAS-bound operations on recorded findings. They must not be silently reclassified as terminal council approvals by the stage-1 change. Their present ability to submit a superseding approval after a trusted, current-head judgement is a separately constrained workflow that requires its own commit-binding decision before any shared guard is generalized.
- R-12. Stage 1 must not fetch a newer head as a substitute for the session’s reviewed target. A newer-head rerun/supersession is a new session, not evidence that the closed council reviewed the new code.

### Stage 2 — four metrics and weekly report

- R-13. Define and persist a round-quality metric for each completed council round. Its raw inputs and classification must be inspectable; it must distinguish a valid, guard-satisfying terminal result from degraded, unparseable, reviewer-insufficient, and SHA-integrity-failed results. It must not claim human correctness.
- R-14. Define trigger-to-GitHub latency from the accepted source-trigger timestamp to the timestamp of the terminal GitHub projection that actually completed. Report both unavailable/pending and completed observations; do not substitute session-close time for a GitHub-write completion.
- R-15. Record actual per-round cost when a trustworthy provider/runtime source is available. Where it is unavailable, report `unknown` explicitly, with the reason/source status; do not estimate or present an inferred figure as actual cost.
- R-16. Define reliable completion separately from visible failure and supersession. A reliable completion requires the intended terminal projection to be durably confirmed; visible failure means a failure/error projection was successfully made visible; superseded/abandoned work is a distinct outcome, not completion or a visible failure unless its own tombstone was confirmed.
- R-17. Publish a weekly report that gives counts, denominators, time window, metric definitions/version, unknowns, and a drill-down-safe correlation key for all four metrics. It must retain zero-count categories so absence is distinguishable from omission.
- R-18. Metrics and reports must be observational only: they cannot alter review verdicts, retries, roster selection, GitHub writes, or production routing.

### Stage 3 — offline same-PR/head comparison

- R-19. For an eligible historical or explicitly supplied PR/head snapshot, run the current council and independent reviewer set against identical frozen evidence for the same repository, PR number, and exact head SHA, then run one synthesis over those outputs.
- R-20. Comparison is offline/plan-only: it must cause no GitHub comments, statuses, reviews, webhooks, live-agent messages, or mutation of a production PR. It must not reuse the normal terminal outbox for comparison output.
- R-21. Store a comparison record with immutable input identity, evidence/version references, individual outputs, synthesis output, and a machine-readable comparison classification. Idempotency must be keyed by comparison identity and input content, with conflict on a reused identity carrying different content.
- R-22. Independent reviewers must be independently prompted/executed as defined by the later design; the synthesis may compare their outputs and the council output but must not write GitHub or be presented as human adjudication.
- R-23. Comparison results are decision support only. Human quality adjudication, threshold setting, operational acceptance, and production rollout are separate future decisions.

## User journeys

1. Normal review: a qualifying trigger opens a council session with its immutable target SHA. The council closes with a parseable verdict and findings block naming the same valid SHA. The controller records the round/findings, queues ordered GitHub projections, submits the formal review explicitly for that SHA, and records the provider receipt. A retry reconciles that same event/commit intent.
2. SHA guard failure: the council returns an approval but the findings SHA is absent, malformed, or differs from the opened target. The controller records a failed-closed round, makes a clear diagnostic/error projection where possible, and submits no approval. The author re-runs review for the correct revision rather than relying on a current-head substitution.
3. Legitimate ask: a commenter asks the bot a question. The solo ask session returns a sanitized answer comment. No verdict trailer or findings SHA is required and no branch-protection artifact changes.
4. Legitimate finding dismissal: an authorized writer dismisses a finding with a reason on the current head. The ledger CASes that finding and recomputes visible artifacts according to the existing decision workflow; a stale-head command is refused without mutation. The later stage-1 design must explicitly protect any approval emitted by this separate workflow.
5. Weekly operations review: an operator reads the report for a defined week and can see quality classifications, latency coverage, actual-or-unknown cost, and reliable-completion versus visible-failure/supersession counts without mistaking unknown data for zero.
6. Offline evaluation: an evaluator supplies/chooses a frozen same-PR/head corpus item. The system records council, independent reviewer, and single-synthesis comparison results without any GitHub write. A human later judges usefulness separately.

## Observable acceptance tests (requirements, not executed tests)

- A matching, valid reviewed SHA plus a valid approval queues exactly one success status and one commit-bound `APPROVE`; the returned review receipt proves the same commit.
- Each of missing findings block, missing SHA, empty SHA, malformed SHA, and mismatched SHA queues no `APPROVE` and no success status. It persists the failure category and emits the declared error projection when target SHA exists.
- A `request_changes` result with an SHA anomaly follows the deliberately selected fail-closed policy; it never produces an approval. This case is a design-closure test, not an assumed behavior.
- A terminal event redelivery records no second round or review intent. A forced lease-expiry replay cannot adopt a marker-bearing review with the wrong event or commit, and it does not create a duplicate when the matching controller-owned review exists.
- A PR head moving after session open does not alter the queued review’s commit binding. A re-review can open a separate session for the new head.
- Reviewer-insufficient and unparseable-result cases continue to submit no formal review; combining either with a bad SHA still fails closed.
- An `/ask` terminal result produces one plain answer comment and no round, findings, status, review, or SHA-guard failure notice.
- A dismiss/waive/reopen request on a stale head remains refused; a current-head authorised request retains its current ledger semantics. Any approval it emits is covered by an explicit, tested commit-binding policy before release.
- A weekly report over seeded rows includes all four metric sections, definitions/version, denominators, `unknown` cost observations, zero-valued categories, and distinct reliable-completion, visible-failure, and superseded counts.
- A round with a queued-but-unconfirmed GitHub write is not counted as reliably complete. A successfully posted error/tombstone is classified according to the selected visible-failure/supersession rules.
- An offline comparison of one exact PR/head produces stored council, independent-reviewer, and one-synthesis outputs with no calls to GitHub write operations and no normal outbox rows.
- Repeating an identical comparison is idempotent; reusing its identity with changed frozen input returns a conflict. Changing the head creates a distinct comparison.

## Scope exclusions

- No implementation, source/config edit, dependency addition, refactor, rename, formatting sweep, commit, push, Agentflow use, delegation, live-service access, credential access, or production claim is authorized by this artifact.
- No production rollout, canary expansion, branch-protection change, human quality adjudication, automated merge policy, or threshold for declaring reviewers better is in scope.
- No redesign of OCP’s provider-neutral action/runtime-event boundary, bot protocol, roster semantics, prompt policy, or existing human finding-governance model is implied.
- No metric dashboard, external analytics vendor, cost estimate, retrospective data repair, or backfill is required unless a later exact design selects one.
- No duplicate GitHub writing is permitted for stage 3; comparison must not share the live side-effect owner.

## Risky unknowns requiring discovery before design/coding

- GitHub formal-review API semantics: verify the exact request field and response field that bind/prove a review’s commit, including whether the REST API accepts a full SHA for all intended review events and how it behaves when the head changes.
- SHA validity policy: establish accepted object-ID formats (current SHA-1 only versus future formats), canonical length/case, and whether session target SHA is guaranteed full and immutable from every trigger path.
- Failure projection policy: decide whether an SHA-invalid blocking verdict may post a blocking review/status, versus only comment/error status, and how to prevent stale branch-protection state.
- Decision workflow interaction: establish whether and how dismiss/waive/reopen approval projections bind commits, especially because they obtain a live head and may supersede a prior blocking review.
- Ordering and atomicity: determine the required relationship among comment, status, and formal-review writes when a single outbox item fails/retries, and whether the review is allowed before the comment receipt is present.
- Metric source truth: locate reliable timestamps for ingress acceptance, GitHub completion, terminal state, costs/token usage, and post-retry finality across SQLite and Postgres without relying on log parsing.
- Weekly-report ownership and delivery: choose its consumer, schedule/time-zone boundary, retention, access control, and whether a signed existing observation endpoint is sufficient.
- Offline evidence acquisition: define permitted PR/head snapshots, immutable diff/metadata capture, reviewer isolation, model/version pinning, redaction, retention, and whether comparison can use recorded historical outputs.
- “Independent reviewer” and synthesis contracts: define independence, sample size, prompt/evidence equality, deterministic comparison schema, and protections against the synthesis inventing evidence.

## Routine defaults, pending contrary discovery

- Treat normal council approvals as high-authority branch-protection actions; fail closed on missing, invalid, or mismatched reviewed SHA.
- Treat the SHA captured at session creation as immutable review scope; do not replace it with current PR head at close or retry.
- Preserve controller ownership checks, signed runtime-event admission, durable outbox claims, per-session markers, and audit journaling.
- Keep `/ask` answer-only and retain finding decisions as separate authorised, current-head-CAS workflows.
- Use explicit `unknown`, not zero or an estimate, for unavailable actual cost and unavailable latency completion.
- Treat reliable completion, visible failure, and supersession as mutually distinguishable report categories; do not collapse them into a single “done” rate.
- Keep stage-3 work plan-only/offline with a single GitHub side-effect owner, consistent with ADR 031’s migration invariants.

Self-check: Frozen scope, output, model, effort and authority declared.
