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
2. **Declared boundaries** (this doc): a repo that wants an explicit fence
   writes one, in a place reviewers are instructed to read. Findings that
   target a declared non-goal are demoted to at most one 🟢 note. Two forms:
   *repo-level* (below — durable, applies to every PR) and *per-PR* (a Review
   Contract in the PR body — see next section).
3. **Adoption loop** (ADR 021): findings the author repeatedly rejects show up
   in the ledger's adoption data — that is how you *discover* boundaries you
   forgot to declare. Grow this file from that signal, not from speculation.

## Per-PR: the Review Contract

Repo-level boundaries fence the whole project; a Review Contract fences one
change. Strong models turn an open-ended review into an unbounded one — the
contract is the author's up-front definition of what "in scope" means for this
PR, so the reviewer's effort is channeled into verification, not expansion.
No prior art has this: CodeRabbit infers scope from a linked issue and lets
maintainers set repo policy, but there is no author-declared per-PR contract.

Put these sections in the PR body:

- **Goal** — the user problem this PR must solve.
- **Non-goals** — what it intentionally does not attempt.
- **Accepted Residual Risks** — known failure modes / trade-offs accepted for
  this version, each with its mitigation and recovery.
- **Acceptance Criteria** — concrete, testable conditions required for LGTM.
- **Follow-ups** — hardening or broader designs explicitly deferred.

The reviewer treats Acceptance Criteria as the primary checklist (verified
against behavior), demotes findings that hit a Non-goal / Accepted Risk /
Follow-up — but the floor below still holds, and the reviewer verifies the
contract is honest: a real defect laundered as an "accepted risk", or a
criterion claimed met but not, is raised anyway.

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

## Enforcing the council as a merge gate

The council posts an `openab/council` commit status on every review (success on
LGTM, failure on CHANGES REQUESTED). To make it a real merge gate, add
`openab/council` as a **required status check** in the branch's protection —
not a required *review*. A bot's approving review shows "read-only permissions"
and does not count toward "Require approvals" unless the App holds repo push
access, which an untrusted-input review agent must not have (ADR 019). The
status check is the enforceable gate; the APPROVE review stays a friendly
signal. `enforce`-mode REQUEST_CHANGES is read-only for the same reason, so the
status check — not the review verdict — is what actually blocks merge.
