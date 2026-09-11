* _2026-09-10 01:14:01 (gpt-5.6-luna/max)_

# normalization-seam

Source is exact commit `d80e2e2ba1c37627679e618a81570d42e4cda86c`. The work was performed in the disposable independent no-remote clone under the stated Design authority `1107f73567710159c298ff6a60df393e8e6b9775`; no Agentflow call, delegation, commit, push, credential access, live API, or settings/hook/notebook change was made. Write scope was limited to `scripts/review_model_oci_executor.py`, `tests/test_review_model_oci_executor.py`, this report, and `red-correction.patch`.

I read the recorded `live-oci-normalization-failure.json`, all three actual `live-*-plan.json` files (`live-F-1-plan.json`, `live-omission-1-e456c6d814336027-plan.json`, and `live-omission-2-a351ce82491f2fdc-plan.json`), core `_run_validation` read-only, and executor `validate_generated_plan`/`execute`. The recorded failure shows `OCIError`, no runs, and no container control reached. Each actual plan carries generated file `bytes` and `sha256` metadata, matching the reported core-to-executor seam.

The minimal correction is executor-local. Raw file entries with exactly `{path, utf8}` remain accepted and are normalized. Canonical normalized entries with exactly `{path, utf8, bytes, sha256}` are now accepted only when `bytes` is a non-boolean integer equal to the exact UTF-8 byte length and `sha256` is a lowercase 64-hex digest equal to a fresh SHA-256 calculation. Unknown, partial, forged, or inconsistent metadata is rejected. The validator emits the same canonical normalized fields and preserves the model `utf8` text byte-for-byte; it does not strip, rewrite, or unescape generated code. The core was not changed, so both raw and already-normalized plans use the existing execute path and normalized artifact/result shape.

Evidence executed:

- Red-first idempotency test initially failed with `OCIError: generated files require only path and utf8`.
- Focused OCI executor suite: 19 tests passed.
- Complete Python test suite: 76 tests passed.
- `python3 -m compileall -q scripts` passed with bytecode redirected to a disposable temp location.
- `git diff --check` passed.

The fake OCI boundary integration passed a normalized plan through both baseline and counterexample command-seam controls and retained the normalized file manifest. Raw-plan positive behavior and existing artifact/materialization checks remained green in the complete suite. No Docker/actual OCI run, live model, authentication, network, or generated-code execution on the host was performed; actual OCI acceptance remains for the parent after import.

Self-check: source d80e2e2ba1c37627679e618a81570d42e4cda86c; authority Design 1107f73567710159c298ff6a60df393e8e6b9775; scope executor normalization seam plus authorized tests/report/patch only; model gpt-5.6-luna/max; effort red-green implementation with recorded artifact/core/executor inspection and deterministic checks; result idempotent verified derived metadata with raw compatibility; evidence contract includes 19 focused tests, 76 complete Python tests, compileall, diffcheck, and explicit absence of live OCI/model/auth/network/host-generated-code execution.
