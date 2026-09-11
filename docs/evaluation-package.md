# Standalone review-evaluation package

`ocp-review-eval` version `0.1.0` packages the four existing offline Python
modules as a small Unix-only distribution:

- `review_model_evaluation`
- `review_model_adapters`
- `review_model_oci_executor`
- `review_round_weekly_report`

The package has no third-party runtime Python dependencies. Python `>=3.9` is
required because the source uses `zoneinfo`, `str.removesuffix`, and built-in
generic types. The subprocess and OCI boundaries require a Unix host with
`git` and the Docker CLI when evaluation controls are enabled. The source
files remain available through their legacy `python3 scripts/...py` calls.

## Install and run from a wheel

Build or obtain the versioned wheel, then install it into an isolated virtual
environment. The following uses only the local wheel and performs no package
index lookup:

```sh
python3 -m venv /tmp/ocp-review-eval-0.1.0
/tmp/ocp-review-eval-0.1.0/bin/python -m pip install \
  --no-index --no-deps \
  /srv/ocp-review-eval/dist/ocp_review_eval-0.1.0-py3-none-any.whl

mkdir -p /tmp/ocp-review-eval-run
cd /tmp/ocp-review-eval-run
/tmp/ocp-review-eval-0.1.0/bin/ocp-review-eval run \
  --repo /srv/review-input/repository \
  --revision 0123456789abcdef0123456789abcdef01234567 \
  --base fedcba9876543210fedcba9876543210fedcba98 \
  --findings /srv/review-input/findings.json \
  --evidence /srv/review-input/evidence \
  --models /srv/review-input/models.json \
  --environment /srv/review-input/environment.json \
  --output /srv/review-output/evaluation-0.1.0
```

The repository, input files, evidence, model profiles, and output are external
operator data. `models.json` retains the shipped adapter choices (`claude` or
the explicitly gated `codex`) and Claude's `oauth` default or explicit `bare`
transport. No credentials are read from package data.

For a Claude run, provide authentication to the process at runtime, for
example through an operator-managed environment file or shell environment:

```sh
export CLAUDE_CODE_OAUTH_TOKEN='provided-at-runtime'
/tmp/ocp-review-eval-0.1.0/bin/ocp-review-eval run \
  --repo /srv/review-input/repository \
  --revision 0123456789abcdef0123456789abcdef01234567 \
  --base fedcba9876543210fedcba9876543210fedcba98 \
  --findings /srv/review-input/findings.json \
  --evidence /srv/review-input/evidence \
  --models /srv/review-input/models.json \
  --output /srv/review-output/evaluation-0.1.0
```

The adapter's supported authentication variables are
`CLAUDE_CODE_OAUTH_TOKEN`, `ANTHROPIC_API_KEY`, and
`ANTHROPIC_AUTH_TOKEN`. Authentication remains with the installed Claude CLI.
The dedicated launcher described below accepts only those three names in its
authentication file; that option is not a general runtime-environment or
program-loading configuration channel. Do not put secrets in the repository,
wheel, evidence, model files, or output.

The weekly command consumes an operator-captured bundle without a model or
OCI call:

```sh
/tmp/ocp-review-eval-0.1.0/bin/ocp-review-weekly \
  --bundle /srv/review-input/review-round-weekly-evidence \
  --week 2026-W37 \
  --as-of 2026-09-09T08:10:00+08:00 \
  --output /srv/review-output/weekly-2026-W37
```

The weekly bundle schema remains OCP-specific: this command consumes the
existing OpenAB Control Plane evidence-export contract rather than defining a
generic review-bundle schema.

## Release build inputs

The release workflow and the Docker package-build stage share
`packaging/evaluation/build-requirements.lock`. It contains the exact
`setuptools` 84.0.0, `pyproject-hooks` 1.2.0, `packaging` 26.3, and `build`
1.6.0 wheels, each with its approved SHA256 hash. Both release paths install
that lock with `--require-hashes --only-binary=:all:` and then use the verified
toolchain without isolated build dependency resolution (`python -m build
--no-isolation` in CI and `pip wheel --no-build-isolation --no-deps` in the
Docker package-build stage). Normal source consumers continue to use the
minimum build-system requirement in `pyproject.toml`; this lock is only for
the release paths.

Refresh these inputs deliberately as one reviewed change: select the new base
image tag and verify its registry index digest, then update the corresponding
tag-plus-digest `FROM` line; for the Python builder, update each exact wheel
version and its SHA256 hash in the lock together. Re-run the release checks
and record the new identities in the release report. The pins make these
selected image and Python builder inputs auditable, but do not promise a
bit-for-bit reproducible image: Debian packages installed by `apt` and the
Claude CLI installed by `npm` still resolve through their upstream registries.

## Versioned image

The release workflow uses the independent package tag
`evaluation-v0.1.0` and publishes the versioned image
`ghcr.io/canyugs/ocp-review-eval:0.1.0`. It does not publish a `latest` image
or a PyPI release. The first image lane is tested and published for
`linux/amd64` only; no multi-architecture support is claimed.

Local build and test work has not published the wheel, sdist, or image. The
image and package names above describe the independent release workflow, not a
claim that these artifacts are currently available from a registry or package
index.

The image's default entrypoint is the evaluator (`ocp-review-eval`) with the
legacy `run` subcommand. Its image contains Python in an isolated virtual
environment, Git, timezone data, the Docker CLI, Node 22, and the pinned
Claude CLI `2.1.266`. The image build copies only the wheel into its final
stage; repository source, tests, records, and credentials are not copied.

For an offline weekly report, override the entrypoint and use read-only input
and a writable external output directory:

```sh
docker run --rm --network none \
  --user "$(id -u):$(id -g)" \
  --mount type=bind,src=/srv/review-input/review-round-weekly-evidence,dst=/srv/review-input/review-round-weekly-evidence,readonly \
  --mount type=bind,src=/srv/review-output/weekly-2026-W37,dst=/srv/review-output/weekly-2026-W37 \
  --entrypoint ocp-review-weekly \
  ghcr.io/canyugs/ocp-review-eval:0.1.0 \
  --bundle /srv/review-input/review-round-weekly-evidence \
  --week 2026-W37 \
  --as-of 2026-09-09T08:10:00+08:00 \
  --output /srv/review-output/weekly-2026-W37
```

The weekly command needs no host Docker socket or credentials. Its output
directory must be new or empty as required by the existing report contract.

## Dedicated Docker-socket evaluation runner

The evaluation controller creates its source tree, fixed runner, and process
working paths with `tempfile`. When it runs inside an outer container while
using a host Docker socket, those paths must be visible to the Docker daemon
as the same absolute paths. Use the small launcher in
`packaging/evaluation/run.sh`; it creates/validates only the requested scratch
and output directories, requires an empty scratch directory, canonicalizes
non-symlink paths, and forwards the literal scratch bind and `TMPDIR` value.
Before invoking Docker, it creates a bounded `mktemp` staging child under the
scratch directory and copies only the supplied repository into its `repo`
child. The copy preserves symlinks without following them and does not execute
repository code, Git, or hooks. Only the staging parent is mounted read-only at
the identical absolute path; the original repository is not mounted. The
staging directory is removed after Docker succeeds or fails, while Docker's
exit status is preserved. This temporarily requires disk space for a second
copy of the repository.

The launcher rejects scratch or output paths that overlap the repository,
evidence, findings, models, environment, or authentication input paths before
creating either directory or copying the repository. Scratch must remain an
empty dedicated root. `--uid` must be the invoking non-zero host UID (the value
reported by `id -u`); `--gid` and `--docker-gid` may be any non-negative
numeric IDs, including `0`.

The following exact invocation assumes all input paths already exist and
`/srv/ocp-review-eval` is a dedicated runner root whose parent directories are
operator-created:

```sh
packaging/evaluation/run.sh \
  --scratch-dir /srv/ocp-review-eval/scratch \
  --output-dir /srv/ocp-review-eval/output \
  --repo /srv/review-input/repository \
  --revision 0123456789abcdef0123456789abcdef01234567 \
  --base fedcba9876543210fedcba9876543210fedcba98 \
  --findings /srv/review-input/findings.json \
  --evidence /srv/review-input/evidence \
  --models /srv/review-input/models.json \
  --environment /srv/review-input/environment.json \
  --auth-env-file /srv/review-input/claude.env \
  --docker-gid 998 \
  --uid "$(id -u)" \
  --gid "$(id -g)" \
  --image ghcr.io/canyugs/ocp-review-eval:0.1.0
```

`claude.env` is an external launcher authentication file, for example with a
runtime-only `CLAUDE_CODE_OAUTH_TOKEN=...` entry. It is read once as literal
text before scratch/output creation, staging, or Docker invocation. It is
never mounted, copied, or passed through Docker's `--env-file` option. After
the entire file validates, the launcher exports only the explicitly parsed
allowlisted names and gives Docker `--env NAME` entries; credential values
remain out of Docker's command-line arguments and are not written to files.

The supported authentication-file format is intentionally narrower than a
generic Docker env-file:

- Use LF-delimited `KEY=VALUE` records; a final record may omit its newline.
- Empty lines and lines whose first non-space character is `#` are ignored.
- The key must be exactly one of `CLAUDE_CODE_OAUTH_TOKEN`,
  `ANTHROPIC_API_KEY`, or `ANTHROPIC_AUTH_TOKEN`, and each key may appear only
  once.
- The value is every byte after the first `=`. Leading/trailing spaces,
  additional `=`, `$`, backticks, quotes, and an empty value are literal; no
  quoting, escaping, or shell expansion is performed. Do not add whitespace
  around the key or `=`.

An omitted `--auth-env-file`, or a file containing only blanks/comments,
forwards no authentication variables. A supported `KEY=` record forwards that
key with an empty value. Unsupported keys, duplicate keys, and malformed
records fail before Docker is invoked or launcher-created scratch/output or
staging paths exist, and validation errors do not print values or whole input
records. `--docker-gid` must be the numeric group owning the dedicated host
socket as seen inside the outer Linux container; that group can be `0` on
Docker Desktop. `--uid` must be non-zero to retain a nonroot process, while
`--gid` and `--docker-gid` accept nonnegative numeric IDs that can write the
external scratch and output directories. The launcher accepts a non-default
socket only when `--docker-socket` names an existing Unix socket, and always
maps it to `/var/run/docker.sock` inside the outer image.

The host Docker socket is a trusted orchestration authority. Run this launcher
only on a dedicated runner with an explicitly provisioned daemon and socket
group. The outer evaluator gets the socket; generated validation containers
created by the existing OCI executor receive no socket, repository checkout,
credential mount, host-root mount, privileged mode, or host networking. The
launcher does not provision a daemon, add DinD, or alter the executor.

## Limits and truth boundaries

- The package is a standalone offline controller/report tool, not Rust/OCP
  runtime code and not an automatic OCP integration.
- `git`, a pinned local OCI image from the evaluation environment, and a
  usable Docker daemon are operator prerequisites for the corresponding
  stages. The package does not install a daemon or log in to providers.
- A Linux image cannot use a macOS keychain. No claim is made that macOS
  keychain OAuth state is available inside this image; provide supported
  runtime authentication explicitly.
- A Linux model-auth run still requires explicit runtime credentials. Existing
  host-model checks and local package/OCI checks do not imply that model auth
  works inside the Linux image.
- Model identity, human truth, list-price estimates, and actual provider
  billing remain separate. Unknown billing is not converted into an estimate,
  and no verdict, roster, routing, GitHub, scheduler, or production behavior
  is changed by packaging.
- The package release is independent of Cargo versioning. Existing
  `release.yml` responds to `v*` Rust/OCP tags; only
  `.github/workflows/evaluation-release.yml` responds to strict
  `evaluation-vX.Y.Z` tags.
