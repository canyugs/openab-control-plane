* _2026-09-09 13:45:23 (gpt-5.6-terra/high)_

# Stage 2 discovery/codewalk — weekly observational metrics

Coverage marker: `stage2-codewalk/shared-discovery/v1` — read-only inspection of
the current disposable clone at `f02b5ff` (which contains accepted Stage 1 after
the requested `d6e96c3` anchor). Authority was limited to this clone and this
report; no source/configuration, database, network, tests, Agentflow, delegation,
commit, or live-service access occurred.

The referenced `../A-001-review-service/requirements-report.md` and mandatory
`requirements-resolution.md` are outside this clone. The stated authority
expressly forbids reading outside it, and neither artifact is present here
(`rg --files` found none). This report therefore treats the request's four
metric requirements as controlling and records only in-clone design/implementation
evidence. `docs/design.md` confirms review/GitHub logic belongs to the separate
`github-pr-controller` process, not the kernel ([docs/design.md:44-48](docs/design.md#L44)).

## Bottom line

An offline, user-invoked local weekly report over explicit exported evidence is
enough for the smallest first design. It should not become an OCP service,
schema migration, scheduler, API rollup, dashboard, or cost estimator. This is
also the existing direction: ADR 021 calls for an offline script outside OCP
that starts from available state, not a kernel feature ([docs/adr/021-review-effectiveness-feedback-loop.md:49-53](docs/adr/021-review-effectiveness-feedback-loop.md#L49)); ADR 032 describes the later scheduled/outcomes join
([docs/adr/032-review-calibration-loops.md:73-82](docs/adr/032-review-calibration-loops.md#L73)).

Conservative default proposal: the operator invokes a local command with an
explicit evidence directory and `--week YYYY-Www`; weeks are Monday 00:00:00
through the following Monday 00:00:00 in `Asia/Taipei`; it writes a local
Markdown/JSON report only; it makes no network call or external post; absent,
ambiguous, or unjoinable evidence remains `unknown` and is counted in an
unknown/coverage denominator rather than guessed away.

## Evidence map

| Requested metric | Durable evidence found | What can be stated now | Gap / required explicit input |
| --- | --- | --- | --- |
| Human finding validity/usefulness and confirmed escapes | Controller `review_findings` retains `repo`, PR, stable finding ID, severity, status, path, `raised_by`, `angle`, reviewed SHA and creation time ([crates/github-pr-controller/src/store/sqlite.rs:80-96](crates/github-pr-controller/src/store/sqlite.rs#L80); [crates/github-pr-controller/src/store.rs:238-265](crates/github-pr-controller/src/store.rs#L238)). Author decisions record actor, reason, and timestamp ([crates/github-pr-controller/src/store/sqlite.rs:788-821](crates/github-pr-controller/src/store/sqlite.rs#L788)). | Finding volume and the existence of a dismissal/waiver decision are factual. A report can join each finding to an explicit human annotation by `(repo, pr_number, head_sha, stable_id)`. | No field records `valid`, `invalid`, `useful`, fix verification, or an escape. `dismissed` and `waived` are decision states, not validated false-positive labels; waiver is an accepted trade-off with expiry ([crates/github-pr-controller/src/store.rs:499-519](crates/github-pr-controller/src/store.rs#L499)). Provide reviewed, versioned human-annotation and human-confirmed-escape exports. Never infer either outcome from a dismissal, waiver, lack of action, commit proximity, or status colour. |
| Accepted trigger → actually completed GitHub projection latency | The controller audit schema includes trigger reference/fingerprint, session and write identities, outcome, occurred/recorded times, and provider detail ([crates/github-pr-controller/src/store/sqlite.rs:129-167](crates/github-pr-controller/src/store/sqlite.rs#L129)). `github.write.enqueued` is atomically journaled with each outbox row ([crates/github-pr-controller/src/store/sqlite.rs:1039-1080](crates/github-pr-controller/src/store/sqlite.rs#L1039)); successful/reconciled writes emit an audit event containing a provider receipt ([crates/github-pr-controller/src/lib.rs:2823-2855](crates/github-pr-controller/src/lib.rs#L2823)). Trigger acceptance is audited with trigger correlation in the webhook path ([crates/github-pr-controller/src/lib.rs:848-898](crates/github-pr-controller/src/lib.rs#L848)). | With a shared `trigger_ref`/fingerprint/session ID, an export can measure accepted-to-successful provider projection per write and report partial sets. GitHub formal-review success validates the returned state and commit ID ([crates/github-pr-controller/src/github.rs:213-261](crates/github-pr-controller/src/github.rs#L213)). | There is no single persisted “all required writes for this projection completed” receipt. The report must derive a conservative projection outcome from the expected enqueued writes and their audit outcomes, or mark it unknown if any expected write lacks terminal evidence. Do not use enqueue, claim, terminal session, or a visible comment as completion. |
| Actual cost, otherwise explicit unknown | Provider identity is only bot metadata/failover information in the kernel; it is not accounting ([src/orchestrator.rs:1411-1440](src/orchestrator.rs#L1411)). The controller’s GitHub receipts are review IDs/state/commit IDs, not usage ([crates/github-pr-controller/src/github.rs:22-27](crates/github-pr-controller/src/github.rs#L22)). Neither workspace manifest introduces billing/usage telemetry ([Cargo.toml:16-42](Cargo.toml#L16); [crates/github-pr-controller/Cargo.toml:7-29](crates/github-pr-controller/Cargo.toml#L7)). | The report can truthfully say `actual_cost: unknown` for every scoped unit unless an operator supplies reconciled actual-cost evidence. | No token count, model, unit price, provider usage receipt, or cost field was found in the examined schemas/audit contract. Accept a separately exported, reconciled actual-cost input keyed at least by provider account, time interval, and a documented allocation key; otherwise retain unknown. No estimator or inferred cost belongs in this slice. |
| Reliable completion vs visible failure vs supersession | Runtime events admit `session.terminal`, `session.timeout`, and `session.superseded` ([crates/github-pr-controller/src/runtime_events.rs:136-145](crates/github-pr-controller/src/runtime_events.rs#L136)). Controller outbox rows retain state, attempts, error, enqueue, claim and done timestamps ([crates/github-pr-controller/src/store/sqlite.rs:97-112](crates/github-pr-controller/src/store/sqlite.rs#L97); [crates/github-pr-controller/src/store/sqlite.rs:1083-1192](crates/github-pr-controller/src/store/sqlite.rs#L1083)). Failures are journaled as retry-scheduled or failed, including error class/provider target ([crates/github-pr-controller/src/lib.rs:2241-2310](crates/github-pr-controller/src/lib.rs#L2241)). | A report can classify a scoped attempt as `superseded`, `visible_failure`, `reliably_completed`, or `unknown/incomplete` from exported terminal/runtime and write-audit evidence. Supersession is its own terminal outcome, never a failure denominator and never a completion. | “Visible failure” needs a report definition: conservative default is a terminally failed required GitHub write (`github.write.failed`) or an explicitly recorded terminal failure/timeout whose public abandonment/comment projection succeeded. Retries, pending/in-flight rows, missing journal rows, and locally observed errors are not visible failure; they remain incomplete/unknown. |

Facts above are repository facts. The classification rules and suggested joins are
inferences for the smallest report design, deliberately conservative.

## Correlation, identities, and clocks

- The controller makes `session_targets` immutable for a session, retaining repo,
  PR, head SHA, reason, and required reviewer evidence ([crates/github-pr-controller/src/store/sqlite.rs:538-590](crates/github-pr-controller/src/store/sqlite.rs#L538)). A review round is unique by session, and its head/proven commit identity is separately retained ([crates/github-pr-controller/src/store/sqlite.rs:59-79](crates/github-pr-controller/src/store/sqlite.rs#L59); [crates/github-pr-controller/src/store.rs:194-228](crates/github-pr-controller/src/store.rs#L194)). These are the durable round/session/write identities to export.
- `review_findings` insertion is append-only/idempotent by session
  ([crates/github-pr-controller/src/store/sqlite.rs:993-1037](crates/github-pr-controller/src/store/sqlite.rs#L993)); finding `stable_id` is only safely unique in its repo/PR/head context. Join annotations at that full scope, never stable ID alone.
- Audit correlation has first-class delivery, controller/action, trigger,
  fingerprint, session, message, runtime-event and write IDs
  ([crates/controller-protocol/src/audit.rs:62-84](crates/controller-protocol/src/audit.rs#L62)). Prefer these IDs over parsing log prose or GitHub comment text.
- Kernel session timestamps are Unix milliseconds; controller product-table
  timestamps are Unix seconds, while the shared audit contract is Unix
  milliseconds ([crates/github-pr-controller/src/store.rs:45-67](crates/github-pr-controller/src/store.rs#L45)). Normalize explicit units before any join. They are wall clocks: the kernel already guards against negative elapsed durations due to skew ([src/store.rs:4633-4644](src/store.rs#L4633)); flag negative/cross-clock latency as `clock_invalid`, never clamp it to zero.
- A kernel session’s opaque controller trigger reference is namespaced/hashed;
  controller audit preserves the original trigger reference. Do not parse the
  kernel value to recover GitHub identity. The kernel controller-action binding
  preserves both trigger forms/fingerprint as distinct values
  ([src/controller_api.rs:768-797](src/controller_api.rs#L768)).

## Minimal report inputs and necessary future edits

No implementation is authorized in this stage. The smallest later change is an
ops/local-report artifact, not a product change:

1. Define an evidence-directory contract, checked into the report tool/tests:
   `controller-audit.ndjson` (complete cursor-paginated audit export),
   `review-findings.ndjson`, `human-finding-annotations.ndjson`,
   `confirmed-escapes.ndjson`, and optional `actual-cost.ndjson`. Each record
   needs a schema version, source/export time, timezone/unit declaration, stable
   correlation fields, and source reference. Require an explicit `evidence-manifest.json`
   with export completeness/cursor information.
2. Manual finding annotations must have a human identity, timestamp, explicit
   verdict (`valid_useful`, `valid_not_useful`, `invalid`, or `unknown`), and a
   short evidence reference. `dismissed`/`waived` may be displayed as context
   only; neither populates the valid/invalid numerator. This follows the
   human-gated posture in ADR 021 ([docs/adr/021-review-effectiveness-feedback-loop.md:26-42](docs/adr/021-review-effectiveness-feedback-loop.md#L26)) and ADR 032’s rule that dismissal needs structured ground truth ([docs/adr/032-review-calibration-loops.md:110-120](docs/adr/032-review-calibration-loops.md#L110)).
3. A confirmed escape must be a separate human record: affected repo/path,
   defect/fix reference, confirmation identity/time, matching review window,
   and a disposition (`confirmed_escape`, `not_escape`, `unknown`). Candidate
  -generation can be proposed later, but it must not enter the confirmed-escape
   numerator. ADR 032 itself calls escape candidates human-triaged
   ([docs/adr/032-review-calibration-loops.md:78-82](docs/adr/032-review-calibration-loops.md#L78)).
4. Report projection per individual required GitHub write and derive a round
   summary: `reliably_completed` only when all expected required writes have
   successful/reconciled provider receipts; `visible_failure` only under the
   strict definition above; `superseded` only from an explicit supersede event;
   otherwise `unknown_or_incomplete`. Retain every denominator: accepted,
   projectable, completed, visible-failure, superseded, unknown/incomplete, and
   excluded-with-reason.
5. Add fixture-only unit tests for schema validation, duplicate/idempotent audit
   events, missing terminal receipts, every state classification, unit conversion,
   Taipei week boundaries, dismissal/waive non-inference, and unknown cost. Do
   not add a service API, scheduler, DB table, dashboard, or cost estimator.

Existing read paths can supply explicit extracts but are not a report/export
product: controller `GET /api/v1/audit/events` is signed and cursor-paginated
([crates/github-pr-controller/src/lib.rs:1321-1396](crates/github-pr-controller/src/lib.rs#L1321)); controller findings have a read route
([crates/github-pr-controller/src/lib.rs:400-415](crates/github-pr-controller/src/lib.rs#L400)); kernel audit is bearer-protected and paginated
([src/api.rs:189-235](src/api.rs#L189)). The local tool should consume captured
exports, not credentials or a live database.

## Retention, denominator, and audit traps

- Audit retention defaults to 90 days, with failure/security/configuration,
  dead-letter, uncertain and reconciled effects eligible for an extended 365-day
  window ([crates/controller-protocol/src/audit.rs:11-15](crates/controller-protocol/src/audit.rs#L11); [src/main.rs:89-111](src/main.rs#L89)). Controller maintenance applies the same configurable policy
  ([crates/github-pr-controller/src/lib.rs:420-485](crates/github-pr-controller/src/lib.rs#L420)). Archive evidence exports before retention cuts; a later report must say its coverage is unavailable rather than treat pruned evidence as no event.
- Completed webhook deliveries and runtime receipts have separate, short-lived
  operational retention; do not use their absence as a denominator. The audit
  journal is the intended historical source.
- `github_writes` has `done_at` but its `mark_write_done` does not itself create
  the provider-success journal event; success is recorded by the drain before
  it marks done ([crates/github-pr-controller/src/lib.rs:2257-2263](crates/github-pr-controller/src/lib.rs#L2257); [crates/github-pr-controller/src/lib.rs:2823-2855](crates/github-pr-controller/src/lib.rs#L2823)). A crash between those operations makes the evidence incomplete, not a success or failure.
- A terminal OCP session is not automatically a GitHub projection. The controller
  only queues writes after processing the terminal event, and an ask session
  intentionally has no review round/findings/status ([crates/github-pr-controller/src/lib.rs:2132-2147](crates/github-pr-controller/src/lib.rs#L2132)). Keep asks and council rounds separate or label their required-write sets.
- Finding counts and colours are volume, not quality. ADR 021 explicitly warns
  that adoption/quality needs sample-size honesty and cannot measure recall
  ([docs/adr/021-review-effectiveness-feedback-loop.md:57-70](docs/adr/021-review-effectiveness-feedback-loop.md#L57)). Show `n`, unknowns, and annotation/escape confirmation coverage; do not produce a single score or auto-tuning recommendation.
- The SQLite and Postgres controller implementations intentionally mirror the
  same product tables behind `ProductStore` ([crates/github-pr-controller/src/store.rs:359-364](crates/github-pr-controller/src/store.rs#L359)). Evidence schema checks should be backend-neutral; never assume SQLite rowid order is an event-time order.

## High-signal later validation commands

These are proposed future validation commands, not run during this codewalk:

```sh
cargo test -p github-pr-controller audit
cargo test -p github-pr-controller review_findings
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features --workspace -- -D warnings
```

The future local report should additionally run fixture tests without a network
or database: a complete known week, no-cost/all-unknown week, a waived/dismissed
finding with no validity annotation, a candidate-but-unconfirmed escape, a
partial projection, a terminal failed projection, a supersession, a missing
audit page, mixed seconds/milliseconds, and a Taipei Sunday/Monday boundary.

## Blocking owner choices

None for the codewalk or the conservative first report. The proposed defaults
make all unresolved evidence explicit. Before any metric is represented as a
product KPI, the owning human must ratify the precise required-write set for a
“completed GitHub projection” per session reason; until then the report should
show only per-write latency and mark aggregate projection completion `unknown`.

Self-check: Frozen scope, output, model, effort and authority declared.
