# Offline review-round weekly report

`scripts/review_round_weekly_report.py` reads one operator-captured evidence
bundle and writes a deterministic Markdown/JSON snapshot. It is a reporting
observer only: it makes no database, network, GitHub, model, OCI, scheduler,
credential, verdict, retry, routing, or provider call.

## Command

```text
python3 scripts/review_round_weekly_report.py \
  --bundle /path/to/review-round-weekly-evidence \
  --week 2026-W37 \
  --as-of 2026-09-09T08:10:00+08:00 \
  --output /path/to/new-output \
  [--evaluation-root /path/to/verified-evaluation]
```

`--week` is the Taipei ISO week of the offset-bearing `--as-of`. The manifest
`snapshot_at` must be the same instant. The output directory must be empty or
new and must not contain, be contained by, or resolve through a symlink to the
bundle or evaluation root. Existing report files are never overwritten.

## Bundle

```text
bundle/
  evidence-manifest.json
  audit.ndjson
  product.json
  human.ndjson       # optional; coverage must be unknown when absent
  cost.ndjson        # optional; coverage must be unknown when absent
```

The manifest is `review-round-weekly-evidence/v1` and contains a bundle ID,
`snapshot_at`, `timezone: "Asia/Taipei"`, file SHA-256s, audit cursor metadata,
metric coverage, and capture spans. Every source span has `start`, `end`, and
`coverage` (`complete`, `partial`, or `unknown`). The product table coverage is
declared separately for all five tables:

```json
{
  "coverage": {
    "audit": "complete",
    "product": "complete",
    "product_tables": {
      "session_targets": "complete",
      "review_rounds": "complete",
      "review_findings": "complete",
      "github_writes": "complete",
      "runtime_event_receipts": "complete"
    },
    "human": "unknown",
    "cost": "unknown"
  },
  "sources": {
    "audit": {"coverage": "complete", "start": "…+08:00", "end": "…+08:00"}
  },
  "audit_cursor": {
    "first_cursor": "…",
    "last_cursor": null,
    "page_count": 1,
    "final_null_cursor": true
  }
}
```

`sources` has the same span shape for `audit`, each product table, `human`, and
`cost`. A complete audit requires a positive page count and a final null
cursor. Hashes prove byte identity, not completeness or truth. The report
discloses that the cross-source capture is non-atomic. It does not use
`created_at` to reconstruct mutable product history.

`audit.ndjson` preserves one flattened raw `AuditEventRecord` per line:
`seq`, `version`, `event_id`, `event_key`, `occurred_at`, `recorded_at`,
`service`, `kind`, `outcome`, `correlation`, `detail`, and optional envelope
fields. Audit timestamps are integer milliseconds. `recorded_at` is the audit
cutoff clock; records after `snapshot_at` are excluded.

`product.json` maps the five source tables to arrays with their stored source
columns. Controller product timestamps (`created_at`, `claimed_at`, `done_at`)
are integer seconds. `runtime_event_receipts.occurred_at` remains envelope
time and is not converted; its integer `received_at` seconds timestamp is the
snapshot admission cutoff.

## Cohort

The cohort uses only `kind: "action.accepted"` and `outcome: "accepted"`.
Each accepted record must join to exactly one successful
`kind: "action.completed"`, `outcome: "succeeded"`, through the exact
`correlation.delivery_id`, `correlation.action_id`, and
`correlation.trigger_ref` values. Missing or ambiguous joins remain in the
accepted unknown denominator. `ingress.accepted` is plan-only and excluded.

Resolved sessions are deduplicated across the whole captured span before week
assignment; the earliest accepted event determines the week, so a later-week
redelivery cannot create a duplicate. Exact duplicate natural identities are
deduplicated. Conflicting identities remain unknown. `reason: "ask"` is
excluded only when it is explicitly present in the joined session target;
missing target data cannot prove an ask.

## Source-bound reliability

Only exact terminal write kinds are considered: `comment`, `status`, and
`review` for a normal/diagnostic round, and `comment_abandon` for an
abandonment. `comment_open` and `decision_status:*`, `decision_review:*`, and
`decision_comment:*` are deliberately excluded from the terminal required set.

Every delivery receipt is bound by all of:

- `event.correlation.session_id` and the exact `event.correlation.write_id`;
- the exact `github_writes.payload_json` UTF-8 SHA-256;
- the exact `detail.operation`; and
- the persisted session/write row.

Enqueue audit detail is not a receipt. A valid source receipt is exactly a
`github.write.succeeded`/`succeeded` or `github.write.reconciled`/`reconciled`
event with `detail.request_sha256` and `detail.provider_receipt`.

Normal verified approval or change-request reliability requires an immutable
full target SHA equal to `verified_commit_id`, and bound successful/reconciled
comment, status, and review writes. Status payload/provider SHA and
`commit_id` must agree with that target. The formal review payload carries
`event: "APPROVE"` or `"REQUEST_CHANGES"`; the actual provider receipt carries
`review_id`, `state: "APPROVED"` or `"CHANGES_REQUESTED"`, `commit_id`, and
`reconciled`. A provider `event` field is not accepted as review evidence.

Only these controller-supported diagnostic dispositions establish a diagnostic
write set: `unparseable`, `insufficient_valid_reviewers`, `missing_target`,
`invalid_target`, `missing_reviewed_sha`, `invalid_reviewed_sha`, and
`reviewed_sha_mismatch`. A diagnostic always requires a bound visible comment,
plus a bound `error` status exactly when the target is a valid full SHA.
`legacy_unverified` alone is not diagnostic, and unsupported dispositions stay
unknown.

An explicit `session.superseded` has precedence over every other category.
Timeout and supersession tombstone visibility requires a real persisted
`comment_abandon` row, its exact payload digest/write binding, and a non-null
provider `comment_id`. A successful null `comment_id` is a no-op, not a public
tombstone. The mutually exclusive categories are `superseded`, `reliable`,
`visible_failure`, and `pending_or_unknown`.

Latency starts at the accepted audit `occurred_at`. For each required terminal
write, the earliest valid proof is selected; repeated success/reconciliation
observations of the same write are deduplicated, while conflicting provider
evidence remains unknown. The latest selected required terminal proof ends the
measurement. Opening, decision, earlier, and unrelated late receipts do not
participate. Original receipts produce exact observed latency; a
reconciliation-only proof is labelled `reconciliation_upper_bound`. Negative
or impossible ordered times are `clock_invalid`.

## Human and cost metrics

Human annotations require explicit annotator, version, timestamp, complete
session/finding/repository/PR/head-SHA scope, verdict, and evidence reference.
Escapes additionally require confirming human, reviewed-window ID, status,
timestamp, complete scope, version, and evidence reference. Rows after the
cutoff are excluded. Exact duplicates dedupe; conflicting natural identities
become unknown. Confirmed escapes and reviewed-window coverage are reported
separately; no recall or submitted-record escape rate is inferred.

Costs are integer source minor units only. Each eligible session must have
provider-reconciled rows with currency, compatible minor-unit definition,
reconciliation time/reference, completeness, and all-attempt/retry coverage.
Missing sessions, incompatible currencies/units, and incomplete attempt
coverage preserve `totalunknown`. A partial known subtotal remains separate
from the unknown total. Currency-specific sums are never combined, estimated,
priced, allocated, or converted to floating actual dollars.

## Model evaluation

With `--evaluation-root`, the report calls the named
`verify_evaluation_artifacts(root)` seam from the concurrent core worker. It
does not read `summary.json` directly. A missing helper is reported as an
explicit `dependency_unavailable` evaluation status; it is not permission to
trust raw summaries. Injected verifier results are identity-deduplicated;
conflicting duplicate identities fail. Evaluation artifact identity hashes
are included in `report_id`. Partial or failed evaluations are shown for
coverage but do not produce a quality score. Usefulness, disagreement,
validation classes, omission candidates, and human-confirmed escapes retain
separate explicit denominators from delivery reliability and human review.
The disagreement numerator counts items whose two valid typed judge verdicts differ, and its denominator counts only items with exactly two valid assessments; synthesis prose is qualitative, and a missing legacy denominator remains unknown.

## Output and checks

The report has no generated timestamp. Its ID is the SHA-256 of the frozen
definition version, week, exact `--as-of`, manifest bytes hash, and evaluation
identity digest. Files are named:

```text
review-round-weekly-{week}-snapshot-{UTC-snapshot_at}-{report_id[:12]}.md
review-round-weekly-{week}-snapshot-{UTC-snapshot_at}-{report_id[:12]}.json
```

Run the fixture suite and compile the changed module:

```text
python3 -m unittest tests.test_review_round_weekly_report
python3 -m compileall -q scripts/review_round_weekly_report.py
```

The suite exercises real source-shaped approve and request-changes receipts,
diagnostics, legacy/unsupported dispositions, exact binding corruption,
opening/decision exclusions, timeout and supersession tombstones, action joins
and cross-week deduplication, cutoff/clocks, human/cost unknowns, injected
evaluation verification, identity conflicts, deterministic output, and
input/output overlap refusal. A real integrated evaluation-helper journey is
pending until the concurrent core helper is available.
