* _2026-09-10 00:35:15 (gpt-5.6-luna/max)_

# Live-schema correction report

Source commit verified: `563ae17a969f2619c277e2ab28a01886a4179529`.

## Scope

Read the captured negative-control resolution and JSON, `correction-contract.md`,
the current evaluation controller, and the read-only adapter and OCI executor.
No Agentflow, delegation, commit, push, notebook/settings/hooks, credential,
live model, Docker, authentication, or API call was used.

Writes are limited to the requested controller schema/prompt changes, model tests, documentation, this report, and the tests-only `red-correction.patch`. `review_model_adapters.py` and `review_model_oci_executor.py` were not modified.

## Correction

- `JUDGE_SCHEMA` and `SYNTHESIS_SCHEMA` now describe closed citation objects with exactly `path`, `start`, `end`, and `evidence_id`, bounded paths/IDs and line numbers, citation limits, and verdict-aware citation cardinality. `validation_verdict` remains required with the existing `valid|invalid|unproven` enum.
- `DISCOVERY_SCHEMA` now describes closed candidate objects with exactly `claim`, `path`, `start`, `end`, and `evidence_ids`, including bounds and the existing 128-candidate limit.
- `VALIDATION_SCHEMA` now describes closed generated-file, run, expectation, and observation objects. It carries generated-file bounds, exactly two baseline/counterexample runs, literal argv arrays, `cwd: "/work"`, `exit: 0`, boolean `claim_present`, and generated paths below `generated/`.
- Dynamic evidence membership, source-range membership, path safety, shell rejection, UTF-8/NUL checks, distinct controls, actual JSON stdout, and execution outcomes remain enforced by the existing strict validators and OCI boundary; no validator was loosened.
- Per-role packets and prompts now require exact reference shapes and matching IDs, source/evidence-only citations, blind discovery, two fresh judges without peer output, meaningful claim-versus-negation assessment, read-only `/source`, `/work/generated` materialization, direct Python3 literal argv, `/work` cwd, actual JSON stdout, and no manual prerequisite.
- A concrete regression showed normal `sys.path.insert(0, "/source")` plus direct source import was accepted by the plan validator but missed by the controller’s structural binding signal. The narrow signal now recognizes that pattern while existing print-only and path-comment negative controls remain unbound. This remains a structural signal, not semantic proof.

## Evidence

Red-first schema regressions were added before the implementation. The initial focused run produced one schema error at the missing citation `maxItems`/items contract and one failure for the imported-source binding case. The exact tests-only red diff is retained in `red-correction.patch`.

Executed green checks:

- Focused contract tests: 3 passed.
- `python3 -m unittest tests.test_review_model_evaluation tests.test_review_model_oci_executor`: 41 passed (25 controller, 16 OCI).
- `PYTHONPYCACHEPREFIX=/private/tmp/live-schema-pycache-final python3 -m compileall -q` over both model controller tests and the read-only adapter/OCI modules: passed.
- `git diff --check`: passed.
- Draft-7 schema grammar check for all four model schemas: passed using the installed JSON Schema checker.
- Final scope check: only `docs/model-evaluation.md`, `scripts/review_model_evaluation.py`, `tests/test_review_model_evaluation.py`, `red-correction.patch`, and this report are present as task outputs; no repository bytecode artifacts remain.

## Remaining live limitations

No live Opus/Claude transport, Docker daemon, auth, or production/API validation was run by this worker. The parent must perform those authorized live checks. Static schemas cannot encode the runtime-specific allowed evidence IDs or source line bounds; the controller validators remain the acceptance authority. No live-model compatibility or live OCI success is claimed.

Self-check: Exact source, authority, scope, model, effort, result and evidence contract frozen.
