* _2026-09-10 12:59:30 (gpt-5.6-luna/max)_

# Package implementation report

## Authority and frozen scope

- Exact base: `df101e86fb428d458bf8cd057829de03cd991fb4`.
- Model/effort: configured `codex-default`, `gpt-5.6-luna/max`; no live model
  call, credential read, API call, network access, Agentflow invocation, or
  delegation was used.
- Work stayed in the disposable no-remote clone. No commit, push, merge,
  publication, production operation, Rust/OCP source edit, existing test/doc
  edit, README edit, settings edit, notebook edit, or memory edit was made.
- The four accepted modules were preserved byte-for-byte. Their final
  SHA-256 values are the base values:
  `review_model_evaluation.py`
  `ef6ca77e1466ebbada4d7d8c3c911c18934e673cafa62e8d39dded4d5a3f89bf`,
  `review_model_adapters.py`
  `dac5dcebff8438ef49d1b437faf87eb5f6cc09759ef0656dcbf3c5504e8547e7`,
  `review_model_oci_executor.py`
  `5fdcfff0a0ffa0714f0681949aba31d7c7b1494b02248fdb51bea798ecd2f73f`, and
  `review_round_weekly_report.py`
  `1493f337c6afb05921912a0712314859fdefa97f9929e640e650e4b60c453e29`.

## Implementation

- Added root setuptools metadata for independent package `ocp-review-eval`
  version `0.1.0`, Python `>=3.9`, Unix classifiers/platform, no runtime
  dependencies, explicit `scripts` package directory, the four explicit
  `py-modules`, MIT license metadata, and the two requested console commands.
- Added an explicit bounded `MANIFEST.in`. A full-checkout sdist check showed
  that setuptools otherwise included tests; `global-exclude *` now leaves only
  package metadata, the four modules, and `LICENSE` in the source distribution.
- Added `Dockerfile.evaluation`: multi-stage wheel build, official
  `node:22-bookworm-slim`, isolated Python venv, Git, timezone data,
  `docker.io`, CA certificates, and exact
  `@anthropic-ai/claude-code@2.1.266`. The final image has the non-root
  `evaluator` user, evaluator entrypoint with default `run`, no repository
  source/records/credentials, and no privileged DinD setup.
- Added `packaging/evaluation/run.sh`. It validates absolute non-symlink input
  paths, creates only dedicated scratch/output directories, requires scratch
  to be empty, requires numeric non-zero UID/GID and a Unix Docker socket, and
  forwards the scratch directory at the identical outer-container path with
  `TMPDIR`. It mounts the host socket only in the outer container and passes
  no socket to generated controls.
- Added `docs/evaluation-package.md` with exact wheel, image, weekly entrypoint,
  credential, and dedicated-runner examples. It records `evaluation-v0.1.0`,
  `ghcr.io/canyugs/ocp-review-eval:0.1.0`, the amd64-only limit, explicit auth
  and daemon prerequisites, the Linux/macOS keychain limitation, and the
  unchanged model/human/billing truth boundaries.
- Added `.github/workflows/evaluation-release.yml`, triggered only by the
  `evaluation-v*.*.*` tag shape and guarded by an exact `evaluation-vX.Y.Z`
  check. It matches the tag to `pyproject.toml`, builds wheel/sdist, runs the
  Python suite and fresh-wheel smoke, builds/tests a linux/amd64 image, pushes
  only the versioned GHCR tag, and uploads wheel/sdist to the matching GitHub
  release with `--latest=false`; it does not use PyPI or an image `latest`
  tag.
- Added `tests/test_evaluation_package.py` and retained its tests-only red
  patch as `red-packaging.patch`.

## Commands and results

- `git status --short`, `git rev-parse HEAD`: clean starting tree and exact
  base above.
- Initial red run `python3 -m unittest tests.test_evaluation_package -v`:
  expected missing-`pyproject.toml`/launcher errors; the offline builder was
  skipped because setuptools/build was not installed in the default host
  interpreter. A later pre-fix launcher run also exposed the host sandbox's
  inability to create a Unix socket; the test now uses an available socket or
  skips only when none exists.
- `PYTHONPATH=/Users/can/.cache/uv/archive-v0/N3wtGwolBcXKx1bkTwxly python3 -m unittest tests.test_evaluation_package -v`:
  5 tests passed, including offline wheel build, wheel install outside the
  checkout, all imports, both console `--help` entrypoints, and launcher
  forwarding/rejection checks.
- The same cached local setuptools path with
  `python3 -m unittest discover -s tests -p 'test_*.py'`: 91 tests passed in
  8.478 seconds.
- The same cached local setuptools path built a wheel in a temporary staging
  copy using `pip wheel --no-deps --no-build-isolation`; wheel modules/license
  and clean-venv installation checks passed. It also built a full-checkout
  temporary sdist through `setuptools.build_meta.build_sdist`; required module
  and license checks passed and tests/docs/Rust/record paths were absent. The
  sdist emitted only the non-fatal setuptools warning that no conventional
  README file was present.
- The first Ruby YAML parse of the workflow failed at line 46 because a
  quoted executable was used as a plain scalar; both variable-prefixed `run`
  commands were changed to block scalars and the parser then passed. An
  initial diff-check loop also used zsh's special `path` variable and was
  rerun with a non-special name; no source or output content was affected.
- `PYTHONPYCACHEPREFIX=<temporary cache> python3 -m compileall -q` over all
  four modules and the new packaging test: passed; temporary bytecode was
  removed.
- `bash -n packaging/evaluation/run.sh`, `shellcheck packaging/evaluation/run.sh`,
  Ruby YAML parsing of the new workflow, and `tomllib` parsing/metadata checks
  for `pyproject.toml`: passed.
- `git diff --check` plus `git diff --no-index --check` for every new allowed
  file: passed. `git apply --check --reverse red-packaging.patch`: passed.
- No Docker/image build, npm registry install, nested OCI run, live Claude or
  Codex invocation, authentication check, daemon provisioning, network check,
  GHCR push, GitHub release upload, PyPI action, or Cargo test was run here.
  Those are explicit parent/operator checks and are not represented as green.

## Final write scope and limits

The only task outputs are:

`.github/workflows/evaluation-release.yml`, `Dockerfile.evaluation`,
`MANIFEST.in`, `docs/evaluation-package.md`, `packaging/evaluation/run.sh`,
`pyproject.toml`, `tests/test_evaluation_package.py`,
`package-implementation-report.md`, and `red-packaging.patch`.

The image workflow intentionally claims linux/amd64 only. A Docker-socket
runner remains a dedicated-host trust boundary: the operator must provision
the daemon, socket group, external data, and supported Claude authentication.
The launcher does not prove that a host is dedicated, does not install a
daemon, and does not change the existing OCI executor. The package remains
standalone and does not integrate automatically with the Rust/OCP runtime.

Self-check: Scope, source, model/effort, authority and write boundary frozen.
