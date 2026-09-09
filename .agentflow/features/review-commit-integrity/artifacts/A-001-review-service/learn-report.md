* _2026-09-09 13:18:51 (gpt-5.6-luna/max)_

# Learn record

Delivered evidence is the final host verification plus acceptance: build, package tests (167 unit + 3 boundary), workspace tests, focused eight-test SHA suite, package formatting, clippy, and PostgreSQL tests passed; acceptance outcome is PASS. The implementation, fixture, and security reports are worker-produced claims/interpretations, useful only where corroborated by those delivered records.

Concrete lessons from this run:

- Sandbox `PermissionDenied` is an environment constraint, not a semantic test failure. Network-dependent fake-GitHub and PostgreSQL checks need authorized host-runtime verification; the initial blocked run must not be scored as behavioral failure.
- Realistic verified-round fixtures can change anchor expectations. The corrected fixture expects comment `1001`, excludes decoy `900`, and preserves duplicate/replay and identity assertions; fixture realism changes the anchor without weakening replay guarantees.
- The original immutable-target design intentionally forbids later SHA enrichment. Security and acceptance evidence classify a missing-target session’s later `None → Some` attempt as an accepted fail-closed availability limit: it cannot silently substitute a newer review head.
- Formatting evidence must stay separated by scope: package formatting passed, while workspace-wide differences remained in unchanged out-of-scope baseline files. That baseline result is not an implementation regression.

Self-check: Frozen scope, output, model, effort and authority declared.
