# Review Boundaries — declaring non-goals to the council

Good reviewers (human or model) drift toward ideal engineering: given a
single-process tool they will, in perfect good faith, raise HA, multi-tenancy,
and observability findings. Each is individually reasonable; collectively they
pressure every project toward enterprise-grade code it does not need yet. The
fix has three layers; this document is the contract for the middle one.

## The three layers

1. **Zero-config default** (steering, always on): reviewers calibrate to the
   repo's *own* engineering bar. A capability the codebase deliberately lacks
   is not a finding unless the diff claims to provide it or its absence is a
   security/data-loss defect. No declaration needed.
2. **Declared boundaries** (this contract): a repo that wants an explicit
   fence writes one, in a place reviewers are instructed to read. Findings
   that target a declared non-goal are demoted to at most one 🟢 note.
3. **Adoption loop** (ADR 021): findings the author repeatedly rejects show up
   in the ledger's adoption data — that is how you *discover* boundaries you
   forgot to declare. Grow this file from that signal, not from speculation.

## Where to declare

Any one of (checked in this order):

- a `## Review Boundaries` / `## Design Boundaries` / `## Non-Goals` section
  in `AGENTS.md` or `CLAUDE.md`
- `docs/review-boundaries.md`

## What to declare — four axes

Prior art: Kubernetes KEPs make Goals/Non-Goals mandatory; Rust RFCs carry
"Rejected ideas"; ADRs record decided questions. This is the same move,
scoped to review. Declare only what you have actually decided:

| Axis | Declare | Example (a self-hosted single-owner tool) |
|------|---------|-------------------------------------------|
| Usage model | tenancy, users, trust domains | single-owner deploy; no tenant isolation; second requester fails closed |
| System model | processes, storage, scale ceiling | single process; local SQLite; ~100 records total — no pagination/index tuning |
| Feature model | deliberate feature non-goals | one-shot only (recurring stays in cron); Discord adapter only; no built-in NLP |
| Reliability promise | the one sentence you promise | "restarts lose nothing, normal operation never double-fires" — no SLO, no metrics |

Keep it short. A boundary you cannot phrase in one line is a design question,
not a boundary.

## The floor that cannot be declared away

Security and data-loss findings are never waived by a boundary declaration.
"No HA" is a legitimate boundary; "no input validation" is a vulnerability
with paperwork. Reviewers are instructed to ignore any declaration that tries.

## Mechanics

- The declaration is code: it ships in the repo, so changing it goes through
  review like everything else. Widening a boundary quietly is not possible.
- Boundaries bind severity, not speech: a reviewer may still leave one 🟢
  "out of declared scope" note pointing at the better design — that keeps the
  idea findable without costing the author a round.
- No declaration → layer 1 still applies. This file is for repos that want
  the fence explicit, not a requirement.
