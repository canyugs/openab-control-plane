# Product hypothesis — outcome-priced PR review ("free on green, charge on red/yellow")

**Status: archived prior-art survey + risk backing for [ADR 006](../adr/006-commercial-model-outcome-framed-review.md) · 2026-06-28.** This note holds the vendor survey and the risk analysis; the commercial **decision** (proposed) lives in ADR 006. Not itself a decision record.

## The hypothesis

An AI PR-review service delivered as a GHA/webhook app, open to anyone: push a PR → a council of multiple agents/LLMs reviews in parallel → an aggregated verdict comes back. **Green/LGTM is free; only red (blocking) and yellow (actionable) findings are billed.** Illustrative: $20/mo bundling ~500 red + ~1000 yellow. **No BYOK** — the provider absorbs all model cost and hides which model is used ("用什麼 model 不重要").

The intuition: LGTM is the worthless part of review; the value is catching the issue in a PR you thought was perfect. Price should track that.

## Why it fits OCP

- The **engine mostly exists**: webhook/GHA trigger → multi-agent council → chair aggregates a severity-graded verdict (red/yellow/green is already produced). What's missing is the **commercial layer**: metering (count red/yellow per account), billing (Stripe), multi-tenant auth (per-user keys / OAuth) — which is exactly [roadmap](../roadmap.md) **Phase 4** (multi-tenant auth + audit log), plus a metering layer that doesn't exist yet.
- It makes **cost governance existential, not optional.** If you charge a flat/bundled price but pay per-PR compute, then [ADR 005](../adr/005-cost-governance-roster-swap.md)'s cheap-default + escalate-on-irreversibility *is* your margin control: cheap model on green-likely PRs, escalate only when it matters. The "no BYOK, we absorb model cost" choice is the *reason* the Tier-2 roster-swap work matters.

## Prior art — nobody in code review prices on outcome

Verified survey (2026 pricing). **Every** AI code-review vendor charges per-seat, per-run, or per-repo — **charged regardless of whether any bug is found.** None is per-finding, outcome-based, or free-on-green.

| Vendor | Pricing axis | Current price (2026) | OSS | Outcome-priced? |
|---|---|---|---|---|
| CodeRabbit | per-seat | $24–48 /seat/mo | — | No |
| Bito | per-seat | $12–25 /seat/mo | — | No |
| Sourcery | per-seat | $12–24 /seat/mo | **Free** | No |
| Korbit | per-seat (git author) | Pro ~$15 / Max ~$24 /seat/mo | **Max free for OSS** | No |
| Graphite | per-seat | Starter $20 / Team $40 /seat/mo; AI reviewer ("Diamond") retired → bundled into "Graphite Agent" | case-by-case | No |
| Entelligence | per-seat | ~$30–60 /seat/mo *(sales-gated; third-party figures, soft)* | free tier; no explicit OSS policy | No |
| Qodo (ex-CodiumAI) | usage / credits | Pro Team from $30/mo (≤30 users), $0.012/credit, **scales with PR size** | OSS program | No |
| Greptile | per-seat + per-run | $30 /seat **+ $1 /review** | — | No |
| Cursor BugBot | per-run | ~$1–1.5 /run | — | No |
| Ellipsis | per-run | **~$0.55 /review — clean or not** | — | No |
| GitHub Copilot code review | bundled + usage | included in Pro $10 … Enterprise $39 /seat/mo, **+ GitHub AI Credits (token-metered, since Jun 2026) + Actions minutes** | students / popular OSS maintainers get Pro free | No |
| Trag | **per-repo** | $300 /repo/mo (Team, ≤15 eng) | **Free forever for OSS** | No |

Sharpest data point: **Ellipsis explicitly costs ~$0.55 per review whether it finds bugs or approves the PR cleanly** — i.e. the industry standard is the opposite of free-on-green.

> Note: "Charlie"/"CharlieAI" is **Charlie Labs (by Pulley)**, a separate autonomous-SWE-agent company — *not* Entelligence's reviewer (which is "Ellie" / the Entelligence PR Review bot). Don't conflate them.

## Where outcome pricing *does* work — and the structural reason it hasn't reached code review

Outcome/resolution pricing is established, but only in **AI customer support**:
- **Intercom Fin** — **$0.99 per resolution**.
- **Sierra** — per resolved outcome; *escalation is not charged.*
- (Zendesk is often cited as outcome-based, but that framing rests on a single analyst source and **did not survive verification** — don't lean on it.)

**The structural difference (this is the real finding):** in support, the billable event is **customer-confirmable** — the customer agrees the issue was resolved. A code-review "red finding" is **vendor-adjudicated** — the vendor's own agent decides it's a red *and* bills for it. That self-dealing is why free-on-green/charge-on-red has **no precedent in code review**: buyers won't accept "pay because I said so."

## The unlock

Make the billable event **buyer-confirmable**: bill a finding only when the **user accepts** it (reject / false-positive → no charge). This:
1. converts vendor-adjudicated → customer-confirmable, importing the proven support-pricing model;
2. **neutralizes the adverse incentive** — manufacturing false findings now *loses* money instead of earning it.

Red/yellow then becomes the **value narrative**, not the raw **billing hook**.

## Risks to resolve before this is ADR-able

1. **Adverse incentive** — paying per finding rewards false positives / nitpick inflation, which destroys the very value prop. (Mitigation: accept-to-bill, above.)
2. **Unit-economics inversion** — COGS is per-PR (every review burns tokens); revenue is per-finding. A clean repo = full compute, $0 revenue. Clean big customers cost the most. (Mitigation: ADR 005 cost governance; possibly a per-PR or per-seat floor so red/yellow is the *value story* over a base, not the sole meter.)
3. **Self-adjudicated severity = trust** — if the vendor's agent sets severity and severity sets price, precision stops being nice-to-have and becomes the moat. The [roadmap](../roadmap.md) Evaluation/Benchmark work (CodeReviewBench etc.) moves from "future" to "core."

## Billing unit → decided in ADR 006

The billing-unit decision (subscription bundle, metered on **addressed** red/yellow,
green free with COGS capped by cost governance) has been promoted to its own decision
record: **[ADR 006 — Commercial model: outcome-framed PR review](../adr/006-commercial-model-outcome-framed-review.md)** (Status: proposed). The option comparison (A–D) and the
"addressed, not self-declared accept" reasoning live there. This note remains the
**prior-art survey + risk backing** for that ADR.

## Open questions / next

- ADR 006 is **proposed, not accepted** — ratify once the metering/billing path is
  committed and the "addressed" detection rule is specified.
- Define "addressed" precisely (line changed within N commits? thread resolved?
  merged-with-edit?) — the detection rule is where this gets hard. (ADR 006 key open problem.)
- Complete the survey periodically — 2026 pricing is volatile (Greptile/BugBot/Ellipsis/Copilot all changed recently); Entelligence numbers are sales-gated/soft; Trag figures are third-party-reported.

## Sources
- CodeRabbit https://www.coderabbit.ai/pricing · Greptile https://www.greptile.com/pricing + /docs/code-review-bot/billing-seats · Cursor BugBot https://cursor.com/blog/may-2026-bugbot-changes · Ellipsis https://www.ellipsis.dev/ · Bito https://bito.ai/pricing/ · Sourcery https://www.sourcery.ai/pricing
- Qodo https://www.qodo.ai/pricing/ · Graphite https://graphite.com/pricing (+ /blog/introducing-graphite-agent-and-pricing) · Korbit https://www.korbit.ai/pricing.html · GitHub Copilot https://docs.github.com/en/copilot/get-started/plans · Entelligence https://entelligence.ai/pricing (sales-gated) · Trag https://tragai.cc/pricing (third-party-confirmed)
- Outcome-pricing precedent: Intercom Fin https://fin.ai/help/en/articles/13975800-fin-pricing-outcomes · Sierra https://sierra.ai/blog/outcome-based-pricing-for-ai-agents
