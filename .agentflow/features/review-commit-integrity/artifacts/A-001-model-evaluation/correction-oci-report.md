* _2026-09-09 23:35:25 (gpt-5.6-luna/max)_

# correction-oci

The source was verified at commit `27497d41aef6c7c736733d4333a75defef386255`.
I read the mandated `design.md`, `design-resolution.md`,
`early-executor-inspection.md`, `early-runtime-controls.json`, and
`correction-contract.md`. The implementation stayed within the OCI executor
boundary and its tests; no core, weekly, documentation, notebook, settings,
hooks, credentials, Agentflow, commit, push, or live API changes were made.

The executor now uses the approved digest-pinned Python image by default,
attaches Docker stdin with `-i`, uses a non-root networkless read-only
container with bounded `/work` and `/tmp` tmpfs mounts, explicit resource and
privilege limits, and only allowlisted child environment variables. Baseline
and counterexample controls run in separate fresh containers, so the first
generated process cannot mutate the second control's workspace or harness.

Generated plans require two literal controls with zero expected exits,
structured observations, declared evidence, and differing boolean
`claim_present` expectations. The return seam preserves additive
`controls_passed` and baseline `claim_present` fields. Mechanical controls can
pass when fake print-only output matches the plan, but `classification` remains
`unproven`; semantic reproduction is left to the independent controller
assessment. No source-path substring heuristic is used here.

The host process boundary streams and bounds stdout/stderr, kills the process
tree on timeout or overflow, retains bounded raw text plus base64/digest/size
captures, and removes only the exact owned named container on failed attempts.
Failures retain the normalized plan, both control records, and available raw
captures. Source materialization verifies UTF-8/base64 bytes and digests,
rejects traversal, symlink components, and path conflicts, and generated files
are sent as data to the trusted fixed runner rather than written on the host.
Optional operator checks use the same validation/execution boundary and are
deferred when the primary controls fail; they are not prerequisites.

Evidence executed:

- Red-first run: the initial expanded OCI suite ran 15 tests and failed with
  9 failures and 4 errors against the uncorrected source, covering the
  missing `-i`, placeholder image, shared controls, unbounded
  `communicate`, missing claim checks, cleanup, and capture regressions.
- Green run: `python3 -m unittest -v tests/test_review_model_oci_executor.py`
  passed all 16 tests.
- Integration run: `python3 -m unittest discover -s tests -p 'test_*.py'`
  passed all 30 tests.
- `python3 -m compileall -q scripts/review_model_oci_executor.py` passed.
- The embedded fixed runner compiled with Python's in-memory `compile`, and
  `git diff --check` passed.
- `report/red-correction.patch` matches the current authorized source/test
  diff byte-for-byte.

All Docker behavior was exercised with deterministic fake process boundaries;
no live Docker daemon, image pull, authentication, or real OCI execution was
claimed. Parent-owned real Docker acceptance remains required.

Self-check: Exact source, authority, scope, model, effort, result and evidence contract frozen.
