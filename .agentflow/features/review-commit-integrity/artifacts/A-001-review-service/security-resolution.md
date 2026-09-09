* _2026-09-09 13:13:17 (GPT-6/default)_

# Security finding resolution

Reviewed report and exact candidate d6e96c32abfd766588c549e8922e3077d6abcb00. External worker changed only security-report.md and exited 0. No source-backed surviving SHA authority bypass was reported. One availability observation is factually consistent with lib.rs:980-1016 and both insert-or-compare-equal stores: a session admitted without target SHA cannot later be enriched by retry.

Disposition: retain as an intentional, approved fail-closed compatibility limit, not a new repair. The exact approved design says: "A repeat accepts only the same repo, PR, raw target, reason, and reviewer requirement; any difference is an error." It also says a missing/invalid stored target is retained as historical input but cannot authorize a result, and rejects fetching a new head because it silently changes review scope. User Design Go 8b35b4c explicitly approved this choice. Restoring None-to-Some enrichment would violate that frozen invariant; no evidence establishes that a later current head was the session's original review target. Valid identical-target replay remains exercised by the store and HTTP replay tests.

Operational consequence to report: if initial comment-triggered target lookup fails, that session will report missing_target and needs a new review round. Improving pre-open admission/recovery is a future proposal, not authorized source work in Stage 1. This risk is separate from legacy pending authority writes being withheld.

No code change, no second security cycle. Acceptance must independently evaluate conformance and compatibility against the actual design, including this limit.

Self-check: Source observation confirmed; design authority and operational limit explicit; no silent scope expansion.
