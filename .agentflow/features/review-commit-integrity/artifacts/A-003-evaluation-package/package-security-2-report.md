* _2026-09-10 14:05:45 (gpt-5.6-sol/low)_

# Package security review, start 2

Reviewed full implementation commit `ab483e48e99995c90ea2b51bec3432920461a758` against comparison baseline `df101e86fb428d458bf8cd057829de03cd991fb4`.

Verdict: PASS

Outcome: PASS

Minimality: PASS

Conformance: PASS

## Findings

No confirmed security finding survives in the reviewed seven-path implementation.

The prior HIGH finding at historical implementation `54a0c1a614e29de10ed7decec8cbe0e00e3da46f` is closed. The current launcher does not give Docker a raw `--env-file`. It reads the external authentication file as literal records, accepts exactly `CLAUDE_CODE_OAUTH_TOKEN`, `ANTHROPIC_API_KEY`, and `ANTHROPIC_AUTH_TOKEN`, rejects unsupported, duplicate, empty-key, and no-`=` records before creating scratch/output or staging, and exports only validated names. Docker receives `--env NAME`; values remain in the launcher's/Docker client's process environment and are absent from Docker argv. Literal spaces, extra `=`, `$`, backticks, quotes, and empty values are preserved without source/eval expansion. In particular, `PYTHONPATH`, `PYTHONHOME`, `PATH`, `DOCKER_HOST`, and `GIT_CONFIG_GLOBAL` can no longer use the credential input to replace the installed entrypoint, model executable, Docker endpoint, or Git configuration.

## Boundary assessment

- Data and cleanup: all input/storage overlap checks, including the auth input, occur before launcher-created directories. Scratch must be empty and dedicated. The launcher creates one unpredictable staging parent beneath scratch, preserves repository symlinks without following them during `cp -R -P`, preserves Docker's status, and its EXIT trap removes only that recorded staging parent. Original input and external output are not cleanup targets.
- Repository and Git: only the explicit repository is copied; the original repository is not mounted. The staging parent is mounted read-only at the identical absolute path. The outer process retains the invoking nonzero host UID, with independently configurable nonnegative primary/socket groups including group `0`; no root user, `safe.directory`, checkout, hook, or generated-code execution bypass was added. The accepted module's inspected Git interface fixes system/global configuration, rejects repository execution configuration, disables hooks, credentials, external diff/textconv, submodule recursion, and ext/file protocols, and resolves both immutable full SHAs before archiving.
- OCI execution: scratch and `TMPDIR` retain identical host/outer paths for sibling-container bind mounts. The host Docker socket is a dedicated outer mount and is not placed in generated-child argv. The unchanged executor gives generated children only materialized source plus a fixed runner, both read-only, with no network, read-only root, nonroot UID, dropped capabilities, no-new-privileges, and resource bounds.
- Credentials and sources: supported model credentials are external runtime input, are neither mounted nor staged nor packaged, and are filtered again by the unchanged adapter environment allowlist. Wheel/sdist selection contains the four approved Python modules plus license/package metadata; the image build uses explicit `COPY` inputs and transfers only the built wheel and pinned CLI runtime into its final stage. The four existing modules are byte-identical and match the pinned SHA-256 values.
- Release: only `evaluation-v*.*.*` tags trigger this independent lane, and the in-job anchored check requires exact `evaluation-vX.Y.Z` syntax and equality with `project.version`. The workflow does not publish `latest` or PyPI. Workflow-level permissions are empty; the sole release job has the necessary `contents: write` and `packages: write`, pinned actions use only the repository token for GHCR and release creation, and no model credential is referenced. The release still resolves build and npm inputs from public registries; this is a declared supply-chain limitation, not evidence of credential/source inclusion.

## Declared evidence read critically

I read the current `host-validation-report.md`, historical first security report, package-auth report, and RUN-021 host disposition, then inspected their accompanying actual package build/install/readback, package and auth PTY, 96-test final result, source/image input digests, image/runtime and installed sibling-OCI results, staging/ownership/Git diagnostics, socket/group-0 evidence, release guard, and auth runtime replay. These records support the repaired wheel filename, installed setuptools backend, unittest discovery, socket group `0`, Docker Desktop ownership staging, unchanged eight image inputs, completed image replay with dummy-only credentials, rejected `PYTHONPATH`, empty scratch after replay, generated-child mount restrictions, and unchanged accepted outputs. I treated them as recorded executions rather than proof by assertion and cross-checked their relevant argv, exits, hashes, and source implementation.

## Commands and results

- `git status --short`; `git rev-parse HEAD`; and `git diff --name-status df101e86fb428d458bf8cd057829de03cd991fb4..ab483e48e99995c90ea2b51bec3432920461a758` — initially clean; HEAD matched the frozen implementation; the seven product paths and accompanying review evidence were enumerated.
- Scoped `git diff`, `sed`, and `rg` inspection of `pyproject.toml`, `MANIFEST.in`, `Dockerfile.evaluation`, `packaging/evaluation/run.sh`, `.github/workflows/evaluation-release.yml`, `docs/evaluation-package.md`, and `tests/test_evaluation_package.py`, plus only boundary-relevant interfaces in the four Python modules — completed.
- `git diff --quiet ... --` for the four modules and `shasum -a 256` — no module changes; hashes were `ef6ca77e...`, `dac5dceb...`, `5fdcfff0...`, and `1493f337...`, matching the test constants and image-input record.
- `PYTHONDONTWRITEBYTECODE=1 /private/var/folders/sd/lyvwlbld52j4b4bptd8yfr9w0000gn/T/ocp-package-validation-pjckyg4w/ci-build-venv/bin/python -m unittest discover --start-directory tests --pattern 'test_*.py'` — PASS, 96 tests, zero failures, zero skips. macOS Git emitted `DARWIN_USER_TEMP_DIR` fallback warnings only.
- `bash -n packaging/evaluation/run.sh` and `shellcheck packaging/evaluation/run.sh` — PASS.
- Seven-product-path `git diff --check` — PASS. The broader commit-wide check reported pre-existing intentional Markdown hard-break whitespace in two generated weekly evidence artifacts outside the seven product paths; no product-path whitespace error was reported.
- Final `git status --short` before report creation — clean.

The seven product changes total 1,583 added lines: the independent package metadata/manifest, image, launcher, release workflow, operator documentation, and package/boundary tests. Each added concept has an owner outcome in the requested package/release/launcher boundary; I found no unrelated source refactor, dependency, evaluator-policy change, or modification to the four accepted modules.

## Limitations

This was a local defensive review. I did not call Docker, a model, an API, or the network; did not read real credentials; and did not publish, merge, commit, push, or operate production. Parent-owned actual image, OCI, PTY, auth, and release-adjacent executions were inspected but not regenerated. No real Linux model authentication is claimed. Concurrent mutation of operator-managed inputs between validation and use and compromise of the trusted host Docker authority remain outside the launcher contract. The only workspace write is this report.

Self-check: Scope, source, model/effort, authority and write boundary frozen.
