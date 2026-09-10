* _2026-09-10 13:24:38 (gpt-5.6-luna/max)_

## package-ci-build

Base: `7ffa8fd77ee9fef70fb5ceba17ca9f08f198637f`.

Changed paths:

- `.github/workflows/evaluation-release.yml`
- `package-ci-build-report.md`

The isolated builder install now provisions `setuptools>=77` alongside `build`, matching `[build-system].requires` in `pyproject.toml`. This is required because `tests/test_evaluation_package.py` prefers the installed `build` module and invokes `python -m build --wheel --no-isolation`; the builder venv therefore needs the backend available in its outer environment. Runtime `dependencies = []`, the evaluation tag guard, independent image/release identity, and existing wheel/test steps are unchanged.

Verification commands and results:

- `yq --version` — passed with yq v4.52.4.
- `yq eval '.' .github/workflows/evaluation-release.yml >/dev/null` — passed YAML parse.
- `yq -r '.jobs.release.steps[] | select(has("run")) | .run' .github/workflows/evaluation-release.yml | bash -n` — passed shell syntax for all workflow `run` blocks.
- `git diff --check -- .github/workflows/evaluation-release.yml` — passed.
- `git diff -- .github/workflows/evaluation-release.yml` — confirmed the single requested install-line change.
- `git rev-parse HEAD` — confirmed the exact base above.

No package tests, wheel builds, live installs, Docker commands, network/API/model/credential operations, delegation, publication, production, merge, commit, or push were performed. Parent owns the clean-environment live tests and imports. `actionlint` and `yamllint` were unavailable; no performed validation failed.

Self-check: Scope, source, model/effort, authority and write boundary frozen.
