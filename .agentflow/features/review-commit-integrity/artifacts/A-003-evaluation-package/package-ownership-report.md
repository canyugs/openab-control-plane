* _2026-09-10 13:34:37 (gpt-5.6-luna/max)_

# package-ownership report

## Scope and base

Implemented the requested launcher-only ownership correction from exact base
`7ffa8fd77ee9fef70fb5ceba17ca9f08f198637f`.

The final implementation stages the explicitly supplied repository into a
bounded `mktemp` child beneath the declared scratch root, copies with symlinks
preserved without following them, mounts only the staging parent read-only at
the identical absolute path, and passes the staged `repo` child to the
evaluator. The original repository mount was removed. The scratch bind and
`TMPDIR` remain identical. The staging child is removed on Docker success,
failure, or copy failure without changing Docker's exit status. A non-zero
`--uid` must match the invoking `id -u`; `--gid` and `--docker-gid` remain
non-negative-only. Scratch/output overlap checks cover the repository,
evidence, findings, models, environment, and authentication inputs and run
before scratch/output creation or repository copying.

## Changed paths

Only these paths were changed or added:

- `packaging/evaluation/run.sh`
- `tests/test_evaluation_package.py`
- `docs/evaluation-package.md`
- `package-ownership-report.md`

No Dockerfile, package metadata, frozen core module, other test, record,
instruction, credential, or runtime source path was changed. No commit, push,
merge, publication, production operation, Agentflow invocation, delegation,
live model call, network call, or credential lookup was performed.

## Verification commands and results

- `bash -n packaging/evaluation/run.sh` — PASS.
- `shellcheck packaging/evaluation/run.sh` — PASS.
- `PYTHONDONTWRITEBYTECODE=1 python3 -m unittest -v tests.test_evaluation_package` — PASS: 8 tests, 1 skip. The existing fresh-wheel test skipped with `offline wheel builder is not installed`.
- `PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests -p 'test_*.py' -v` — PASS: 94 tests, 1 skip. The same offline wheel-builder skip was reported; the suite emitted existing macOS Git `DARWIN_USER_TEMP_DIR` warnings but no failures.
- `git diff --check` — PASS.
- Frozen hash checks — PASS. The four required SHA-256 values remain `ef6ca77e1466ebbada4d7d8c3c911c18934e673cafa62e8d39dded4d5a3f89bf`, `dac5dcebff8438ef49d1b437faf87eb5f6cc09759ef0656dcbf3c5504e8547e7`, `5fdcfff0a0ffa0714f0681949aba31d7c7b1494b02248fdb51bea798ecd2f73f`, and `1493f337c6afb05921912a0712314859fdefa97f9929e640e650e4b60c453e29`.

The added launcher regressions use the actual invoking UID via `os.getuid()`
and a fake Docker boundary. They cover literal staged-parent/read-only argv,
absence of the original repository mount, copied bytes, non-followed
symlinks, success/failure cleanup, preserved exit status, and root/mismatched
UID plus input-overlap rejection before Docker or copying.

Observed intermediate failures were part of the test-first loop: the first
staging-argv test correctly failed against the pre-change launcher, and the
first copy-boundary run hit the macOS AF_UNIX temporary-path length limit in
the test socket helper. The helper was narrowed to fall back to an existing
socket, after which the final commands above passed.

## Limits

This worker did not run the actual Docker daemon, amd64 image, nested OCI
replay, model, network, credential, or publication path. The wheel test could
not run because no offline wheel builder was installed. Parent-owned runtime
and image evidence was not re-run here. The copy intentionally keeps the
existing repository contract and does not add worktree or submodule handling.
Staging temporarily needs disk space for a second repository copy and relies
on cleanup after the Docker process returns.

Self-check: Scope, source, model/effort, authority and write boundary frozen.
