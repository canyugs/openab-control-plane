* _2026-09-10 13:09:35 (gpt-5.6-luna/max)_

# package-repair

Bounded packaging repair from exact base `d9c1e2ca6632414aedfcaafac1ee19e3d8f45266`.

## Changes

- Removed `--top-level-directory .` from the evaluation release workflow. The
  workflow now runs `unittest discover` with `tests` and `test_*.py` only.
- Split launcher numeric validation: `--uid` remains nonzero, while `--gid`
  and `--docker-gid` accept nonnegative decimal IDs, including `0`. The
  literal argv test now exercises `--user 1001:0`, `--group-add 0`, preserves
  the identical scratch bind and `TMPDIR` assertions, and verifies `--uid 0`
  is rejected.
- Clarified that the weekly bundle schema remains OCP-specific, local work
  has not published the package or image, socket GIDs are interpreted inside
  the outer Linux container and may be `0` on Docker Desktop, and Linux
  model-auth requires explicit runtime credentials beyond host-model or
  package/OCI checks.

## Verification

- `bash -n packaging/evaluation/run.sh` — PASS.
- `PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests -p 'test_evaluation_package.py'` — PASS, 5 tests, 1 skipped. The wheel-install test was honestly skipped because no offline wheel builder (`build` or `setuptools`) was installed.
- `PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover --start-directory tests --pattern 'test_*.py'` — PASS, 91 tests, 1 skipped.
- `PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests -p 'test_*.py'` — PASS, 91 tests, 1 skipped.
- `git diff --check && ! rg -n --fixed-strings -- '--top-level-directory .' .github/workflows/evaluation-release.yml` — PASS.

The full suite emitted repeated macOS `DARWIN_USER_TEMP_DIR` warnings from
Git subprocesses but had no test failures. No Docker, model, API, network,
credential, publication, commit, push, Agentflow, or delegated work was run.
Parent-owned image, CLI, weekly-report, nested-OCI, and model-auth checks were
not repeated. The wheel build was therefore skipped rather than claimed.

## Changed paths

- `.github/workflows/evaluation-release.yml`
- `docs/evaluation-package.md`
- `packaging/evaluation/run.sh`
- `tests/test_evaluation_package.py`
- `package-repair-report.md`

No core modules, `pyproject.toml`, `MANIFEST.in`, `Dockerfile.evaluation`,
Rust/legacy source, other tests, or other documentation/settings/records were
modified. Work remains local and unpublished.

Self-check: Scope, source, model/effort, authority and write boundary frozen.
