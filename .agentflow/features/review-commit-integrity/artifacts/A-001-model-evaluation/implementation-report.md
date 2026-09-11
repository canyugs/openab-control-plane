* _2026-09-09 23:09:24 (gpt-5.6-luna/max)_

# Automatic evaluation implementation report

The frozen Stage 2 model-evaluation and round-measurement designs were
implemented in this disposable clone. No Rust, controller, notebook,
settings, hooks, credentials, live model, Docker daemon, GitHub, or Agentflow
action was used. Stage 1 source remains untouched.

## Approved result paths

- `scripts/review_model_evaluation.py`
- `scripts/review_model_adapters.py`
- `scripts/review_model_oci_executor.py`
- `scripts/review_round_weekly_report.py`
- `tests/test_review_model_evaluation.py`
- `tests/test_review_model_oci_executor.py`
- `tests/test_review_round_weekly_report.py`
- `tests/fixtures/model_evaluation/`
- `docs/model-evaluation.md`
- `docs/review-round-weekly-report.md`
- `implementation-report.md`
- `red-regression.patch`

No other implementation result is authorized. The scope-discipline paragraph
was not delegated because no worker brief was created.

## E-1 through E-12

- E-1: The controller snapshots one full revision, evaluates every supplied
  finding, and records `provenance: model_assessment`.
- E-2: Two fresh judge invocations require distinct configured model IDs and
  return the frozen verdict/usefulness/citation/counterexample shape. Same
  family is a visible correlation warning; there is no fallback.
- E-3: Source paths, line ranges, evidence IDs, evidence digests, and allowed
  ranges are controller-validated before an assessment is valid.
- E-4: Fresh synthesis receives anonymized valid assessments plus source and
  executed evidence, preserves disagreement, and can return `unknown`.
- E-5: Blind discovery receives no original finding/result data. Candidates
  are reconciled explicitly, then receive fresh independent judges, synthesis,
  and reproduction before an automatic omission label.
- E-6: Static, executed-reproduced, executed-refuted, environment-blocked,
  and unproven categories are exclusive. Generated files and literal controls
  run only through the OCI boundary; source binding prevents print-only proof.
- E-7: The weekly report derives accepted-to-projection latency from captured
  controller/GitHub receipt evidence and labels reconciliation upper bounds.
- E-8: Requested IDs, CLI probes, argv, raw transport data, trusted metadata
  when emitted, unknown observed identity, usage, and cost are retained
  separately.
- E-9: Weekly reliability is source-bound and exclusive across reliable,
  visible-failure, superseded, and pending/unknown; judgment is separate.
- E-10: Weekly JSON/Markdown exposes model assessment, usefulness,
  disagreement, validation coverage, omission candidates, human escape
  counts, delivery categories, and separate denominators/unknowns.
- E-11: No result path mutates verdicts, retries, rosters, GitHub writes,
  routing, checkout, patches, merge, or production state.
- E-12: Snapshot/input/artifact conflicts fail visibly; completed invocation
  bytes are rechecked on restart; failed/incomplete runs are partial/failed
  and never score.

## Red-first and green verification

The tests-only red diff was saved as `red-regression.patch` before the modules
were implemented. The initial behavior run was:

```text
python3 -m unittest tests.test_review_model_evaluation tests.test_review_model_oci_executor tests.test_review_round_weekly_report -v
```

Result: 5 tests ran and 5 real assertion failures reported missing controller,
OCI, and weekly-report behavior; there were no syntax-error-only failures.

The final deterministic command was:

```text
python3 -m unittest tests.test_review_model_evaluation tests.test_review_model_oci_executor tests.test_review_round_weekly_report -v
```

Result at implementation handoff: 18 tests passed, 0 failed. The suite uses
fake CLI transcripts/process boundaries and temporary synthetic Git/bundle
data; it makes no live model or Docker claim.

Compilation was run with:

```text
python3 -m py_compile scripts/review_model_adapters.py scripts/review_model_oci_executor.py scripts/review_model_evaluation.py scripts/review_round_weekly_report.py
```

Result: passed. A final `compileall` check is included in the handoff command
set and completed with no output. Generated `__pycache__` files are not result
paths and are removed before handoff.

## Limitations and unresolved items

- Claude authentication and a real PTY journey were not attempted. Provider
  alias resolution, observed backend identity, usage, and actual cost remain
  unknown unless the operator’s CLI envelope supplies them.
- Codex remains explicitly unavailable until enforced no-tools/process
  confinement is independently proven. It is never substituted.
- The default OCI image is a placeholder digest. A locally present digest-
  pinned image and Docker daemon are operator prerequisites; unavailable setup
  is reported as `environment_blocked`.
- No live weekly bundle was collected. Human adjudication, cost reconciliation,
  delivery capture, fleet operation, scheduling, dashboards, calibration, and
  production use remain out of scope.
- Model agreement is not correctness, automatic omission is not recall, and
  incomplete source/model/receipt evidence cannot support whole-scope claims.

Self-check: Exact approved capability scope, source authority, model, effort, tests and no live system work frozen.
