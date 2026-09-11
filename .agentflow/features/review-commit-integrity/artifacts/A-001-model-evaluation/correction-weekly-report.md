* _2026-09-09 23:50:03 (gpt-5.6-luna/max)_

# correction-weekly

Implemented source-grounded offline weekly correctness for source commit `27497d41aef6c7c736733d4333a75defef386255`.

The allowed implementation paths are limited to the weekly report module,
its tests, its documentation, and `report/red-correction.patch`. No Rust,
OCI, notebook, settings, hooks, credentials, Agentflow, commit, push, live
API, or external service was used.

The report now uses exact accepted/completed action joins; Taipei week
deduplication; seconds-versus-milliseconds cutoff handling; exact
`correlation.write_id` + session + operation + payload-digest receipt binding;
immutable target/verified SHA agreement; formal review receipts with
`review_id`, `APPROVED`/`CHANGES_REQUESTED`, `commit_id`, and `reconciled`;
source-derived diagnostic write sets; excluded opening/decision writes;
bound visible tombstones; supersession precedence; required-write latency;
metric-specific human/cost unknowns; and evaluation identity hashing through
the `verify_evaluation_artifacts` seam.

Red-first evidence: after changing the positive fixture to the actual formal
review receipt, the focused regression failed with reliable count `0 != 1`.
The corrected implementation and source-shaped fixture suite then passed.

Executed checks:

- `python3 -m unittest tests.test_review_round_weekly_report` — 13 tests passed.
- `python3 -m compileall -q scripts/review_round_weekly_report.py` — passed.
- `git diff --check` — passed.

The injected-verifier evaluation tests pass and raw `summary.json` is never
trusted. Integrated evaluation verification remains pending because this
clone's concurrent core does not yet expose `verify_evaluation_artifacts`; the
weekly report returns an explicit dependency status instead of reading raw
evaluation files. No Rust or core-helper integration was claimed.

Self-check: Exact source, authority, scope, model, effort, result and evidence contract frozen.
