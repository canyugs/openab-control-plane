# ADR 006 — Commercial model: outcome-framed PR review (bundle, metered on *addressed* findings)

Status: **proposed** · 2026-06-28

## Context

OCP's engine (webhook/GHA trigger → multi-agent council → chair's severity-graded
verdict) can be productized as a paid PR-review SaaS. The wedge — from a product
discussion, and aligned with the governance-layer thesis (the model is commoditized;
value lives in the layer around it) — is **"free on green, charge on red/yellow":**
LGTM is the worthless part of review; the value is catching the issue in a PR the
author thought was perfect.

A verified 2026 prior-art survey (see
[outcome-priced-review-hypothesis.md](../archive/outcome-priced-review-hypothesis.md) for the
full table + sources) establishes two facts:

1. **No AI code-review vendor prices on outcome** — all are per-seat, per-run, or
   per-repo, billed regardless of whether a bug is found (Ellipsis literally charges the
   same for a clean PR as for one with findings). Free-on-green has **no precedent in
   the category.**
2. **Outcome pricing works only in AI support** (Intercom Fin $0.99/resolution; Sierra,
   escalation not charged) — because there the billable event is **customer-confirmable**
   (the customer agrees it was resolved). A code-review finding is **vendor-adjudicated**
   (the vendor's agent both decides it's a "red" and bills for it). That self-dealing is
   the structural reason free-on-green hasn't reached code review.

Three risks bound any finding-based pricing: (a) paying per finding rewards false
positives / nitpick inflation; (b) unit-economics inversion — COGS is per-PR (every
review burns tokens), so a clean repo is full compute at zero revenue; (c) self-
adjudicated severity is a trust problem when severity sets price.

## Decision (proposed)

1. **Productize as a subscription bundle** — e.g. ~$20/mo bundling ~500 red + ~1000
   yellow. Red/yellow is both the **metering unit and the value narrative**; green is
   free. A bundle (not pure usage) gives the buyer a predictable bill.

2. **Bill on *addressed* findings, confirmed behaviorally — not on raw findings, not on
   user-declared "accept".** A finding bills only when the author acts on it (changes the
   flagged line / amends the PR / resolves the thread). This is the load-bearing choice:
   - Raw-finding billing → vendor inflates (risk a).
   - Explicit accept-to-bill → the **buyer** gains an incentive to mark real bugs
     "rejected" to dodge the fee (gaming flips to the buyer).
   - **"Addressed" is an objective fact in the diff**, not a self-reported flag either
     side can fake → blocks vendor inflation *and* buyer dodging, and makes the billable
     event **customer-confirmable** (mirrors Sierra's *observed*, not self-reported,
     resolution — importing the proven support-pricing model into code review).

3. **Inversion is bounded by cost governance, not a price floor.** Clean PRs still cost
   compute; rather than add a per-seat floor, lean on [ADR 005](005-cost-governance-roster-swap.md):
   the cheap-default model drives green-PR COGS toward zero, escalating only on signal.
   **Free-on-green is economically viable *because* cost governance caps the COGS of
   green.**

4. **No BYOK — the provider absorbs model cost and hides the model.** "Which model"
   becomes an implementation detail, consistent with the thesis that the model is
   commoditized and the value is the governance/aggregation layer.

## Consequences

- **ADR 005 becomes existential, not optional.** A flat/bundled price against per-PR
  compute means the cheap-default + escalate-on-irreversibility work *is* the margin
  control. This ADR couples commercial viability to the roster-swap path in
  [ADR 005](005-cost-governance-roster-swap.md) / #18.
- **Requires the missing commercial layer:** metering (count *addressed* red/yellow per
  account), billing (Stripe), multi-tenant auth (per-user keys / OAuth) — = ROADMAP
  Phase 4 (multi-tenant auth + audit log) plus a metering layer that does not exist yet.
- **Precision becomes the moat.** If billing depends on findings being real and
  acted-on, the ROADMAP Evaluation/Benchmark work (CodeReviewBench) moves from "future"
  to core.
- **Hardest open problem — defining/detecting "addressed":** line changed within N
  commits? thread resolved? merged-with-edit? Commit-diff attribution is where this gets
  hard, and it gates implementation. Tracked as the key open question in the hypothesis
  note.
- **Status → accepted** when the metering/billing path is committed *and* the "addressed"
  detection rule is specified. Until then this records the chosen commercial direction.
- This is the first **commercial** ADR (001–005 are technical). Kept in `docs/adr/` for
  now; split into a separate track only if commercial ADRs proliferate.
