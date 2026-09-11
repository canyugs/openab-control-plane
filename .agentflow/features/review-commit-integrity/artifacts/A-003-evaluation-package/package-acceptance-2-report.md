* _2026-09-10 14:05:35 (gpt-5.6-terra/high)_

# A-003 package acceptance cross-check, start 2

Reviewed implementation commit: ab483e48e99995c90ea2b51bec3432920461a758

I reviewed the original A-003 Ask and approved direction in
`.agentflow/features/review-commit-integrity/review-commit-integrity.devlog.md`,
including its A-003 RUN-021 disposition, rather than inheriting either
historical review verdict. The comparison baseline is
`df101e86fb428d458bf8cd057829de03cd991fb4`. The accepted scope is an
independently installable same-repository Python package/CLI, an independently
versioned image/release lane, and the bounded runner needed to use it. It does
not authorize a new repository, evaluator-semantic change, automatic OCP
integration, deployment, publication, merge, or Stage 3 work.

## Direct implementation and minimality cross-check

The seven approved product paths are `pyproject.toml`, `MANIFEST.in`,
`Dockerfile.evaluation`, `packaging/evaluation/run.sh`,
`.github/workflows/evaluation-release.yml`, `docs/evaluation-package.md`, and
`tests/test_evaluation_package.py` (1,583 added product lines in the frozen
facts). The correction after the earlier defensive finding changes only the
launcher, its focused tests, and its documentation. The four accepted Python
modules have no diff from the baseline and their SHA-256 values are the
test-pinned `ef6ca77e…`, `dac5dceb…`, `5fdcfff0…`, and `1493f337…` values.
Their legacy script entrypoints remain available; the weekly module and its
OCP-specific input schema are unchanged. Consequently, model assessment,
human-truth, estimated-price, and actual-billing limits are preserved rather
than recast by packaging.

Each added concept has a direct owner outcome or reproduced boundary reason:

- Metadata and manifest expose exactly the four existing modules, MIT license,
  no Python runtime dependencies, Python `>=3.9`, Unix boundary, and the two
  approved commands.
- The Dockerfile supplies the approved amd64 runnable image. Its final stage
  receives a built wheel rather than checkout source; it installs usable
  Python, Git, tzdata, Docker CLI, Node 22, and pinned Claude `2.1.266` and
  defaults to nonroot user 65532.
- The separate workflow implements the approved independent package release
  lane and validates build/test/smoke/image before versioned GHCR and GitHub
  artifact steps.
- Dedicated repository staging is narrowly justified by the recorded
  DockerDesktop root-mount/Git-ownership failure. It copies only the explicit
  repository with symlinks preserved into a `mktemp` child, mounts its parent
  readonly, retains the caller UID, preserves identical absolute scratch and
  `TMPDIR` paths, rejects source/storage overlap, cleans up, and returns the
  Docker status. The original checkout, credential file, and host socket are
  not mounted into generated validation children.
- The literal three-key auth parser is the scoped response to the recorded
  unrestricted env-file/import-path failure. It reads no sourceable syntax,
  accepts only `CLAUDE_CODE_OAUTH_TOKEN`, `ANTHROPIC_API_KEY`, and
  `ANTHROPIC_AUTH_TOKEN`, accepts literal values (including spaces, equals,
  dollar signs, backticks, and empty values), rejects unsupported, duplicate,
  and malformed records before storage/staging/Docker, and forwards only
  `--env NAME`. There is no raw `--env-file`, credential value, or auth-file
  path in Docker argv; values exist only in the launcher/Docker process
  environment. Focused tests cover rejected `PYTHONPATH`, `PYTHONHOME`,
  `PATH`, `DOCKER_HOST`, and Git configuration inputs, so the previous
  execution-control forwarding path is closed without broader auth machinery.

No unrelated redesign is present: Dockerfile, metadata, workflow, and core
bytes are unchanged by the auth correction, and the documentation and tests
describe and bound the behavior actually implemented.

## Local checks run in this clone

- `PYTHONDONTWRITEBYTECODE=1 /private/var/folders/sd/lyvwlbld52j4b4bptd8yfr9w0000gn/T/ocp-package-validation-pjckyg4w/ci-build-venv/bin/python -m unittest discover --start-directory tests --pattern 'test_*.py'` — PASS: 96 tests, zero skips. macOS Git temporary-directory warnings occurred without failures.
- `bash -n packaging/evaluation/run.sh`, `shellcheck packaging/evaluation/run.sh`, and product-scoped `git diff --check` — PASS.
- I evaluated the release rule against `evaluation-v0.1.0`, `v0.1.0`,
  `evaluation-v0.1.1`, and `evaluation-v0.1.0-extra`: only the exact matching
  evaluation tag passed. The workflow is disjoint from the existing `v*` Rust
  lane, provisions `build` plus `setuptools>=77`, runs the exact unittest
  command, and does not use PyPI or a `latest` image tag.
- In a new disposable staged source tree, I built wheel and sdist offline with
  the mandated interpreter, installed the wheel with `pip --no-index --no-deps`
  into a new outside-checkout venv, ran both command helps, and imported all
  four modules — PASS. The wheel contained only those modules, license, and
  distribution metadata; the sdist contained package metadata/license and the
  four modules. Setuptools emitted only its conventional missing-README sdist
  warning, consistent with the inline project description and minimal archive.

## Independently inspected host evidence and limits

I read the host reports as evidence, including raw final-suite, distribution,
PTY, image-input-hash, image-check, OCI, weekly/readback, launcher staging,
tag-guard, and new auth-runtime records. They show a real linux/amd64 image
digest, both image entrypoints, Python 3.11.2, Node 22.23.2, Claude 2.1.266,
Docker 20.10.24, Git 2.39.5, and default UID 65532. The eight image inputs
match the final hashes. The actual nested OCI replay records nonroot,
network-none, read-only root, capability drop, no-new-privileges, resource
limits, and no host socket/credential/original-checkout mount in generated
children; its classification remains explicitly unproven.

The final installed CLI weekly/replay and image weekly readbacks are byte-equal
to the accepted baseline. The real PTY auth replay shows unsupported
`PYTHONPATH` rejected with exit 2 before scratch/output creation, while a
dummy-only supported three-key input completed the existing image replay with
exit 0, empty scratch cleanup, clean source, and 42 unchanged artifact hashes
and mtimes. This evidence supports the package and auth boundaries without
claiming a new real provider request.

This review did not invoke Docker, a model, credentials, APIs, network,
publication, release, deployment, merge, or production systems. It does not
prove Linux authenticated model operation, native-amd64-host or multiarch
operation, published GHCR/GitHub artifacts, or any production deployment.
Explicit supported runtime authentication and a dedicated Docker runner remain
operator prerequisites. Parent-recorded `cargo clippy --locked` passed; the
three reported `cargo fmt --check` differences are baseline Rust files and are
not a packaging regression.

Outcome: PASS
Minimality: PASS
Conformance: PASS
Overall: PASS
Verdict: PASS

Self-check: Scope, source, model/effort, authority and write boundary frozen.
