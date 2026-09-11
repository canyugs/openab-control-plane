* _2026-09-09 13:52:56 (gpt-5.6-terra/high)_

# Stage 2 corrected design — offline weekly council report

## Authority, scope, and future change set

This is an implementation-ready, read-only Stage 2 design correction. It authorizes no
runtime, schema, Rust, network, scheduler, exporter, service, credential, commit, or
production action. The report tool only reads an operator-captured local bundle and writes
local Markdown and JSON; it never influences verdicts, retries, writes, routing, or roster
selection (R-18 / INV-7). Stage 3 and comparison-arm work are out of scope.

The exact future paths are unchanged:

- `scripts/review_round_weekly_report.py`
- `tests/test_review_round_weekly_report.py`
- `docs/review-round-weekly-report.md`

The Python-stdlib module is the single deep module: its interface is input bundle, required
`--week`, required offset-bearing `--as-of`, and a new output directory; validation, joins,
classification, and rendering stay behind that seam. No source/export adapter is added.

## Source facts that constrain the design

`crates/controller-protocol/src/audit.rs` defines a raw `AuditEvent`: `version`, `event_id`,
`event_key`, millisecond `occurred_at`/`recorded_at`, `service`, `kind`, `outcome`, optional
`caused_by`, `correlation`, optional `actor`, `target`, `error`, and JSON `detail`; cursor
pages contain `{seq, ...event}` and `next_cursor`. IDs/event keys are bounded source strings
(1..=256 bytes), not restricted identifiers and never executable.

`crates/github-pr-controller/src/store/sqlite.rs` and
`crates/github-pr-controller/src/store/postgres.rs` persist product
timestamps in seconds. `enqueue_write` records `github.write.enqueued` detail as only
`{"operation": kind}`. It does not contain the payload. The durable `github_writes` row has
the payload, and `perform_write_with_receipt` audits attempted/succeeded/reconciled writes
with `operation`, `request_sha256`, expected provider identity, and a provider receipt.
`crates/github-pr-controller/src/lib.rs` emits correlated `action.accepted`/`action.completed`
and implements `dispatch_terminal`/`perform_write_with_receipt`; together with
`crates/github-pr-controller/src/closing.rs`, it shows that `comment_abandon` returns a null
`comment_id` when no opening comment existed. That is a successful no-op, not a visible
tombstone. `dispatch_terminal` records a round only for normal or insufficient-reviewer
terminal handling; `dispatch_abandoned` handles runtime `session.superseded`/`session.timeout`,
so either can have no `review_rounds` row.

## Operator capture contract

The operator makes an otherwise immutable local bundle. It contains no bodies, tokens,
credentials, or unneeded free text in its report output; the tool reads JSON only and makes
no database or network connection.

```text
bundle/
  evidence-manifest.json                 required
  audit.ndjson                           required, flattened cursor pages
  product.json                           required, one current-snapshot table export
  human.ndjson                           optional
  cost.ndjson                            optional
```

`audit.ndjson` has one raw `AuditEventRecord` per line: `{seq, version, event_id, event_key,
occurred_at, recorded_at, service, kind, outcome, caused_by?, correlation, actor?, target?,
detail, error?}`. The operator fetches every page from `/api/v1/audit/events`, follows each
returned cursor until `next_cursor` is absent, writes records exactly once, and records the
first/last cursor, page count, and final-null-cursor observation in the manifest. Flattening
does not rename or discard envelope fields. Audit timestamps remain milliseconds.

`product.json` is a single point-in-time best-effort export, with these exact source columns
(SQLite types shown; Postgres uses equivalent `BIGINT`/`TEXT` columns):

| table | exported columns |
| --- | --- |
| `session_targets` | `session_id, repo, pr_number, head_sha, created_at, reason, required_valid_reviewers` |
| `review_rounds` | `id, repo, pr_number, round, session_id, head_sha, comment_id, decision, red, yellow, green, created_at, verified_commit_id, integrity_disposition` |
| `review_findings` | `id, session_id, repo, pr_number, stable_id, severity, status, head_sha, created_at, raised_by, angle` |
| `github_writes` | `id, session_id, kind, payload_json, state, attempts, created_at, claimed_at, done_at` |
| `runtime_event_receipts` | `event_id, body_sha256, event_type, session_id, occurred_at, received_at` |

The table export preserves the stored `payload_json` bytes/text and the write `id`; the tool
calculates SHA-256 over that exact UTF-8 payload and requires it to match the receipt audit's
`detail.request_sha256` where such a receipt is used. This binds an immutable request to a
receipt; an enqueue audit alone cannot. Product timestamps are seconds and are labelled as
such, never mixed with audit milliseconds. `review_rounds` has neither `terminal_event` nor
`terminal_branch`; no input may invent either.

The manifest declares schema version, bundle ID, `snapshot_at`, timezone `Asia/Taipei`, each
file's SHA-256/record count, audit cursor completion, per-source capture start/end, and
coverage (`complete|partial|unknown`) for audit, product tables, human, and cost. Hashes bind
bytes, not the truth or completeness of their contents. Aliases may be supplied only as
display/grouping hints; they do not establish identity, authority, or a missing join.

`snapshot_at` is the operator's capture cutoff and must equal `--as-of`; it is not a generated
time. Audit entries are included only when `recorded_at <= snapshot_at`; human and cost entries
are included only when their declared observation/reconciliation time is at or before it.
Product rows are mutable current snapshots: their `created_at` must not be used to reconstruct
or promise historical state at `snapshot_at`. The report states the non-atomic cross-source
capture span. Missing historic state, unfinished cursors, partial/unknown coverage, or an
inconsistent join is unknown and prevents a reliability claim.

Human records join on `session_id` plus the actual finding row reference/`stable_id`, repo, PR,
and head SHA; explicit annotations have `valid_useful|valid_not_useful|invalid|unknown`.
Escape records carry a reviewed-window identity, explicit `confirmed_escape|not_escape|unknown`,
confirmation time, and source reference. Dismissal/waive is not an annotation. Report confirmed
escape counts and reviewed-window coverage; never call a fraction of submitted escape records a
recall or escape rate.

Each cost record supplies `session_id`, `currency`, integer `amount_minor`, source minor-unit
definition/reference, reconciliation time/reference, plus `completeness`
(`complete|partial|unknown`) and `attempt_coverage`
(`all_attempts_and_retries|partial|unknown`). Per-session coverage is required even for zero.
Only provider-reconciled per-session source minor units count; no pricing table/API, allocation,
estimate, or cross-currency arithmetic. A partial known amount is reported as a partial known
amount with unknown total, never as full actual cost.

## Cohort, binding, and classifications

The cohort is unique successful `action.accepted` records in the Taipei ISO week, joined through
delivery/action/trigger correlation to one `action.completed` session. `ingress.accepted` is
plan-only and excluded; retries/redeliveries deduplicate at session scope. An ambiguous or
missing join remains counted as pending/unknown coverage, not dropped. Show bounded, escaped
raw repo, PR, session, finding, event, and source-row references for drilldown; omit bodies,
tokens, credentials, errors, and unneeded free text.

For each joined session, derive only supported terminal evidence:

1. An explicit `runtime_event_receipts.event_type = session.superseded` classifies superseded;
   record `session.timeout` separately as timeout evidence. These receipts can exist without a
   round. A matching `comment_abandon` receipt is visibly tombstoned only when its bound
   `provider_receipt.comment_id` is non-null.
2. A round supplies `decision` and `integrity_disposition`. `verified` plus decision approve or
   request-changes is normal. `unparseable`, `insufficient_valid_reviewers`, or any non-verified
   integrity disposition is diagnostic evidence. Do not infer an absent round's terminal path.
3. The expected write set comes from the matching persisted `github_writes.payload_json`, its
   kind, and the above supported state, not a renamed field or merely observed successes.
   Normal verified requires bound successful/reconciled visible comment, status, and review;
   approve has status `success`, request-changes status `failure`. A diagnostic requires every
   actual planned diagnostic write, including a visible comment and its optional error status.
   Timeout requires a bound visible `comment_abandon`. Missing/extra/conflicting writes or
   receipt/payload binding is unknown (extras are diagnostic only).

Reliability categories are mutually exclusive, in this precedence order:

1. `superseded` for explicit supersession, with `tombstone_visible` reported separately.
2. `reliable` only for a complete, bound, verified normal approve or request-changes set.
3. `visible_failure` only for a fully delivered, bound integrity/unparseable/reviewer diagnostic
   set, or a timeout with a visible tombstone.
4. `pending_or_unknown` otherwise.

A diagnostic set is never also reliable. A failed/retried write is delivery evidence, not public
failure. A `comment_abandon` success with null `comment_id` has no public artifact and cannot
make a timeout visible_failure.

Latency starts at `action.accepted.occurred_at`. With original successful/reconciled receipt
times for every required projection, end at the latest and report the non-negative observed
duration. If only a reconciliation observation exists, report it as an upper-bound observed
timing, not original completion latency and not clock-invalid merely because an original time
is absent. Negative/impossible ordered original timestamps are clock-invalid; incomplete sets
are pending/unknown.

## Artifact, tests, and ledger

JSON includes definition version, Taipei week, `snapshot_at`, capture spans/coverage,
manifest/file hashes, category and denominator counts, per-session cost completeness/attempt
coverage, latency qualification, diagnostics, and sorted safe drilldowns. Markdown is its
deterministic rendering. `report_id = sha256(definition_version || week || snapshot_at ||
manifest_sha256)`; both files are named exactly
`review-round-weekly-{week}-snapshot-{UTC-snapshot_at}-{report_id[:12]}.{md,json}`. There is no
`generated_at`; identical validated bytes yield identical files and ID.

The fixture-first `unittest` suite creates every bundle and output directory in a temporary
directory. It covers verified approve/request-changes, all diagnostic paths, supersession,
timeout with and without visible tombstone, no round, conflicting bindings, partial cursor or
snapshot coverage, human states/escapes, partial/retry costs, reconciled-only latency, and
deterministic overwrite refusal. The documented later check runs that fixture suite, compiles
the script, and performs one real reusable PTY journey against a locally captured bundle per
the later Agentflow route; it does not cite nonexistent fixtures or rerun Rust tests.

| requirement | satisfied by |
| --- | --- |
| R-13 | Explicit human annotation states, confirmed-escape counts, and coverage—not structural proxies. |
| R-14 | Accepted-to-projection observed latency, with reconciliation upper bounds labelled honestly. |
| R-15 | Per-session reconciled minor-unit costs, currency-specific sums, completeness, and retry coverage. |
| R-16 | Source-bound, exclusive reliability classification and visible-artifact proof. |
| R-17 | Deterministic weekly Markdown/JSON snapshot with useful bounded drilldowns and provenance. |
| R-18 | Offline stdlib CLI only; it observes evidence and cannot alter controller behavior. |
| INV-7 | No report input/output changes execution, authority, verdict, write, retry, or routing. |

Self-check: Frozen scope, output, model, effort and authority declared.
