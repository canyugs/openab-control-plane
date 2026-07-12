# ADR 021 — Review effectiveness feedback loop

Status: proposed · 2026-07-12

## Context

The council is a mature evaluator (parallel reviewers → quorum → chair synthesis).
What it lacks is the *optimizer* half of the loop: production outcomes never feed
back into how it reviews. Steering docs and presets are tuned by hand, from
one-off precision audits (see the 2026-07-11 SNR audit: signal-to-noise ~0.3–0.44,
a long 🟡 speculation/style tail).

ADR 020 specifies the data substrate — a findings ledger with resolution/re-review
history and an effectiveness metric list (resolution rate, reviewer contribution,
actionability proxy). It deliberately stops there: "the ledger supplies production
evidence; it is not itself ground truth."

The open decision is what closes the loop between that evidence and the council's
behavior. Framed against the agent literature, this is the *self-reflective* rung:
a system that adjusts its own strategy from results. The failure mode to avoid is
equally well documented — an over-automated tuner that "improves" itself into
outputs no human understands (the specification-driven "suicide pact").

## Decision

Close the loop, but keep the human as the gate. Concretely:

1. **The loop is human-gated, not auto-tuning.** Production data produces a
   *report*; a human reads it and edits steering/preset. OCP does not mutate its
   own steering docs, presets, or rosters from metrics. This matches the dev→prod
   deploy gate (canonical runbook: `openab-control-plane-ops/docs/deploy-gate.md`):
   behavior changes ship through the dev→prod release audit trail, never a silent
   runtime self-edit.

2. **Adoption is the primary signal, not finding counts.** A finding "helped" iff
   the author acted on it. Ground-truth proxies, cheapest first:
   - a fix commit / thread-resolve landed against the cited path after the finding
     (GitHub review-thread + subsequent commits);
   - an APPROVE that merged as-is and was *not* later reverted/hot-fixed;
   - `resolve F<n>` / `dismiss F<n>` ledger mutations (ADR 020) once they exist.
   Raw red/yellow/green counts are volume, not value, and are explicitly *not* the
   optimization target.

3. **Per-angle signal-to-noise is the unit of tuning.** Attribute adoption back to
   the review angle (correctness/security/integration) and reviewer. An angle whose
   findings are consistently ignored is a steering/preset candidate for trimming —
   this is the measurable version of the SNR scope/severity gate.

4. **Start offline, read what already exists.** The first artifact is a periodic
   *script outside OCP* (sessions + `trigger_ref`→GitHub), not a kernel feature and
   not a dashboard. It runs on today's `sessions.findings_*` + timing + `quorum_n`
   columns, and upgrades to the ADR 020 ledger fields when those land. No new mode,
   no new table, no new perm scope for step one.

## Guardrails against gaming the signal

Adoption rate is a metric, so it invites Goodhart. Three hard rules keep it honest:

- **Severity-weight; never trim a hard angle on adoption alone.** Maximizing raw
  adoption biases toward cheap-to-fix nitpicks and penalizes findings that are
  *correct but ignored* — classically `red`/security. A low-adoption security angle
  is a review-the-steering signal, not an auto-trim trigger; security/correctness
  angles keep their seat regardless of adoption number.
- **The loop cannot see recall.** Adoption only scores findings we *made*; it is
  blind to what we *missed* (false negatives). Precision-by-adoption is trivially
  gamed by finding less. Recall stays the eval harness's job (ADR 015); the report
  must never be read as "the council is good" on adoption alone.
- **Sample-size honesty.** No angle/preset conclusion off a handful of PRs. The
  report states n and confidence; a low-n cell is "unknown," not "bad." (Same
  discipline as the SNR audit's blind-judge design.)

## Non-goals

- **No utility-function auto-optimizer.** No RL, no automatic preset/roster search.
  The "utility function" is the report; the optimizer is a human.
- **No new precision/recall claims.** Benchmark precision/recall stays the eval
  harness's job (ADR 015). This loop measures *production adoption*, a different,
  noisier signal, and says so.
- **No self-editing steering.** Ruled out above; called out again because it is the
  tempting over-reach.

## Consequences

### Positive
- The manual SNR audit becomes a repeatable measurement instead of a one-off.
- review-mode's prod go/no-go (v0.1.29, currently dev-only) gets a data gate:
  "APPROVE-and-merged-clean rate on dev" instead of eyeballing one or two PRs.
- Preset A/B (`quorum_n` × duration × adoption) becomes answerable from existing
  columns — no build required to start.

### Negative
- Adoption is a lagging, noisy signal (an ignored finding may be right; a "fixed"
  file may have changed for unrelated reasons). The report must surface confidence,
  not single-number verdicts.
- GitHub-side attribution (which commit resolved which finding) is heuristic until
  the ADR 020 ledger makes finding→resolution a first-class edge.

### Neutral
- Nothing ships to the kernel. The loop is a plugin/ops concern; the first cut is
  an ops-repo script.

## Migration / build order

1. Offline adoption-rate script (ops repo): sessions + `gh api` → per-angle
   adoption table. Run it against dev review-mode PRs first.
2. Use its output to gate review-mode's prod deploy.
3. When ADR 020 ledger lands, repoint the script at
   `pr_review_findings` / `pr_review_finding_events` for exact finding→fix edges
   instead of the commit-proximity heuristic.
4. Only if the manual loop proves too slow to run: consider a read-only
   `GET /v1/review/effectiveness` rollup. Still human-gated; still no auto-tune.

## References

- [ADR 013 — Decision→review-state](013-decision-review-state.md)
- [ADR 015 — Eval harness](015-eval-harness.md) (benchmark precision/recall)
- [ADR 020 — Review audit and effectiveness ledger](020-review-audit-effectiveness-ledger.md) (the data substrate this loop consumes)
- `openab-control-plane-ops/docs/deploy-gate.md` (dev→prod gate — why the loop is human-gated)
