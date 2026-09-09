# Offline review-round weekly report

`review_round_weekly_report.py` reads one immutable operator capture and
writes a deterministic Markdown/JSON snapshot. It makes no database, network,
GitHub, scheduler, controller, price-table, or model call. Its output cannot
change verdicts, retries, writes, routing, or roster selection.

## Command

```text
python3 scripts/review_round_weekly_report.py \
  --bundle /path/to/review-round-weekly-evidence \
  --week 2026-W37 \
  --as-of 2026-09-09T08:10:00+08:00 \
  --evaluation-root /path/to/evaluation-output \
  --output /path/to/new-report-output
```

`--evaluation-root` is optional and is read only. The output directory must be
new or empty; existing report files are never overwritten. `--as-of` must be
offset-bearing, equal to the manifest timestamp, and its Taipei ISO week must
equal `--week`.

## Bundle contract

```text
bundle/
  evidence-manifest.json
  audit.ndjson
  product.json
  human.ndjson       # optional
  cost.ndjson        # optional
```

The manifest has this exact top-level shape:

```json
{
  "schema_version": "review-round-weekly-evidence/v1",
  "bundle_id": "opaque-bundle-id",
  "snapshot_at": "2026-09-09T08:10:00+08:00",
  "timezone": "Asia/Taipei",
  "coverage": {"audit": "complete", "product": "complete", "human": "unknown", "cost": "unknown"},
  "audit_cursor": {"first_cursor": "0", "last_cursor": null, "page_count": 1, "final_null_cursor": true},
  "files": {
    "audit.ndjson": {"sha256": "<64 lowercase hex>", "record_count": 1},
    "product.json": {"sha256": "<64 lowercase hex>"}
  }
}
```

Each source declares `complete`, `partial`, or `unknown` coverage. Hashes bind
captured bytes, not the truth or historical completeness of mutable rows.
Optional files, when present, also require a manifest SHA-256 and record count.

`audit.ndjson` is flattened cursor output: one record per line with the exact
envelope fields below. The cursor itself is represented by the manifest’s
capture metadata when available; no field is reconstructed by this tool.

```json
{
  "seq": 1,
  "version": 1,
  "event_id": "bounded-source-id",
  "event_key": "bounded-source-key",
  "occurred_at": 1788912000000,
  "recorded_at": 1788912000000,
  "service": "controller",
  "kind": "action.accepted",
  "outcome": "accepted",
  "caused_by": null,
  "correlation": {"session_id": "session-1"},
  "actor": null,
  "target": null,
  "detail": {},
  "error": null
}
```

Audit `occurred_at` and `recorded_at` are integer milliseconds. Records with
`recorded_at` after the cutoff are excluded and counted. Source IDs are
bounded opaque strings; they are escaped before drilldown rendering.

`product.json` is an object mapping these five persisted source tables to raw
row arrays (the audit event stream is the separate sixth source):

```json
{
  "session_targets": [],
  "review_rounds": [],
  "review_findings": [],
  "github_writes": [],
  "runtime_event_receipts": []
}
```

The required row columns are exactly the source columns captured by the
controller: `session_targets` has `session_id, repo, pr_number, head_sha,
created_at, reason, required_valid_reviewers`; `review_rounds` has `id, repo,
pr_number, round, session_id, head_sha, comment_id, decision, red, yellow,
green, created_at, verified_commit_id, integrity_disposition`;
`review_findings` has `id, session_id, repo, pr_number, stable_id, severity,
status, head_sha, created_at, raised_by, angle`; `github_writes` has `id,
session_id, kind, payload_json, state, attempts, created_at, claimed_at,
done_at`; and `runtime_event_receipts` has `event_id, body_sha256, event_type,
session_id, occurred_at, received_at`. Product controller timestamps are
seconds except runtime `occurred_at`, which is copied envelope milliseconds;
the report never converts it or uses it as an admission clock.

`github_writes.payload_json` is preserved as its exact stored string. Receipt
audit `detail.request_sha256` must equal SHA-256 of those exact UTF-8 bytes.
An enqueue event alone is not a delivery receipt.

Optional `human.ndjson` annotations require `annotator_id, version, session_id,
finding_id, repo, pr_number, head_sha, verdict, evidence_reference`, and an
`observed_at`/`annotated_at` timestamp. `verdict` is
`valid_useful|valid_not_useful|invalid|unknown`. A complete finding scope is
required; conflicts remain unknown. Escape records require
`type:"escape", confirming_human_id, confirmed_at, version,
reviewed_window_id, evidence_reference`, and `status` of
`confirmed_escape|not_escape|unknown`.

Optional `cost.ndjson` rows require `session_id, currency, amount_minor`, a
minor-unit definition/reference (`source_minor_unit`,
`minor_unit_definition`, or `source_reference`), a reconciliation timestamp
(`reconciliation_time` or `reconciled_at`), `reconciliation_reference`,
`completeness` (`complete|partial|unknown`), and `attempt_coverage`
(`all_attempts_and_retries|partial|unknown`). Only provider-reconciled
per-session values count. Partial known amounts are shown as partial known
minor units with an unknown total; no price table, estimate, allocation, or
cross-currency arithmetic is performed.

## Cohort and source-bound classification

The cohort is formed from unique `kind: action.accepted` and
`outcome: accepted` events in the Taipei ISO week after session deduplication.
`ingress.accepted` is counted as plan-only and excluded. The earliest
accepted event for a session determines its week. An action without a
resolvable session, target, or one correlated `action.completed` remains in an
explicit unknown/pending bucket; missing target data cannot prove that it was
an `ask`. `session_targets.reason:"ask"` is excluded only when explicitly
known.

For each eligible session the terminal rules are mutually exclusive:

1. An admitted `runtime_event_receipts.event_type: session.superseded` is
   `superseded`; visible tombstone status is reported separately.
2. A complete verified approve/request-changes round is `reliable` only when
   its bound successful/reconciled comment, expected status, and formal review
   receipts are all present and source-bound.
3. A supported unparseable/integrity/reviewer diagnostic is `visible_failure`
   only when comment is visible and the expected error status exists for a
   valid full-40 target SHA. A timeout is visible failure only with a bound
   non-null `comment_abandon` provider comment ID.
4. Everything else is `pending_or_unknown`. A null comment-abandon ID is a
   successful no-op, not a public tombstone. `legacy_unverified` alone does
   not prove that a diagnostic was planned. Opening-comment and author-decision
   writes are not terminal-round requirements.

Missing/extra/conflicting write rows, wrong payload digest, wrong provider
state/event/commit, and incomplete receipt sets do not become a failure
claim. Failed or retried writes are delivery evidence only.

Trigger-to-terminal-projection latency starts at the accepted audit
`occurred_at` and ends at the latest bound projection receipt. Original
receipts are exact observed latency. Reconciliation-only evidence is labelled
`reconciliation_upper_bound`; missing or negative/ordered clocks are unknown
or clock-invalid. Runtime receipt `received_at` is the cutoff admission clock.

## Evaluation join

When `--evaluation-root` is provided, the report reads `summary.json` at that
root or one level below and exposes model-supported/refuted/unresolved counts,
validation categories, and automatic omission candidates under a separate
`evaluation` object. It makes no model call and never combines those counts
with controller reliability, human annotations, confirmed escapes, or cost.
No recall or human-correctness claim is inferred from model agreement.

## Output

The report has no `generated_at`. Its ID is:

```text
sha256(definition_version || week || snapshot_at || manifest_sha256)
```

Files are named exactly:

```text
review-round-weekly-{week}-snapshot-{UTC-snapshot_at}-{report_id[:12]}.md
review-round-weekly-{week}-snapshot-{UTC-snapshot_at}-{report_id[:12]}.json
```

The JSON includes source hashes, capture coverage, accepted/excluded/unknown
denominators, all zero-valued operational categories, safe sorted session
references, latency qualifications, human/cost metric-specific unknowns, and
separate evaluation metrics. Markdown is a deterministic rendering of the
same validated snapshot.

## Checks

```text
python3 -m unittest tests.test_review_round_weekly_report
python3 -m compileall -q scripts/review_round_weekly_report.py
```

The fixture suite covers accepted/ask/plan-only cohort rules, Taipei cutoff
and clock units, payload-digest receipt binding, reliable and diagnostic
projections, supersession, timeout tombstone/no-op, missing rounds, human and
cost unknowns, separate evaluation denominators, deterministic naming, and
overwrite refusal.
