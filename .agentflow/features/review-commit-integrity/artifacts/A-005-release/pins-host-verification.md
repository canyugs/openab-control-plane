* _2026-09-11 09:34:42 (GPT-6/default)_

# Release input correction verification

Council F1/F2: actual all3 FROM tags were mutable; builder installs used floating ranges and isolated re-resolution. Approved merge/publication needs these required-check findings resolved. The four-file correction pins existing base index digests and a shared exact SHA256 builder lock, disables isolated dependency resolution, and documents limited guarantees. No evaluator/launcher/metadata/other workflow change occurred.

Host verified and imported the full4322-byte release-pins report (SHA98542b3ffd827fe1f2f0e0be1185a1248803551dfb4498c8842abd131e27e746), exact4 product paths and report-only extra scope. Downloaded wheel hashes and registry digests match the final source. All9 actual image inputs exactly match final imported bytes.

Actual pip rejected a deliberately tampered build wheel under require-hashes. The final lock installed successfully in a fresh venv from the verified wheelhouse; CI-style no-isolation wheel/sdist build and fresh isolated wheel installation/both CLIs passed. No lock/evidence entered either archive. The final96 Python tests pass with zero skips in that locked environment. Product diff whitespace check passes.

Actual amd64 image build passed, digest sha256:7dde2c8c32fc635188521cc12efc3bb28565c52fe790e31f6cafbca654c7aca6. Both CLI helps pass network-none; image remains UID65532 and all4 installed module hashes match prior accepted source. Node/Claude/apt runtime layers were cache-identical; no new live model authentication is claimed or needed for changed builder inputs.

F3 description will explicitly say all4 evaluators are new relative to main, reviewed earlier on this branch; packaging-only unchanged claim is scoped. F4 source repair rejected: A-004 host inventory measured unchanged hashes/mtimes, not timestamp-restoration logic. PR wording names that proof. Apt/npm upstream resolution remains, so no bit-for-bit reproducibility claim.

Self-check: Actual lock negative/positive checks, package/image/source identities and full suite verified; independent/current council review and publication remain pending.
