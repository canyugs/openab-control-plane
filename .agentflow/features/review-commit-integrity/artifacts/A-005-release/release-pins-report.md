* _2026-09-11 09:31:57 (gpt-5.6-luna/max)_

# release-pins

Implemented the bounded A-005 F1/F2 release correction at exact base
`aebf538fe8fa03d50c88946bdbf1a9b637e07145`. I read
`.agentflow/features/review-commit-integrity/artifacts/A-005-release/council-round1.json`
and the A-005 `RUN-002` disposition before editing. The required route is the
single immutable-input correction; no full reproducible-build platform or
adjacent source repair was added.

## Changed paths

- `Dockerfile.evaluation`
- `.github/workflows/evaluation-release.yml`
- `packaging/evaluation/build-requirements.lock`
- `docs/evaluation-package.md`
- `release-pins-report.md`

No evaluator module, launcher, test, `pyproject.toml`, notebook, or unrelated
configuration path was changed.

## Applied pins

All three Docker `FROM` lines retain their familiar tags and now use the
parent-verified index digests:

- `python:3.12-slim-bookworm@sha256:782412e85d0f0984994c290652577d4018aff08145c85b262bb63dc0c7522254`
- `node:22-bookworm-slim@sha256:83f487e0a63425e5b4d146fb5e5be574bcbe1b7b843d3ebafdd95eaf7767a7e5` (both Node stages)

The shared lock contains exactly these release-builder wheels and hashes:

- `setuptools==84.0.0`: `sha256:51a52592b3b99e102b609654876bd65f19f999935166d1352678931132b0c670`
- `pyproject-hooks==1.2.0`: `sha256:9e5c6bfa8dcc30091c74b0cf803c81fdd29d94f01992a7707bc97babb1141913`
- `packaging==26.3`: `sha256:d7193f7c8e4e93f444fde0262bf90af30e16fa0ad0ad44cb553c87339b23cd1c`
- `build==1.6.0`: `sha256:f7aaf1ebbb79178a02ba248bb524f2176b256017e17e8e4bd4289c7b38cc2bad`

CI and the Docker package-build stage install that lock with
`--require-hashes --only-binary=:all:`. CI builds with
`python -m build --no-isolation`; Docker uses
`pip wheel --no-build-isolation --no-deps`. The lock is copied only into the
package-build stage, so final image boundaries and package inputs remain
unchanged. The documentation describes deliberate tag/digest and
version/hash refreshes and records that apt and npm registry resolution still
prevent a promise of bit-for-bit image reproducibility.

## Checks run

- `git diff --check` — PASS.
- Offline lock installation using the allowed interpreter and wheelhouse:
  `/private/var/folders/sd/lyvwlbld52j4b4bptd8yfr9w0000gn/T/ocp-package-validation-pjckyg4w/ci-build-venv/bin/python -m venv /private/tmp/ocp-release-pin-check.0f103s/venv`, followed by `pip install --no-index --find-links /private/var/folders/sd/lyvwlbld52j4b4bptd8yfr9w0000gn/T/ocp-release-lock-qwo250xh --require-hashes --only-binary=:all: --requirement packaging/evaluation/build-requirements.lock` — PASS; all four locked wheels installed and local SHA256 values matched.
- In staged disposable source, `python -m build --wheel --sdist --no-isolation --outdir /private/tmp/ocp-release-pin-check.0f103s/dist` — PASS; one wheel and one sdist produced.
- In the same staged source, `python -m pip wheel --no-index --no-cache-dir --no-build-isolation --no-deps --wheel-dir /private/tmp/ocp-release-pin-check.0f103s/pip-wheel .` — PASS; wheel produced.
- Static assertions over all three exact `FROM` lines, the four lock entries, Docker/CI flags, and documentation boundaries — PASS.
- `git status --short --untracked-files=all` — only the five listed paths are modified or new.

## Failures and limits

- The first disposable-check invocation was rejected before execution by the
  local command safety filter because it included a cleanup `rm -rf`; it was
  rerun without cleanup and passed. The disposable check directory remains
  outside the repository.
- The first static assertion script exited 1 because its checker expected a
  literal `python -m build` string while the workflow correctly invokes the
  configured `$EVALUATION_BUILD_PYTHON`; the corrected assertion passed.
- Build output included the existing minimal-sdist warning about no README;
  the build still passed. Pip also reported a non-writable cache and disabled
  that cache.
- Per the owner boundary, I did not run the parent-owned 96-test rerun,
  tampered-wheel rejection proof, Docker build/image smoke tests, network
  operations, publication, merge, credential access, or model/API calls. No
  Rust changes were made, so Cargo checks were not run.

Self-check: Scope, source, model/effort, authority and write boundary frozen.
