* _2026-09-10 13:42:33 (gpt-5.6-sol/low)_

# Package security review

Reviewed full implementation commit `54a0c1a614e29de10ed7decec8cbe0e00e3da46f` against baseline `df101e86fb428d458bf8cd057829de03cd991fb4`.

Verdict: BLOCKING

## Confirmed finding

### HIGH — The credential env file can replace execution inside the Docker-authority container

`packaging/evaluation/run.sh` forwards the operator-provided authentication file directly as Docker `--env-file`. It subsequently fixes `TMPDIR`, `HOME`, `LANG`, and `LC_ALL`, but does not constrain or override `PYTHONPATH`, `PYTHONHOME`, or `PATH`. The image then starts the Python console entrypoint while also mounting the trusted host Docker socket and exposing the authentication variables.

A credential file containing, for example, `PYTHONPATH=<the mounted evidence directory>` can place a replacement `review_model_evaluation.py` in that directory. Python imports that replacement before the installed evaluator module. Likewise, `PATH=<mounted directory>` can replace `claude`, and the existing OCI executor derives its Docker subprocess `PATH` from the outer environment. Either route executes file-supplied code in the outer container, where it has both the runtime credentials and `/var/run/docker.sock`. The accepted adapter's `SAFE_ENVIRONMENT_KEYS` allowlist occurs after Python startup and therefore cannot protect this boundary.

Concrete reproduction: I created disposable repository/evidence/findings/models inputs and an auth file containing `PATH=<evidence>` and a dummy `CLAUDE_CODE_OAUTH_TOKEN`, then invoked the launcher with a fake Docker executable. The launcher exited 0 and its captured arguments contained the auth file. The only later explicit environment values were `TMPDIR`, `HOME`, `LANG`, and `LC_ALL`; no `PATH` override was present. No real Docker or credential was used. The equivalent `PYTHONPATH` value is also unopposed and takes effect before the packaged entrypoint import.

Owner outcome affected: a file intended only to supply supported model credentials can take control of the trusted outer evaluator and its dedicated Docker authority, read those credentials, falsify evaluation output, or use the daemon beyond the generated-child restrictions. This violates the requested credential and execution trust boundaries.

Required outcome: do not pass an unrestricted env file to the outer container. Parse and validate it outside Docker and forward only the evaluator's supported authentication keys (`CLAUDE_CODE_OAUTH_TOKEN`, `ANTHROPIC_API_KEY`, and `ANTHROPIC_AUTH_TOKEN`), rejecting duplicate/unsupported keys and malformed records, or use an equivalently strict credential-only mechanism. Add a launcher test proving variables such as `PYTHONPATH`, `PYTHONHOME`, `PATH`, `DOCKER_HOST`, and Git configuration variables cannot reach the outer process.

## Boundary observations without findings

- The four accepted Python modules are byte-identical to the baseline and match the test-pinned SHA-256 values. No `safe.directory` or root bypass was added.
- The launcher validates full SHA arguments and absolute non-symlink input/storage paths, rejects input/storage overlap before creating storage, requires a matching non-root host UID, permits group 0 where needed, stages only the explicit repository with symlinks preserved rather than followed, mounts the staging parent read-only, preserves Docker's exit status, and removes only its `mktemp` staging child.
- The original repository is not mounted into the outer evaluator. The staged repository remains available for the accepted Git validation and archive operations, while generated OCI children receive only materialized source and the fixed runner read-only; their existing non-root, no-network, read-only-root, capability-drop, resource-limit, and environment controls remain unchanged. The host socket is not included in generated-child arguments shown by the actual image evidence.
- Wheel/sdist configuration selects only the four modules plus license/metadata and declares no runtime Python dependencies. The final image uses explicit Dockerfile `COPY` inputs and installs the wheel without an index. No repository, test, record, or credential path is copied into the final stage by the Dockerfile.
- The release trigger and in-job guard require exact `evaluation-vX.Y.Z` syntax and equality with the package version. Workflow permissions are empty globally and scoped to `contents: write` and `packages: write` for the release job; actions are commit-pinned, and only the GitHub token is used for GHCR/release publication. The release build still resolves build and npm inputs from external registries; that is a release supply-chain limitation, not a locally exercised failure in this review.

## Declared evidence inspected

I treated `host-validation-report.md` as a declaration and inspected its accompanying package build/readback and distribution inventories, package PTY/final-suite results, image input/build/runtime checks, installed sibling-OCI result, ownership/staging and launcher diagnostics, release guard result, and implementation digest records. They support the reported wheel filename/backend/discovery repairs, non-root UID with socket group 0, Docker Desktop staging ownership correction, successful cleanup, bounded distribution contents, and unchanged source hashes. They do not exercise an auth env file containing unsupported execution-control variables and therefore do not rebut the finding above.

## Commands and results

- `git status --short`; `git rev-parse HEAD`; and `git diff --name-status df101e86fb428d458bf8cd057829de03cd991fb4..54a0c1a614e29de10ed7decec8cbe0e00e3da46f` — clean initial worktree; HEAD matched the frozen implementation; implementation/evidence paths enumerated.
- Scoped `git diff` and `sed`/`rg` inspection of the seven product paths, declared evidence, and only the required interfaces in the four Python modules — completed.
- `git diff --exit-code ... --` for the four Python modules and `sha256sum` for those modules — no module diff; all hashes matched the package test constants.
- `PYTHONDONTWRITEBYTECODE=1 /private/var/folders/sd/lyvwlbld52j4b4bptd8yfr9w0000gn/T/ocp-package-validation-pjckyg4w/ci-build-venv/bin/python -m unittest tests.test_evaluation_package` — PASS, 8 tests, 0 failures, 0 skips.
- `bash -n packaging/evaluation/run.sh` — PASS.
- `shellcheck packaging/evaluation/run.sh` — PASS.
- Product-scoped `git diff --check` — PASS.
- Disposable fake-Docker auth-env argument reproduction — PASS as a reproduction: launcher exit 0, env file forwarded, explicit environment was only `TMPDIR`, `HOME`, `LANG`, and `LC_ALL`, and `PATH` was not overridden.

## Limitations

This was a local, read-only defensive review. I did not call Docker, a model, an API, or the network; did not inspect credentials; and did not run actual OCI/model/release publication. The parent-owned actual image, installed package/PTY, final test, source digest, launcher, and CI evidence was inspected but not independently regenerated. I did not expand into the accepted evaluator algorithms or unrelated product policy. The only workspace change is this report; no source, configuration, test, documentation, record, commit, push, merge, publication, or production operation was performed.

Self-check: Scope, source, model/effort, authority and write boundary frozen.
