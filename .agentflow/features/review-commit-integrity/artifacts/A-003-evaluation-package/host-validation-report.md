* _2026-09-10 14:02:42 (GPT-6/default)_

# Host standalone package validation

Owner A-003 approved same-repo independent package/CLI, image and version/release. The exact seven product path digests are in host-validation-result.json; baseline is df101e86fb428d458bf8cd057829de03cd991fb4. All four accepted Python modules and Rust source remain unchanged. This report records coordinator-executed evidence before independent review; it does not claim a review verdict or release.

## Delivered behavior and actual journey

- ocp-review-eval0.1.0 has explicit four-module setuptools metadata, no runtime Python dependencies and two installed commands. Final wheel/sdist were built from staged delivered metadata, wheel built from that sdist, then installed in a new venv outside the checkout. Imports and both CLI helps pass. final-distribution-inventory.json lists only modules/license/metadata; host-final-package-build.json records actual commands. The conventional-README sdist warning is non-fatal; inline package description is present.
- Final installed weekly command ran in reusable actual PTY90890 and generated JSON byte-identical to the accepted synthetic safe baseline. Complete evaluation replay used the installed CLI with existing immutable evidence. Final-wheel/readback files and package-pty.json preserve commands, terminal identity, visible input/output and exits.
- Actual linux/amd64 image build passed, digest sha256:38a45068982b62ad6999173f358b5f468f7e34f06f5dae9f4dd938a94012ab0b, under DockerDesktop emulation on an arm64 host. All eight image build input hashes match final source. Image user65532, Python3.11.2, Node22.23.2, Claude2.1.266, Docker20.10.24 and Git2.39.5 verified. Both image CLI entrypoints work.
- Trusted host-authored host-oci-probe.py ran inside that installed image as UID501 with socketgroup0. It materialized accepted source and replayed the existing model-generated negative-quantity plan in fresh sibling OCI controls. Actual controls passed; classification remains unproven, preserving semantic truth boundaries. installed-image-oci-result.json and host-image-check.json retain actual flags/results. Generated validation runs receive no host socket/credentials and use the existing nonroot/network-none/read-only execution controls.
- Final shipped launcher ran in the same real PTY against the actual image and complete safe evaluation, exit0. It stages only the supplied repository, readonly-mounts its dedicated parent, uses identical scratch/TMPDIR absolute paths and cleans staging. Final scratch is empty. Original repo is clean; all42 existing evaluation file digests and mtimes are unchanged. PTY closed with exit0.
- Image weekly command ran network-none in that PTY and produced the same baseline JSON. Input schema remains OCP-specific.

## Reproduced failures and scoped corrections

The first candidate renamed its wheel to an invalid filename; pip rejected the real image build. Final Dockerfile preserves its wheel basename. CI's top-level unittest option failed importability; corrected actual command passes. Fresh CI builder with only build failed to import setuptools.build_meta; adding only setuptools>=77 made all package tests pass. Matching, mismatching and malformed evaluation tag guards were executed; release YAML and run-block shell syntax pass.

Socketgroup20 could not access the DockerDesktop socket; group0 succeeds with nonrootUID501. Direct repository bind exposed repo rootUID0 and .gitUID501, and Git correctly rejected ownership. Dedicated child staging solved the actual failure. Original Git global/system isolation, root avoidance, core bytes and generated-executor boundaries remain unchanged. Parent candidate and final launcher success/cleanup are separately recorded; the final file includes additional explicit preflight/help checks and was rerun.

## Authentication-channel correction and actual final readback

The first defensive review identified unrestricted Docker env-file forwarding; the first acceptance PASS at54a0c1a is historical. Parent auth-import-proof.json confirmed Python import replacement using only a trusted dummy marker, network-none and no socket. This does not establish attacker control of the operator-owned auth file or any actual compromise.

The final launcher parses only three supported auth keys as literal values, validates before storage/staging/Docker, never sources or evaluates the file, and exports only parsed allowed values in memory. Docker receives only --env NAME arguments, never raw --env-file or credential values in argv. New tests cover literal spaces/equals/dollar/backtick/empty values and unsupported/duplicate/malformed rejection.

Parent reran the full96 tests with zero skips, bash syntax, shellcheck and product whitespace checks. Actual reusable PTY31873 (/dev/ttys026) ran the shipped launcher with dummy credentials: PYTHONPATH input exited2 before creating scratch/output; supported three-key input replayed the complete evaluation in the installed image with exit0. Scratch cleaned, source clean, all42 original evidence hashes and mtimes unchanged. package-auth-pty.json and host-auth-runtime.json preserve actual outputs; PTY exited0. No live provider request or credential was used. All eight image/core input hashes remain identical, so the earlier actual wheel/image/OCI/weekly proofs remain applicable without a redundant rebuild.

## Final checks and limits

Host final full suite:96 tests PASS, zero skips, in the clean CI-equivalent venv with build and setuptools>=77. Bash syntax, shellcheck, product-only diff checks and release guards PASS. cargo clippy --locked PASS. cargo fmt --check reports three pre-existing formatting differences in unchanged src/state.rs, src/store/postgres.rs and tests/second_consumer.rs. No unrelated Rust formatting repair was made. Raw weekly Markdown artifacts intentionally retain their baseline two-space line breaks, which default git diff --check flags; product whitespace checks are clean.

Python package declares Unix and >=3.9 based on its existing standard-library surface; actual host tests used3.14 and image tests3.11. No untested multiarch release or native amd64-host claim. Staging temporarily requires a second repository copy; launcher must run under its intended nonroot host account with matching UID and suitable socket group. It does not add worktree/submodule support or provision a Docker daemon. Run on a dedicated Docker runner.

Linux model authentication was not exercised: explicit supported runtime credentials remain required. Existing accepted host model/OAuth evidence remains separate; no new model/human truth or billing claim. No GitHub release, GHCR/PyPI publication, production deployment, merge, auto-trigger/export/scheduler, or Stage3 comparison. Original main-checkout README.md/docs/flow.md and two PLAN files remain untouched.

Self-check: Actual commands, immutable source/evidence boundaries, failures and current limits verified; independent reviews and final delivery remain pending.
