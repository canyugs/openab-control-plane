* _2026-09-10 00:09:46 (gpt-5.6-luna/max)_

# correction-core report

Source commit: `27497d41aef6c7c736733d4333a75defef386255`.
Authority: Design Go `1107f73567710159c298ff6a60df393e8e6b9775`.
Scope was limited to the core/adapters correction: `scripts/review_model_adapters.py`,
`scripts/review_model_evaluation.py`, `tests/test_review_model_evaluation.py`,
`tests/fixtures/model_evaluation/`, and `docs/model-evaluation.md`, plus this
report and `red-correction.patch`. No OCI or weekly-report file was changed.

The mandated A-001 artifacts were read, including `design-resolution.md`,
`transport-preflight-resolution.md`, `transport-preflight.json`,
`early-executor-inspection.md`, `correction-contract.md`, `design.md`,
`implementation-brief.md`, and `early-runtime-controls.json`.

## Implemented

- Claude defaults to the verified OAuth-safe direct CLI transport, with an
  explicit bare/API-key option. JSON result objects and event arrays are
  parsed for `structured_output`; unexpected executable, file, or MCP tool
  metadata is rejected. Requested identity, trusted assistant transport
  identity, auxiliary `modelUsage`, list-price estimates, and unknown actual
  billing remain separate.
- Subprocess capture is streaming and bounded. Timeout/output overflow kills
  the owned process group and failure records retain partial stdout/stderr and
  capture metadata. The child environment is an explicit runtime/locale and
  supported-auth allowlist; no credential extraction or arbitrary project
  environment is used. Codex remains visibly unavailable without an external
  confinement attestation.
- Validation receives the complete frozen source packet. Generation and OCI
  execution precede both judges for originals and blind-discovery candidates.
  The core accepts the frozen OCI `controls_passed`/nested `actual` seam,
  keeps raw OCI classification `unproven`, and promotes to
  `executed_reproduced`/`executed_refuted` only after source binding, matching
  valid `validation_verdict` assessments, and nonconflicting synthesis.
  Print-only, comment/path-only, disagreement, failed-control, and blocked
  paths remain unproven or blocked.
- Discovery is blind to original findings and author identity. Candidate
  reconciliation, candidate full records, anonymized synthesis, execution
  conflicts, and separate omission metrics are retained.
- `verify_evaluation_artifacts(root)` validates identities, schemas, safe paths,
  symlinks, every artifact hash, invocation captures/finals, validation plans
  and runs, and the summary digest before a completed result is returned.
  Completed model and validation work is reusable without new model/OCI calls;
  failed and interrupted attempts remain auditable with retry directories.

## Evidence

The tests-only red snapshot is `red-correction.patch`. The pre-implementation
test run failed on the newly required event-array transport, structural source
binding, retained failure captures, and restart/reuse behavior. The final
commands and results were:

```text
python3 -m unittest tests.test_review_model_evaluation
Ran 16 tests in 3.262s
OK

python3 -m unittest discover -s tests
Ran 24 tests in 3.275s
OK

python3 -m compileall -q scripts/review_model_evaluation.py scripts/review_model_adapters.py
OK

git diff --check
OK
```

Coverage is synthetic/offline: the Claude event-array fixture exercises OAuth
flags, safe environment filtering, backend metadata, auxiliary usage, and
list-price estimates; tests cover bounded timeout/overflow capture, source
binding negatives, validation-before-judge ordering, full original plus
omission journeys, safe refutation, disagreement, blocking, discovery failure
with zero findings, all-failed model status, immutable replay, and summary/
result/plan tamper rejection. Nested `actual` OCI observations are used in the
fake seam. No live Claude/OAuth call, Docker execution, credential access, or
agent/API call was performed.

The executed tests provide core evidence for portions of E-1 through E-6,
E-8, E-11, and E-12 within the allowed files. E-7, E-9, and E-10 weekly/operational integration,
the parent OCI implementation, and authenticated/container acceptance remain
pending parent integration and are not claimed by this report.

Self-check: Exact source, authority, scope, model, effort, result, and evidence contract frozen.
