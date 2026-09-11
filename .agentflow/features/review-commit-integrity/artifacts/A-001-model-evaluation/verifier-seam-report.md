* _2026-09-10 00:24:06 (gpt-5.6-luna/max)_

# verifier-seam

Source commit: `563ae17a969f2619c277e2ab28a01886a4179529`.
Authority: Design Go `1107f73567710159c298ff6a60df393e8e6b9775`.
Scope stayed limited to the weekly/core verifier exception seam: the weekly
wrapper, its tests, this report, and the tests-only `red-correction.patch`.
No core source, Rust source, dependency, workflow, notebook, setting, hook, or
credential was changed. No service, model, Docker, or live API was used.

## Implemented

- `verify_evaluation_artifacts` now translates only the core
  `EvaluationConflict`/`EvaluationError` validation failures into a clear
  `ReportError`. The existing CLI catches that report error and returns status
  2 without a traceback or core exception-class name.
- The missing-helper control now inserts an actually unavailable
  `review_model_evaluation` module and proves that a raw `summary.json` is not
  scored.
- Added an actual-core invalid-root control (`snapshot.json` is not a regular
  file), identity-preserving validated-result passthrough coverage, and an
  unexpected-fault control proving arbitrary programming faults are not
  suppressed.
- `red-correction.patch` contains only the tests added or tightened before
  the production fix.

## Evidence

Red-first focused run, before the wrapper fix:

```text
python3 -m unittest tests.test_review_round_weekly_report
Ran 17 tests
FAILED (errors=1)
```

The sole error was the real core `EvaluationConflict` for the invalid
`snapshot.json` root escaping the weekly wrapper. The tests-only red snapshot
is `red-correction.patch`.

Green verification:

```text
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest tests.test_review_round_weekly_report
Ran 17 tests in 0.052s
OK

PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests -q
Ran 55 tests in 7.030s
OK

PYTHONDONTWRITEBYTECODE=1 python3 -m compileall -q scripts/review_round_weekly_report.py
OK

git diff --check
OK
```

The full suite emitted only the environment's repeated Darwin temporary
directory warnings from fixture Git commands; it still completed successfully.
No commit or push was made.

Self-check: Exact source, authority, scope, model, effort, result and evidence contract frozen.
