* _2026-09-11 09:38:55 (gpt-5.6-terra/high)_

# release-crosscheck

Reviewed implementation commit: 4b9ea56f0fd7466685ee57b196f6947c7de713a7

## Scope and source

This is an independent, read-only cross-check of the frozen implementation
against comparison baseline `db4c91f373a8fa69189a3fdeee3dc99e96891ee9`.
The product delta is exactly these four paths and 50 changed lines (44 added,
6 removed):

- `Dockerfile.evaluation`
- `.github/workflows/evaluation-release.yml`
- `packaging/evaluation/build-requirements.lock`
- `docs/evaluation-package.md`

`git diff --check` passed. No evaluator, launcher, runtime dependency,
`pyproject.toml`, or evaluator semantic change is present. The unchanged
`[build-system]` minimum remains `setuptools>=77` for ordinary source
consumers.

## Direct reconstruction

F1 is resolved by all three immutable `FROM` references. The current Python
stage is exactly `python:3.12-slim-bookworm@sha256:782412e85d0f0984994c290652577d4018aff08145c85b262bb63dc0c7522254`;
both Node stages are exactly
`node:22-bookworm-slim@sha256:83f487e0a63425e5b4d146fb5e5be574bcbe1b7b843d3ebafdd95eaf7767a7e5`.
They match the recorded identities in `base-image-digests.json` (the Node
identity deliberately covers two stages).

F2 is resolved by the shared release-only lock. Direct SHA-256 calculation of
the four supplied wheelhouse files matches every locked value: setuptools
84.0.0 `51a525...0c670`, pyproject-hooks 1.2.0 `9e5c6b...41913`, packaging
26.3 `d7193f...3cd1c`, and build 1.6.0 `f7aaf1...2bad`. CI and the Docker
package-build stage both install that same file with
`--require-hashes --only-binary=:all:`. CI then uses `python -m build
--no-isolation`; Docker then uses `pip wheel --no-build-isolation --no-deps`.
Neither release path therefore asks an isolated backend to resolve build
dependencies afterward.

The Docker source copies the lock only to `package-build`; the final stage
receives only the generated wheel and removes its temporary wheelhouse. The
raw locked package-build evidence lists neither lock nor evidence/record files
in the wheel or sdist, and the independently rebuilt archives likewise had no
lock, `.agentflow`, secret, or credential-named path. Static final-stage review
finds no repository source, lock, evidence, or credential copy. The raw image
build/smoke evidence records a successful amd64 build and non-network CLI
smokes; it was inspected but not rerun, as Docker is outside this review's
authority.

The documentation is traceable to F1/F2 only: it describes deliberate
tag/digest and wheel-version/hash refresh, the common locked non-isolated
toolchain, and expressly limits the claim because `apt` and `npm` still
resolve upstream inputs. It does not claim bit-for-bit reproducibility.

## Evidence inspected and checks reproduced

I read the required raw `pins-host-verification.md`,
`tampered-builder-rejected.json`, `locked-builder-install.json`,
`locked-package-build.json`, `pinned-image-inputs.json`,
`pinned-image-build.json`, `pinned-image-smoke.json`, and
`pins-full-tests.json`, plus the necessary recorded registry and wheel hash
maps. Current SHA-256 values for all nine recorded image inputs exactly match
`pinned-image-inputs.json`.

Using the allowed interpreter and `PYTHONDONTWRITEBYTECODE=1`, the following
completed successfully:

- `/private/var/folders/sd/lyvwlbld52j4b4bptd8yfr9w0000gn/T/ocp-release-validation-qna7shwe/venv/bin/python -m unittest discover --start-directory tests --pattern 'test_*.py'` — 96 tests, 0 skips, 9.918s.
- A fresh disposable venv installed the current lock from
  `/private/var/folders/sd/lyvwlbld52j4b4bptd8yfr9w0000gn/T/ocp-release-lock-qwo250xh`
  with `--no-index --find-links ... --require-hashes --only-binary=:all:` —
  all four wheels installed.
- A copied disposable source built one wheel and one sdist with
  `python -m build --wheel --sdist --no-isolation`; archive inspection passed.
- A fresh disposable venv rejected a deliberately byte-tampered build wheel
  under the same hash-required lock (exit 1, expected versus actual SHA-256
  mismatch). This independently confirms the negative control reported in
  `tampered-builder-rejected.json`.

The full-suite run emitted host `git confstr` warnings; pip emitted its
non-writable-cache warning; and sdist emitted the pre-existing missing-README
warning. None is a test or release-input failure. An initial combined
disposable command was rejected before execution because it contained a
cleanup command; the rerun above retained its disposable directory and
passed. One read-only PR-evidence attempt temporarily assigned zsh's special
`path` variable and therefore did not run its later readers; it made no
change and was rerun successfully with a different variable name.

## F3/F4 boundary and owner effect

Local inspection of PR base `16aa2d46b0be72a35100e57c8c934bf0835adef4`
confirms that all four evaluator modules are absent there. Thus honest PR-base
wording must call them new relative to main, while limiting the
"unchanged-by-packaging" statement to this four-file correction. The external
PR body is not locally available and GitHub access is prohibited here, so its
final readback remains an owner action before merge/publication; it is not a
reproducible blocker for accepting this completed code correction. F4 is
correctly treated as a host inventory/readback observation, not a claim that
this patch implements mtime restoration; no such source change was added.

Outcome: PASS
Minimality: PASS
Conformance: PASS
Overall: PASS
Verdict: PASS

Self-check: Scope, source, model/effort, authority and write boundary frozen.
