# ADR 032 — Review knowledge and calibration loops

Status: proposed · 2026-08-01

Builds on: [ADR 020](020-review-audit-effectiveness-ledger.md) (findings ledger),
[ADR 021](021-review-effectiveness-feedback-loop.md) (adoption as the primary signal),
[ADR 031](031-provider-neutral-kernel.md) (controller-owned review closing).

## Context

The council now owns the full review write path: every round persists
structured findings (stable IDs, severity, `raised_by`, `angle`) and closes
with a formal review and status. What it does not do is learn. Nothing reads
the findings ledger back; repo-specific conventions and confirmed false
positives live only in the heads of authors who keep re-explaining them;
and steering changes ship without a numeric check that they helped.

The displacement target (an incumbent review vendor) accumulated hundreds
of per-repo "learnings" with usage counters. Analysis of that corpus showed
where the value and the danger both live: ~60% of entries are false-positive
suppressions — precision calibration bought with thousands of review rounds —
but suppressions fossilize (a judgment call like "this fail-open is
intentional" becomes permanently invisible), version-sensitive facts rot,
and dead entries accrete (17% never fired).

Relevant prior art, in order of influence on this ADR:

1. **Google Tricorder/Critique**: per-check "actionable rate" (did the
   author act?), with checks **auto-disabled** below a threshold (~90%).
   Precision is governed by outcome data, not by exhorting the analyzer.
2. **Meta Infer's deployment lesson**: report only what the diff
   introduces; whole-repo findings get ignored. (The council's delta
   re-reviews already embody this; it is load-bearing, not incidental.)
3. **Static-analysis baseline decay**: every suppression mechanism without
   expiry review converges on blindness.

## Decision

Three loops at three cadences, plus one hygiene rule. Measurement is
automated; **every write into council behavior stays human-gated.**

### Loop 1 — per round: priors in, evidence still required

Reviewers and chair load a per-repository priors file (repo conventions and
confirmed false positives, ordered by how often each has fired) before
reviewing, when one exists for the target repo. Framing is normative:
**prior, not law** — a prior that contradicts what HEAD shows must be
re-verified, and re-raising with evidence beats silent deference.

Priors enter the corpus through exactly one door: **author interaction** —
a dispute the council accepted, or an explicit author dismissal with a
reason. Model self-reflection never writes to the corpus; letting the
council author its own suppressions is self-reinforcement with a delay.

### Loop 2 — weekly: calibration from outcomes

A scheduled job joins the findings ledger with PR outcomes and produces
three numbers per lane:

- **adoption rate** (per ADR 021) — precision proxy, computed per
  `angle` and per severity;
- **escape candidates** — post-merge fixes touching council-reviewed
  paths with no prior red/yellow finding (recall proxy, human-triaged);
- **actionable rate per angle** — the Tricorder number.

Governing rule: an angle whose actionable rate stays below threshold for
two consecutive periods is removed from the preset or has its task
rewritten — by a human, via the normal PR path. This is the structural
answer to "how does the council avoid blind hunting": angles that hunt
blindly lose their seat, on evidence.

### Loop 3 — per steering change: the eval gate

No steering change ships without running the recall A/B against the
documented-miss corpus. A change that improves precision but drops recall
against known misses is rejected. The gate exists so loop 2's precision
pressure cannot quietly optimize the council into agreeableness.

### Hygiene — decay

Every prior carries a last-fired timestamp. A prior that has not fired for
a quarter, or whose referenced paths changed substantially, moves to a
review queue instead of silently persisting. Version-sensitive facts
(language features, pinned dependency behavior) are tagged at import and
re-verified against the toolchain before each use in a verdict.

## Consequences

- The findings ledger becomes load-bearing read infrastructure, not an
  audit trail. Its schema (stable IDs, angle, raised_by) is the join key
  for every metric; changes to it now carry migration duty.
- A new author-facing dismissal command becomes worthwhile (structured
  "dismiss finding with reason"), because it is the cleanest ground truth
  for both loop 1's intake and loop 2's precision math. Scoped separately.
- The incumbent vendor's learnings corpus can be imported as loop 1's
  seed, filtered by usage and re-framed as priors — a one-time migration,
  not the ongoing mechanism.
- Curation cost is real and accepted: human-gated intake and quarterly
  decay review trade speed for immunity to corpus poisoning and
  fossilization — the two failure modes prior art demonstrates.

## Non-goals

- No online/automatic modification of steering, presets, or rosters by
  any model or metric. Loops measure and propose; humans dispose.
- No cross-repository generalization of priors: a convention is scoped to
  the repository that taught it.
- No reviewer-visible adoption pressure: metrics inform curation, they do
  not appear in review prompts (a reviewer told "your adoption rate is
  low" learns to under-report, which is worse than over-reporting).
