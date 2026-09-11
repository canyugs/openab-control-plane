* _2026-09-10 00:38:20 (gpt-5.6-luna/max)_

# structured-transport report

Source commit: `1a75b2d8b864fd9e1b464f0b3078a77a7f86bc4b`.

Authority: Design Go `1107f73567710159c298ff6a60df393e8e6b9775`.

Scope was limited to the requested adapter transport correction:
`scripts/review_model_adapters.py`, `tests/test_review_model_adapters_auth.py`,
`tests/fixtures/model_evaluation/claude-structured-tool-events.json`,
`red-correction.patch`, and this report. No live model/auth, Docker, network,
Agentflow, delegation, commit, push, notebook, settings, hooks, or credential
operation was performed.

Read parent evidence:
`.agentflow/features/review-commit-integrity/artifacts/A-001-model-evaluation/structured-output-tool-envelope.json`
and `corrected-adapter-live.json`. The new fixture is byte-identical to the
captured envelope; its SHA-256 is
`37c945de73ca3bb2185a11623d3ed6eaae69ab2dffff4efc60d2146e332d7e10`.
The parent live-result record reports authenticated failures for both configured
models at the parser's nested tool-block rejection; this worker did not rerun
those live calls.

Implemented `_reject_unexpected_tools` support for the observed Claude envelope:
one assistant `tool_use` named exactly `StructuredOutput`, with an object input
and bounded non-empty ID, followed by one user `tool_result` whose
`tool_use_id` exactly matches and whose content is exactly
`Structured output provided successfully`, before the final result envelope.
The init declaration must be exactly `["StructuredOutput"]`. Duplicate calls,
duplicate or conflicting result IDs, unmatched/missing IDs, wrong ordering,
failed/altered completions, other tool names, file/MCP events, and top-level
tool events remain rejected. Non-empty plugins, skills, or MCP metadata remains
rejected. Existing observed model identity, auxiliary usage, list-price estimate,
and `actual_cost_usd: "unknown"` handling remain unchanged.

The Claude argv contract was not loosened: `--tools` remains an empty value,
with the existing safe/restricted, strict MCP, empty setting-source, and related
no-tools flags unchanged. No general tools were enabled.

Red-first evidence, before the adapter change:
`python3 -m unittest tests.test_review_model_adapters_auth` ran 3 tests and
failed the new positive captured-envelope test with
`AdapterError: CLI emitted an unexpected executable or MCP tool block`.

Post-fix evidence:

- `python3 -m unittest tests.test_review_model_adapters_auth` — 3 tests passed.
- `python3 -m unittest tests.test_review_model_adapters_auth tests.test_review_model_evaluation` — 25 tests passed.
- `git diff --check` — passed.
- `python3 -m unittest discover -s tests` — 55 tests ran with one unrelated
  error in
  `test_review_round_weekly_report.WeeklyReportBehaviorTests.test_integrated_evaluation_without_core_helper_reports_dependency_not_raw_summary`;
  it fails on `snapshot.json` handling in the concurrent weekly/schema authority
  (`scripts/review_round_weekly_report.py` / `scripts/review_model_evaluation.py`),
  outside the listed files. No change was made to that path.

Self-check: Exact source, authority, scope, model, effort, result and evidence contract frozen.

