* _2026-09-10 01:32:06 (gpt-5.6-sol/low)_

# Stage 2 security review

Security: PASS

This is a defensive security result for the Python offline evaluator, OCI executor, model adapter, weekly reporter, tests, fixtures, documentation, and captured evidence added after accepted Stage 1. It is not product acceptance, Result Go, permission to merge or deploy, or a claim that the parent's fresh full CLI journey completed.

## Identity, authority, and scope

- Reviewed implementation HEAD: `6e5784bdd19e5e84ccc19637e67de643e6d65920` (exact full SHA requested by the wrapper and returned by `git rev-parse HEAD`).
- Diff base: accepted Stage 1 `d6e96c32abfd766588c549e8922e3077d6abcb00`.
- Owner Design Go: `1107f73567710159c298ff6a60df393e8e6b9775`.
- Review identity: one `gpt-5.6-sol/low` security pass; no delegation or Agentflow invocation.
- Declared implementation surfaces inspected: `scripts/review_model_evaluation.py`, `scripts/review_model_adapters.py`, `scripts/review_model_oci_executor.py`, `scripts/review_round_weekly_report.py`; the seven `tests/test_review_*.py` modules; `tests/fixtures/model_evaluation/`; `docs/model-evaluation.md`; `docs/review-round-weekly-report.md`; and the complete Stage 1-to-HEAD name/status and stat diff.
- Authority/contract records read: model-evaluation `design.md`, `design-resolution.md`, `correction-contract.md`, `transport-preflight-resolution.md`, `binding-contract-resolution.md`, and accepted `binding-contract-report.md`; round-measurement `design.md` and `spec-resolution.md`; and the current owner Ask/Design Go plus subsequent correction history in `review-commit-integrity.devlog.md`.
- Captured evidence inspected: `normalized-model-plans-live-oci-success.json`, `actual-adapter-semantic-negative.json`, `submodule-boundary-probe.json`, and `submodule-boundary-corrected.json`.
- Initial worktree state was clean. No source, test, documentation, configuration, notebook, setting, hook, or credential was changed. This report is the only created path.

## Confirmed findings

No confirmed security findings.

The reviewed reachable boundaries satisfy the standing obligations:

- Repository snapshot operations reject full-SHA mismatches, dirty/untracked state, gitlinks, executable local/worktree Git configuration, external diff/textconv, filters, fsmonitor, hooks, credential helpers, recursive submodules, and ext/file transports before archive/status/diff use (`scripts/review_model_evaluation.py:156-218`, `scripts/review_model_evaluation.py:221-309`). The corrected nested-submodule capture records `marker_executed: false`, refusal `repository contains submodule gitlinks`, and unchanged nested config.
- The source packet compares the full tracked regular-file tree with the tar archive, bounds file/count/total/diff bytes, records omissions, and cannot advertise completeness after an omission (`scripts/review_model_evaluation.py:312-405`). Revision and base are literal 40-hex commits resolved without later-HEAD substitution (`scripts/review_model_evaluation.py:275-281`).
- Model processes receive literal argv, `shell=False`, a bounded packet/output/timeout, an empty fresh session directory, closed extra descriptors, a process group kill path, and an allowlisted environment (`scripts/review_model_adapters.py:76-94`, `scripts/review_model_adapters.py:210-327`). OAuth transport uses safe/restricted/no-slash/no-persistence, empty tools, strict MCP configuration, empty setting sources, and rejects unexpected tool metadata. Codex remains fail-closed unavailable because its required no-tools confinement is not verified (`scripts/review_model_adapters.py:720-887`). Artifact environment metadata redacts credential-like names; the focused repository/artifact scan returned no candidate API key, GitHub token, bearer token, or populated key variable.
- Generated model files are validated as bounded relative `generated/` UTF-8 paths, reject traversal/symlinks/shell interpreters/`-c`/failed or assertion-only controls, and are passed as JSON over stdin to a fixed runner. They are materialized only inside a fresh container tmpfs, not by the host controller (`scripts/review_model_oci_executor.py:130-296`, `scripts/review_model_oci_executor.py:326-389`, `scripts/review_model_oci_executor.py:565-735`).
- Each control uses a fresh named container with a digest-pinned image, `--pull=never`, `--network none`, read-only root and source/runner mounts, tmpfs work/tmp, UID/GID 65532, all capabilities dropped, `no-new-privileges`, and CPU/memory/PID/output/time bounds (`scripts/review_model_oci_executor.py:326-389`, `scripts/review_model_oci_executor.py:959-1055`). Captured evidence shows all three supplied real normalized plans used these argv controls and produced two valid, distinct observations; their raw OCI classification remained `unproven`, as required before semantic assessment.
- The controller range- and digest-validates source/evidence citations, requires two fresh isolated judgments, anonymized synthesis, passed OCI controls, matching semantic directions, `validation_verdict: valid`, and nonconflicting synthesis before `executed_reproduced`/`executed_refuted` and scoreability. The AST source-binding signal is diagnostic only; missing/invalid/disagreeing semantic qualification cannot complete a run (`scripts/review_model_evaluation.py:640-838`, `scripts/review_model_evaluation.py:2135-2205`, `scripts/review_model_evaluation.py:2477-2491`). The captured argv-echo negative control was independently marked `validation_verdict: invalid` by requested/observed `claude-opus-5` and `claude-opus-4-6`.
- Immutable snapshot/input identity precedes model work; completed replay verifies the ledger and every retained artifact hash, rejects unsafe/symlink/traversal/missing/changed/conflicting files, and does not re-probe models or OCI (`scripts/review_model_evaluation.py:1452-1554`, `scripts/review_model_evaluation.py:1587-1636`). Weekly evaluation ingestion uses that verifier and keeps partial/failed evaluations unscoreable; report generation is offline and has no GitHub/controller mutation path.

## Commands and results actually executed

1. `git rev-parse HEAD` -> `6e5784bdd19e5e84ccc19637e67de643e6d65920`; initial `git status --short` -> empty.
2. `git diff --stat d6e96c32abfd766588c549e8922e3077d6abcb00..HEAD` and matching `git diff --name-status ...` -> declared Stage 2 Python/evaluator/OCI/adapter/weekly, tests, fixtures, docs, and workflow evidence surfaces inspected; no Rust source change was tested.
3. `PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests -p 'test_*.py' -v` -> 86 tests run, all PASS, 7.161 seconds.
4. Eight concrete boundary tests for fsmonitor/filter helper refusal, initialized-submodule refusal, immutable replay/tamper rejection, OCI argv isolation, source symlink/digest rejection, OAuth USER-only environment propagation, and unexpected adapter tool/binding rejection -> 8 tests run, all PASS, 1.787 seconds.
5. `PYTHONDONTWRITEBYTECODE=1 PYTHONPYCACHEPREFIX=<disposable-/tmp> python3 -m compileall -q` over the four Python scripts -> exit 0; bytecode output was directed only to the disposable temp path.
6. `git diff --check` -> exit 0.
7. Focused `rg -l` credential-pattern scan over the Stage 2 artifacts/docs/scripts/tests -> no matches.
8. Read-only `jq` inspection of the four named captured-evidence files -> three OCI plans report `status: success`, `controls_passed: true`, and raw `classification: unproven`; both semantic-negative judges report successful distinct model transports and invalid validation; corrected submodule marker is false and config unchanged. No Docker, model, authentication, network, GitHub, or other live API command was run in this review.

## Limits and proposals

- The review did not reproduce the captured Docker or authenticated model calls. Their checked-in records demonstrate what was captured, not an independently rerun environment. The parent's fresh end-to-end CLI journey was explicitly still running and is outside this security result.
- Two model assessments, especially from one provider family, can agree and still be wrong. The implementation labels them `model_assessment`, preserves the same-family/correlation limitation, and does not claim a deterministic semantic oracle. OCI observations establish controlled execution, not arbitrary semantic causation.
- Artifact hashes establish byte identity, not the truth or completeness of upstream evidence. Weekly source/product joins remain constrained by declared capture coverage and source state.
- No proposal is required for this security gate. Possible runtime tracing remains parked: it would add unauthorized capability and would not prove semantic causation.

No confirmed security issue blocks the owner-requested strong-model workflow at this source SHA. Security PASS does not complete acceptance.

Self-check: Exact source, authority, scope, model, effort, result and evidence contract frozen.
