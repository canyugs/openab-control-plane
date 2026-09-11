# Read-only evaluation bridge

`scripts/review_evaluation_bridge.py` connects one controller observation
capture to the existing offline evaluator and weekly reporter. It is a
standalone integration seam: it does not change Rust/OCP behavior, add an API,
contact GitHub or a model during capture/prepare, or publish a result.

## Stages

Capture is the only network stage:

```text
OCP_EVAL_OBSERVER_SECRET=... \
python3 scripts/review_evaluation_bridge.py capture \
  --controller-url https://controller.example.invalid \
  --repository owner/repo \
  --pr 123 \
  --session SESSION_ID \
  --output /path/to/new-capture
```

The observer secret is read only from `OCP_EVAL_OBSERVER_SECRET`. Each GET is
signed with the controller's exact v1 payload:

```text
v1\n<TIMESTAMP_SECONDS>\nGET\n<PATH_AND_QUERY>
```

The headers are `x-canary-audit-timestamp` and
`x-canary-audit-signature-256: sha256=<hex-hmac>`. The findings request is
fixed at `/api/v1/review/findings?repo=...&pr=...&limit=5000`. The audit
request is fixed at
`/api/v1/audit/events?session_id=...&until=<capture_start_ms>&limit=500`,
then appends the returned cursor verbatim. Redirects, URL userinfo, query,
and fragments are rejected. `http://` is accepted only for a loopback host
when `--allow-local-http` is explicitly supplied. No bearer credential is
sent.

The capture keeps the exact successful response bytes under `raw/` and records
their byte counts and SHA-256 hashes in `capture.json`. It records the fixed
scope, `capture_start_ms`, offset-bearing Taipei `snapshot_at`, page/query
targets, cursors, bounds, and coverage. Findings at the 5,000-row boundary
are `partial`; the findings endpoint has no cursor. A repeated audit cursor is
an error. After a successful Rust `AuditEventPage` shape check, an omitted or
explicit `null` `next_cursor` means that the endpoint is exhausted, regardless
of the event count, including an exactly full page. An audit page cap remains
`partial`. Terminal completeness applies only to this exact session query at
the capture cutoff; it does not establish controller-retention completeness or
whole-week completeness. The capture is non-atomic and session-scoped.

Preparation uses a clean checkout and immutable full SHAs:

```text
python3 scripts/review_evaluation_bridge.py prepare \
  --capture /path/to/capture \
  --repo /path/to/clean-checkout \
  --revision 0123456789abcdef0123456789abcdef01234567 \
  --base fedcba9876543210fedcba9876543210fedcba98 \
  --output /path/to/new-prepared
```

The optional `--product PRODUCT_JSON` input must contain only the selected
repository, PR, session, and full revision. Supplied product tables are
labelled partial because this bridge has no independent completeness proof;
absent tables are empty with unknown coverage. It never invents targets,
rounds, writes, receipts, human annotations, or cost data.

Preparation calls the released evaluator's existing Git helpers for Git config,
clean-tree, full-SHA resolution, and source-packet construction. It snapshots
the requested revision/base, not checkout `HEAD`, and retains incomplete-source
omissions. Findings are selected only when repository, PR, session, and
case-normalized `head_sha` all match. Finding identity is
`<session_id>:<stored_numeric_id>`; `stable_id` is not used as identity.

The ledger title is copied literally into both `title` and `claim`, with
`claim_provenance: ledger_title_only`. Status is not interpreted as accuracy.
Each accepted finding's evidence is the exact regular UTF-8 whole file from
the frozen source packet, with its source SHA-256 and actual full-file range.
Malformed selected rows and conflicting duplicate identities block the
preparation; exact duplicate rows may be deduplicated but are counted. Zero
selected findings is also an explicit blocked preparation, never a passing
zero-denominator evaluation.

The prepared directory contains the copied hashed capture, source packet,
core evaluator inputs, and a weekly bundle:

```text
prepared/
  preparation.json
  capture/capture.json
  capture/raw/*.json
  source-packet.json
  findings.json
  evidence/manifest.json
  product-input.json             # only when --product was supplied
  weekly-bundle/
    product.json
    audit.ndjson
    evidence-manifest.json
```

The weekly bundle uses the existing
`review-round-weekly-evidence/v1` format. Its findings table comes from the
capture. The audit envelope remains the Rust `AuditEventPage.events` shape.
Its snapshot is exactly capture `snapshot_at`, and its Taipei ISO week is
derived from that timestamp. The bundle is checked through the existing weekly
reporter before preparation is made visible.

Run verifies the copied capture, all artifact hashes, core inputs, source
packet, selection derivation, product scope, and the clean checkout again:

```text
python3 scripts/review_evaluation_bridge.py run \
  --prepared /path/to/prepared \
  --repo /path/to/clean-checkout \
  --models /path/to/models.json \
  --environment /path/to/environment.json \
  --output /path/to/new-run
```

`--environment` is optional. Its source limits must remain the evaluator
defaults used by preparation. The existing evaluator is called through its
Python API with the prepared revision, base, findings, and evidence paths;
model authentication and the model/environment allowlists remain owned by the
existing core. The checkout is never modified. Evaluation artifacts are kept
under `evaluation/`, including failed or partial attempts. The weekly report
is written under `weekly-report/` only after evaluation output passes the core
artifact verifier. A failed or unverified evaluator result remains
`not_scoreable`; an exit status or returned object alone is not a quality pass.

All output directories must be new, and output/input ancestor overlap and
symlink components are rejected before writes. CLI status is compact JSON.
Capture returns `complete` or explicit `partial`; prepare returns `ready` or
blocked (blocked exits 2); run reports `complete`, `partial`, or `failed`, and
quality is `not_scoreable` unless the verified weekly evaluation says it is
available.

## Current bounds and truth limits

The bridge bounds each HTTP page at 16 MiB and each request at 15 seconds;
findings use limit 5,000, audit uses limit 500, and audit pagination defaults
to 100 pages (maximum 10,000). The released source-packet defaults remain:

```text
max_files       10,000
max_file_bytes  2 MiB
max_total_bytes 32 MiB
max_diff_bytes  8 MiB
```

An API response that is complete for the selected session is not evidence of
global controller retention or a whole-week population. This bridge reports
session-scoped retained API coverage only. The API capture does not provide a
full product-table cohort, so zero eligible sessions is not zero reviews.
Missing product, human, and cost tables stay unknown. Cost coverage is unknown;
an empty `per_currency` object must not be interpreted as known actual billing,
irrespective of an empty-cohort status label. Model support on positive
observations is not defect precision or human-confirmed usefulness. No
full-week reliability, recall, human truth, provider identity, or actual cost
metric is claimed.

Runnable offline checks from the repository root are:

```text
python3 -m unittest tests.test_review_evaluation_bridge
python3 -m unittest tests.test_review_model_evaluation tests.test_review_model_oci_executor tests.test_review_round_weekly_report tests.test_evaluation_package
python3 -m py_compile scripts/review_evaluation_bridge.py tests/test_review_evaluation_bridge.py
```

The socket-based integration test needs permission to bind a loopback test
listener; in a sandbox that denies listener sockets it is skipped, while the
same signature/query behavior is covered by the deterministic local transport
seam.
