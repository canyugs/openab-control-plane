* _2026-09-10 00:46:54 (gpt-5.6-luna/max)_

# provider-schema

Source commit verified: `3d14e7fc7badbb3c4b28c58866113f53ae371bd2`.

## Scope and authority

Implemented only the provider transport-schema correction requested for
`JUDGE_SCHEMA` and `SYNTHESIS_SCHEMA`. The existing controller validators remain
the citation-cardinality authority. No Agentflow, delegation, commit, push,
live model/authentication/network/API call, or Docker call was used.

## Evidence read

- `.agentflow/features/review-commit-integrity/artifacts/A-001-model-evaluation/strict-schema-semantic-negative-live.json` records both strict-schema roles exiting before usable structured output (`claude-opus-5` and `claude-opus-4-6`).
- `live-schema-report.md` records the prior schema correction state, closed nested reference objects, strict controller validation, and the limitation that live provider validation belongs to the parent.
- The user-provided reopened failure identifies the provider rejection as unsupported top-level `oneOf`/`allOf`/`anyOf` in `input_schema`; this worker did not rerun a live probe.

## Changes

- Removed only the top-level `oneOf` blocks from `JUDGE_SCHEMA` and `SYNTHESIS_SCHEMA`.
- Kept all four transport schemas free of top-level `oneOf`, `allOf`, and `anyOf`.
- Kept closed nested citation objects, required fields, enums, bounds, and array limits unchanged.
- Made the transport citation arrays permissive for empty state cases and stated the verdict condition in schema descriptions and packet rules: support/refute and supported/refuted require citations; insufficient_evidence and unknown may be empty.
- Did not change `validate_judge_result` or `_validate_synthesis_result`, add fallbacks, or add a schema-conversion framework.

## Tests and checks

- Red-first focused regression: `test_model_output_schemas_are_strictly_nested` failed on the pre-fix JUDGE root `oneOf`; the tests-only diff is preserved in `red-correction.patch`.
- Focused provider-shape and cardinality tests passed.
- `python3 -m unittest tests.test_review_model_evaluation tests.test_review_model_adapters_auth tests.test_review_model_oci_executor`: 45 tests passed.
- Compile-only check passed for the model controller, adapter, OCI executor, and their tests using `PYTHONPYCACHEPREFIX=/private/tmp/provider-schema-pycache`.
- `git diff --no-ext-diff --check` passed.

Only `scripts/review_model_evaluation.py`, `tests/test_review_model_evaluation.py`,
`red-correction.patch`, and this report were changed or added; no documentation
claim required correction. Parent-owned real full-journey and live provider
acceptance remain unexecuted.

Self-check: Exact source, authority, scope, model, effort, result and evidence contract frozen.
