# Evaluation v0.1.0 release verification

PR418 merged as e33f77c78c4137abd7104f2e0b6acb667f0c4a5d; its tree equals reviewed source4b9ea56f0fd7466685ee57b196f6947c7de713a7. Required CI/council and independent cross-check passed. Release workflow34551718016 succeeded; annotated evaluation-v0.1.0 resolves to the merge commit.

Release: https://github.com/canyugs/openab-control-plane/releases/tag/evaluation-v0.1.0

Downloaded wheel and sdist SHA256 match GitHub asset digests. Fresh offline wheel installation outside checkout and both CLI help commands passed. Anonymous pull of ghcr.io/canyugs/ocp-review-eval:0.1.0 passed. Registry digest: sha256:b7eaa9fa9b963f26c929db73b216be9dd8ba31bb60c6537ef024f13c51eb2cc3. Actual linux/amd64 image runs as UID65532; both CLI help commands pass with network disabled and all four installed core module hashes match the reviewed source.

96 Python tests, hosted Rust/Postgres CI, locked package/image builds and tampered builder rejection passed before merge. No service deployment or new model evaluation was performed this round. Image supports linux/amd64; apt/npm remain upstream-resolved, so this is not a bit-for-bit reproducibility claim. Prior real model evaluation evidence remains A-004, including unavailable observed response model identity.

Cross-check review: .agentflow/features/review-commit-integrity/artifacts/A-005-release/release-crosscheck-report.md
Cross-check implementation: 4b9ea56f0fd7466685ee57b196f6947c7de713a7
Host gate: PASS
