* _2026-09-10 01:22:33 (gpt-5.6-luna/max)_*

# disagreement-metric

## Scope and authority

- Source commit read: `9feaae8ba848eb3d4d38c80ab081d1aff706de48`.
- Authority: Design Go `1107f73567710159c298ff6a60df393e8e6b9775`.
- Effort/model: basic tier, configured codex-default `gpt-5.6-luna/max`.
- Implemented only E10 model-disagreement metric correction in `_summary` and `_evaluation_metrics`, the new regression test, one precise weekly-report definition sentence, `red-correction.patch`, and this report.
- No Agentflow, delegation, commit, push, notebook/settings/hooks, credentials, live model/auth/Docker/network/API operation, or generated-code execution was performed.

## Evidence and red reproduction

- Read `.agentflow/features/review-commit-integrity/artifacts/A-001-model-evaluation/disagreement-live-counterexample.json` and `first-full-vulnerable-summary.json`.
- The captured item records show three `support/support` typed judge pairs with `judge_disagreement: false`, while each synthesis `disagreement` field contains nonempty qualitative prose; the vulnerable summary records `disagreement_items: 3` and no derived eligible denominator.
- Red command: `PYTHONDONTWRITEBYTECODE=1 python3 -m unittest tests/test_review_model_disagreement.py`. It ran 4 tests and failed on the three prose-counted summary items (`3 != 0`), incomplete-judge count (`3 != 0`), and legacy weekly denominator (`1` instead of unknown).

## Correction

- `_summary` now derives `model_assessment.disagreement_denominator` from items with exactly two valid judge assessments and counts only the existing typed `judge_disagreement is True` flag within that eligible pair set.
- Synthesis disagreement text is untouched and remains qualitative. Absent, failed, and single-judge items contribute neither agreement nor disagreement.
- `_evaluation_metrics` consumes the derived denominator only for scoreable complete evaluations, aggregates it across evaluations, and returns JSON `null` when a complete legacy summary does not provide it; it no longer uses evaluation count as a comparison denominator.
- Partial/failed quality remains `not_scoreable`; validation, cost, human, reliability, and other denominators were not changed.

## Verification

- Focused regression after correction: `PYTHONDONTWRITEBYTECODE=1 python3 -m unittest tests/test_review_model_disagreement.py` — 4 tests, OK.
- Relevant suite: `PYTHONDONTWRITEBYTECODE=1 python3 -m unittest tests.test_review_model_disagreement tests.test_review_model_evaluation tests.test_review_round_weekly_report` — 47 tests, OK.
- Full Python suite: `PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests -p 'test_*.py'` — 80 tests, OK.
- Compile check: `PYTHONPYCACHEPREFIX=/private/tmp/disagreement-metric-pycache python3 -m compileall -q scripts/review_model_evaluation.py scripts/review_round_weekly_report.py tests/test_review_model_disagreement.py` — OK.
- `git diff --check` — OK.
- `git apply --check --reverse red-correction.patch` — OK.
- Final status contains only `docs/review-round-weekly-report.md`, `scripts/review_model_evaluation.py`, `scripts/review_round_weekly_report.py`, `tests/test_review_model_disagreement.py`, `red-correction.patch`, and this report.

The diagnosing-bugs and TDD skills were applied through the red reproduction, narrow correction, and green regression loop. No unrelated controller, schema, AST/source-binding, model, or runtime behavior was changed.

Self-check: Exact source, authority, scope, model, effort, result and evidence contract frozen.

