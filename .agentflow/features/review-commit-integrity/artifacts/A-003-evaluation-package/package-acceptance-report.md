* _2026-09-10 13:41:50 (gpt-5.6-terra/high)_

# A-003 package acceptance cross-check

Reviewed implementation commit: 54a0c1a614e29de10ed7decec8cbe0e00e3da46f

I reviewed the original A-003 Ask and accepted direction in
`.agentflow/features/review-commit-integrity/review-commit-integrity.devlog.md`,
the frozen A-003 facts/plan, and the final implementation against baseline
`df101e86fb428d458bf8cd057829de03cd991fb4`. The accepted scope is met: a
same-repository standalone Python distribution/CLI and its independently
versioned image/release lane; no new repository, evaluator-semantic change,
automatic OCP integration, deployment, publication, merge, or Stage 3 work.

## Direct implementation cross-check

The only product paths are the seven frozen paths: `pyproject.toml`,
`MANIFEST.in`, `Dockerfile.evaluation`, `packaging/evaluation/run.sh`,
`.github/workflows/evaluation-release.yml`, `docs/evaluation-package.md`, and
`tests/test_evaluation_package.py`. Their current SHA-256 values match the
final host inventory. `git diff --exit-code` for all four accepted Python
modules produced no diff, and their SHA-256 values are respectively
`ef6ca77e…f89bf`, `dac5dceb…547e7`, `5fdcfff0…f73f`, and
`1493f337…3e29`. Thus the legacy script entrypoints, evaluator semantics,
weekly OCP-specific schema, and model-versus-human-truth/billing limits remain
the accepted bytes.

`pyproject.toml` defines `ocp-review-eval` 0.1.0, the two requested commands,
the four explicit modules, MIT license metadata, Python >=3.9/Unix boundary,
and an empty runtime dependency list. The manifest bounds the source archive
to metadata/license and those four modules. The Dockerfile copies only those
package inputs into the build stage, installs the resulting wheel in the final
stage, pins Claude Code 2.1.266 on Node 22, includes Python/Git/tzdata/Docker
CLI, and defaults to UID 65532. It contains no repository records or
credentials.

The launcher is proportionate to the reproduced Docker Desktop ownership
failure. It rejects unsafe/overlapping paths and root or mismatched caller UID,
copies only the explicit repository into a `mktemp` child of an empty dedicated
scratch root while preserving symlinks, mounts only that child parent read-only,
keeps scratch and `TMPDIR` literal/identical, retains caller UID plus the chosen
socket group, and removes staging while preserving Docker's exit status. Its
generated child execution remains the unchanged evaluator OCI boundary; the
launcher adds no child socket, credential, original-checkout, or host-root
mount. This is a targeted source-staging boundary, not a wider executor
redesign.

The separate workflow triggers only `evaluation-v*.*.*`, validates strict
`evaluation-vX.Y.Z` and exact package-version equality, provisions `build` and
`setuptools>=77` in an isolated builder, builds/tests/smokes wheel and amd64
image before the versioned GHCR/GitHub artifact steps, and neither mentions
PyPI nor publishes `latest`. Existing `release.yml` remains the independent
`v*` Rust/OCP lane.

## Checks run in this clone

- `PYTHONDONTWRITEBYTECODE=1 /private/var/folders/sd/lyvwlbld52j4b4bptd8yfr9w0000gn/T/ocp-package-validation-pjckyg4w/ci-build-venv/bin/python -m unittest discover --start-directory tests --pattern 'test_*.py'` — PASS: 94 tests, zero skips. Existing macOS Git temporary-directory warnings occurred without failures.
- A fresh, explicitly staged temporary source tree was built offline with that
  interpreter using `python -m build --wheel --sdist --no-isolation`; the
  resulting wheel was installed with `pip install --no-index --no-deps` into a
  new venv outside the checkout. Both commands' `--help` and imports of all
  four modules passed. Wheel contents were exactly four modules, license, and
  distribution metadata; sdist contents were only package metadata/license and
  four modules (plus generated setuptools metadata). The non-fatal conventional
  README warning is consistent with the intentionally minimal archive and its
  inline project description.
- `bash -n packaging/evaluation/run.sh`, `shellcheck packaging/evaluation/run.sh`,
  and product-only `git diff --check` — PASS.
- I extracted and ran the workflow's actual validation block: `evaluation-v0.1.0`
  passed; `v0.1.0`, `evaluation-v0.1.1`, and `evaluation-v0.1.0-extra` each
  failed as required. The final checkout remained clean after all checks.

## Evidence inspected and limits

I independently inspected, rather than adopted as a verdict,
`host-validation-report.md`, its final package/distribution inventory, real
PTY readback, image-input hash inventory, image checks, nested OCI result,
launcher ownership/staging diagnostics, final launcher run, weekly readback,
and CI-builder/tag records. They support an actual linux/amd64 image with
Python 3.11.2, Node 22.23.2, Claude 2.1.266, Docker 20.10.24, Git 2.39.5 and
default UID 65532; real installed weekly/replay and image weekly readbacks;
the successful nonroot socket-group-0 staging replay; empty scratch cleanup;
and nested controls retaining `--network none`, read-only filesystem,
nonroot, capability drop, and no host socket/credential/original-checkout
mount. The earlier direct-mount Git ownership failure and the earlier missing
`setuptools.build_meta` CI failure are both concretely recorded and addressed
only by the corresponding staging and builder changes.

No Docker, network, model, credential, publication, deployment, release, or
live authentication action was performed in this review. The evidence does
not prove a Linux authenticated model run, a native-amd64-host or multiarch
release, a running production deployment, or published GHCR/GitHub artifacts.
Those remain explicit operator/runtime prerequisites and are not required for
the approved local packaging outcome.

Outcome: PASS
Minimality: PASS
Conformance: PASS
Overall: PASS
Verdict: PASS

Self-check: Scope, frozen source, model/effort header, authority, write boundary, commands, evidence limits, and required labels verified.
