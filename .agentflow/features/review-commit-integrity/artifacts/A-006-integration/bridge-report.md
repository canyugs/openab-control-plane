* _2026-09-11 18:23:43 (gpt-5.6-luna/max)_

# bridge

Implemented the approved standalone, read-only capture/prepare/run bridge at
the exact base `a4017b87f6b7357ef9c3cba63ff7052e95ce7532`.

Changed paths only:

- `scripts/review_evaluation_bridge.py`
- `tests/test_review_evaluation_bridge.py`
- `docs/evaluation-integration.md`
- `bridge-report.md`

The bridge signs the controller's exact v1 GET target, retains raw successful
pages and hashes, disables redirects, requires HTTPS except explicitly allowed
loopback HTTP tests, rejects unsafe paths, and never serializes the observer
secret. Preparation revalidates the capture, clean Git checkout, requested
full revision/base, existing evaluator input schemas, finding scope and
identity, title-only claims, frozen-source evidence, and weekly bundle. Run
revalidates provenance, invokes the existing evaluator seam, retains failed or
partial artifacts, and gives the weekly reporter only verified evaluation
output.

Checks actually run:

- Red phase: the initial `python3 -m unittest tests.test_review_evaluation_bridge` failed at import because the bridge module did not yet exist; the same command passed after implementation.
- `python3 -m unittest tests.test_review_evaluation_bridge` — 12 tests passed;
  1 loopback socket test skipped because this sandbox forbids listener sockets.
  The deterministic transport covered the same signature/query contract.
- `python3 -m py_compile scripts/review_evaluation_bridge.py tests/test_review_evaluation_bridge.py` — passed.
- `python3 -m unittest tests.test_review_model_evaluation tests.test_review_model_oci_executor tests.test_review_round_weekly_report tests.test_evaluation_package` — 73 tests passed; 1 existing package-launcher test skipped because this process runs as root.
- `python3 scripts/review_evaluation_bridge.py --help` — passed.

No live controller, provider, model, Docker, scheduler, credential, commit,
push, publication, or production operation was performed. Existing regression
output included only macOS Git temporary-directory warnings; no test failed.

Fixed limits are 16 MiB and 15 seconds per HTTP response/request, findings
limit 5,000, audit limit 500, and audit page cap 100 by default (10,000
maximum). Source defaults remain 10,000 files, 2 MiB per file, 32 MiB total,
and 8 MiB diff. Findings at the endpoint boundary, audit page caps, and
ambiguous non-empty missing cursors are partial. Captures and weekly bundles
are non-atomic and session-scoped retained API observations; they do not claim
global, whole-week, human-truth, provider-identity, recall, reliability, or
actual-cost completeness. An empty Rust terminal audit page may omit its
`None` cursor.

Self-check: Scope, source, model/effort, authority and write boundary frozen.
