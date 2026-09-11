* _2026-09-10 01:08:40 (gpt-5.6-luna/max)_

# submodule-guard

## Scope and authority

- Source commit read: `033e5cb203188a4d4fd1d9f91d77311fc22efdf5`.
- Implemented only the reproduced E11 nested-submodule snapshot boundary.
- Source authority was limited to `scripts/review_model_evaluation.py` snapshot helpers; added `tests/test_review_model_git_submodules.py`, corrected the actual model-evaluation boundary statement in `docs/model-evaluation.md`, and wrote the requested root artifacts.
- No Agentflow call, commit, push, notebook/settings/hooks change, credential access, remote operation, live API/auth/Docker execution, or model-generated code execution was performed.

## Evidence read and reproduction

- Read `.agentflow/features/review-commit-integrity/artifacts/A-001-model-evaluation/submodule-boundary-probe.json`: it records `marker_executed: true` and `exception: null` for the benign nested filter probe.
- Fresh inline reproduction before the fix matched that result: `_validate_clean_repo(parent)` returned successfully, and the nested clean helper changed the marker from absent to present. The parent’s direct status output was empty.
- The persistent red command was `python3 -m unittest tests/test_review_model_git_submodules.py`; it ran 3 tests and failed in all three cases before the source correction.

## Correction

- Added a nonexecuting parent-index gitlink scan using `git ls-files --stage -z` before working-tree status. Any mode-160000 entry is rejected before Git can inspect nested content or config.
- Reordered revision-tree inspection ahead of archive creation and reject mode-160000 entries from `git ls-tree -r -z --full-tree`. A submodule revision therefore cannot be presented as a complete source packet or reach the diff step.
- The existing root fsmonitor/filter guards and root dirty-tree rejection remain active. Normal repositories still produce complete packets.
- The same-path diff probe used two parent revisions containing the same nested path but different submodule commits. It showed the existing guarded mode-160000 `Subproject commit` presentation, and the nested marker remained absent. No external/textconv behavior was changed.

## Verification

- `python3 -m unittest tests/test_review_model_git_submodules.py`: 3 tests, OK.
- `python3 -m unittest discover -s tests -p 'test_*.py'`: 65 tests, OK.
- `python3 -m py_compile scripts/review_model_evaluation.py tests/test_review_model_git_submodules.py`: OK.
- `git diff --check`: OK.
- `git apply --check --reverse red-correction.patch`: OK.
- The new regression covers initialized submodules with benign `filter.probe.clean` and `filter.probe.process` marker helpers; the control covers a normal clean snapshot and dirty root-file rejection.

## Files

- `scripts/review_model_evaluation.py`
- `tests/test_review_model_git_submodules.py`
- `docs/model-evaluation.md`
- `red-correction.patch`
- `submodule-guard-report.md`

The diagnosing-bugs and TDD skills were applied through the red reproduction, narrow fix, and green regression loop. No unrelated architecture, model, production, remote, or network behavior was changed.

Self-check: Exact source, authority, scope, model, effort, result and evidence contract frozen.
