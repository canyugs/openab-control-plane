* _2026-09-10 01:01:34 (gpt-5.6-luna/max)_

# structured-retries report

Source commit: `ea94f2c72fadad02dfae2705369b81acc94bb9cc`.

Authority: Design Go `1107f73567710159c298ff6a60df393e8e6b9775`.

Scope stayed limited to `scripts/review_model_adapters.py`,
`tests/test_review_model_adapters_auth.py`, the new retry fixture,
`red-correction.patch`, and this report. No controller, OCI, weekly-report,
documentation, unrelated fixture, notebook, settings, hook, credential,
Agentflow, delegation, commit, push, live model/auth, network, or Docker
operation was performed.

## Correction

The adapter now accepts up to eight ordered StructuredOutput attempts. Each
attempt must be an assistant `tool_use` named exactly `StructuredOutput` with
a bounded unique ID and object input, followed by a user `tool_result` with
the matching ID. A bounded error result is retained as untrusted data and may
be followed by another attempt; exactly one successful result must be the
last attempt before the final result envelope. The final `structured_output`
must equal the input of that successful call.

All raw stdout/stderr captures remain on `AdapterResponse`; targeted attempt
records retain each call/result block, IDs, event indexes, status, and the
untrusted tool-result capture. The response reports two attempts for the
captured retry and preserves the final envelope's `num_turns` value of three.
The existing minimal result-object path remains accepted when no event-array
tool exchange exists. The Claude `--tools` empty argument and all existing
safe/restricted/no-MCP flags are unchanged.

The parser still rejects duplicate or mismatched IDs, result-before-call,
missing results, calls/results after the final envelope, later structured
attempts after success, other tool/event names, nonempty plugin/skill/MCP
metadata, and final error envelopes.

## Captured evidence

The new fixture is byte-identical to
`.agentflow/features/review-commit-integrity/artifacts/A-001-model-evaluation/structured-output-retry-envelope.json`:
`cmp` passed; both files are 31,643 bytes. The capture contains 17 events,
including the first schema-validation error, the second successful
StructuredOutput completion, and final `result` success. The first call's
extra `path` and `utf8` fields and its error text were preserved exactly.

## Test evidence

- Red first: `python3 -m unittest tests.test_review_model_adapters_auth`
  ran 6 tests and failed one new positive retry test at the old
  `CLI emitted an unexpected StructuredOutput completion` gate; the five
  adversarial/legacy tests passed.
- Green focused suite: `python3 -m unittest
  tests.test_review_model_adapters_auth tests.test_review_model_evaluation`
  ran 32 tests successfully.
- Full Python suite: `python3 -m unittest discover -s tests` ran 65 tests
  successfully.
- `python3 -m compileall -q scripts/review_model_adapters.py
  tests/test_review_model_adapters_auth.py` passed.
- `git diff --check` passed.
- `git apply --check --cached red-correction.patch` passed, and the red
  snapshot contains only the test and fixture changes.

The parent remains responsible for rerunning the real product CLI after
import. No live-generation result is claimed here.

Self-check: Exact source, authority, scope, model, effort, result and evidence contract frozen.
