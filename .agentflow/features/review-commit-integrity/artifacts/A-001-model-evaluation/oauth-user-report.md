* _2026-09-10 00:23:48 (gpt-5.6-luna/max)_

Source commit: `563ae17a969f2619c277e2ab28a01886a4179529`  
Authority: `scripts/review_model_adapters.py`  
Owner: Design Go `1107f73567710159c298ff6a60df393e8e6b9775`

Implemented exactly the OAuth safe-environment regression fix: added only `USER` to the explicit child-process environment allowlist. No credential extraction, token copying, broader environment fallback, or `LOGIN` request was added. The new test verifies `USER` reaches a real `run_direct` child while `NODE_OPTIONS`, `PYTHONPATH`, `LD_PRELOAD`, `DYLD_INSERT_LIBRARIES`, `LOGIN`, and an arbitrary project variable remain absent.

Red-first evidence: `python3 -m unittest tests.test_review_model_adapters_auth` failed before the source change because `safe_environment` returned `{'PATH': '/usr/bin:/bin'}` instead of the required `USER` entry.

Green and regression evidence:

- `python3 -m unittest tests.test_review_model_adapters_auth` — 1 test passed.
- `python3 -m unittest tests.test_review_model_adapters_auth tests.test_review_model_evaluation tests.test_review_model_oci_executor` — 39 tests passed.
- `python3 -m unittest tests.test_review_round_weekly_report` — 14 tests passed separately.
- `git diff --check` — passed.

No documentation was changed. No live authentication, model, Docker, credential, or parent CLI command was executed here; the parent owns final real-CLI acceptance. Working-tree changes are limited to `scripts/review_model_adapters.py`, `tests/test_review_model_adapters_auth.py`, `red-correction.patch`, and this report.

Self-check: Exact source, authority, scope, model, effort, result and evidence contract frozen.
