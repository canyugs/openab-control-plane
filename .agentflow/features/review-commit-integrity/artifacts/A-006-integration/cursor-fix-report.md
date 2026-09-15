* _2026-09-11 18:33:38 (gpt-5.6-luna/max)_

# cursor-fix report

Implemented the bounded cursor-contract correction only. After successful
`AuditEventPage` shape validation, both an omitted and an explicit `null`
`next_cursor` now terminate audit capture regardless of event count. The
capture retains the response bytes and their existing byte-count/SHA-256
ledger, records `final_null_cursor: true`, and makes no follow-up request.

The tests use full Rust-shaped `AuditEventRecord` rows for 22-event and
`AUDIT_LIMIT`-event terminal pages. They assert completion, no extra page, the
terminal metadata, omitted/null wire shape, and raw-byte/hash preservation.
Repeated cursors, page caps, invalid cursor/event shapes, and existing size,
hash, and scope checks were left unchanged.

Interpretation limits documented without changing core behavior:

- Endpoint exhaustion is complete only for the exact session query at the
  capture cutoff. It does not prove controller-retention completeness or a
  whole-week population.
- API capture lacks a full product-table cohort, so zero eligible sessions is
  not zero reviews.
- Cost coverage remains unknown. An empty `per_currency` object is not known
  actual billing, even when an empty-cohort status label is present.
- Model support on positive observations is not defect precision or
  human-confirmed usefulness.

Exact base: `6408b48980863aa1d336c25dc8188e3f1caeb6c9`.

Limits retained: `FINDINGS_LIMIT=5000`, `AUDIT_LIMIT=500`, default audit page
cap `100` (maximum `10000`), `MAX_PAGE_BYTES=16777216` (16 MiB),
`MAX_REQUEST_SECONDS=15.0`, `MAX_INPUT_BYTES=67108864` (64 MiB),
`MAX_SESSION_BYTES=512`, `MAX_REPOSITORY_BYTES=512`; source limits remain
`max_files=10000`, `max_file_bytes=2097152` (2 MiB),
`max_total_bytes=33554432` (32 MiB), and `max_diff_bytes=8388608` (8 MiB).

Changed paths only:

- `scripts/review_evaluation_bridge.py`
- `tests/test_review_evaluation_bridge.py`
- `docs/evaluation-integration.md`
- `cursor-fix-report.md`

Commands and results:

- `python3 -m unittest tests.test_review_evaluation_bridge` before the test
  change: 12 tests, 1 skipped, OK.
- The new regression test before the source fix: expected red; 2 subtests
  failed because omitted-cursor 22-event and 500-event pages returned
  `partial` instead of `complete`.
- `python3 -m unittest tests.test_review_evaluation_bridge.ReviewEvaluationBridgeTests.test_nonempty_rust_terminal_pages_without_cursor_are_complete_and_preserved`
  after the fix: 1 test, OK.
- `PYTHONDONTWRITEBYTECODE=1 python3 -m unittest tests.test_review_evaluation_bridge`
  after the fix: 13 tests, 1 skipped, OK.
- `PYTHONDONTWRITEBYTECODE=1 python3 -m py_compile scripts/review_evaluation_bridge.py tests/test_review_evaluation_bridge.py`:
  OK.
- `git diff --check`: OK.

No commit, push, publication, production operation, credential read, live
model/Docker/API/network operation, Agentflow invocation, or delegation was
performed. The parent-provided frozen pilot result (private repo: 2 findings,
22 events, source 50 files/238617 bytes; model complete; both supported and
executed_reproduced; usefulness split; weekly output verified) was not rerun.
Preparation, evaluation, run functions, and released core behavior were not
changed; parent owns function-hash comparison and prepared-input
revalidation.

Self-check: Scope, source, model/effort, authority and write boundary frozen.
