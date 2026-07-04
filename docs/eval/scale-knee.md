# Council scale knee — how many reviewers is too many?

**Question (PLAN C10):** not "how big can the council get" but "where does the
marginal reviewer stop paying" — the roster size past which adding reviewers
costs time / quota / node without catching more.

**Setup (2026-07-05, oabcp-local):** chair + N reviewers, mixed providers
(Kiro/Claude split to dodge single-key quota), on a 6-CPU / 8-GB docker-desktop
node. Corpus: 3 finding-rich golden PRs from the A4 keycloak eval slice
(`canyugs/keycloak__openab__PR{37038,37634,36880}__20260703#1`, 700–870-line
diffs). One run per (size, PR) — LLM-nondeterministic, so read the trend, not a
single cell. Metrics: ttv = session `closed_at − created_at`; findings = R/Y/G
totals (a **count** proxy — not recall/quality, which is A4's judge); resources =
pod restarts / OOM (no metrics-server → `kubectl top` unavailable); quota = bot-log
`429/rate/quota` grep.

## Results

| Reviewers | PR37038 | PR37634 | PR36880 |
|-----------|---------|---------|---------|
| **4** | 332s · 7 · request_changes | 164s · 7 · request_changes | 371s · 10 · request_changes |
| **5** | 278s · 7 · request_changes | 176s · 7 · request_changes | 351s · 10 · request_changes |
| **6** | 405s · 8 · request_changes | **630s · no verdict** | **518s · 7 · approve** |

(cells: ttv · total findings · decision. quorum_n confirmed = reviewer count each run.)

## The knee is at ~4

- **4 → 5: flat.** Identical total findings (7/7/10 → 7/7/10), same decisions,
  ttv within noise. The 5th reviewer participated but surfaced no new distinct
  findings — pure cost (an extra pod, extra quota pressure; transient 429s on
  chair/rev3/rev4), no signal.
- **5 → 6: actively worse.** Three separate degradations:
  1. **Convergence fails** — PR37634 ran 630s and closed with `decision=None`
     (no verdict), > 2× the 4-reviewer baseline and past any usable bound.
  2. **Signal regresses** — PR36880 flipped `request_changes → approve`, i.e. the
     6-reviewer council *missed* the 🔴 that the 4- and 5-reviewer councils both
     caught. More voices diluted the finding rather than reinforcing it.
  3. **Resource pressure** — first pod restart (rev3) appeared at 7 bots on the
     6-CPU node (a liveness restart under load, not a clean OOM).

## Takeaway

For this workload, **4 reviewers is the sweet spot**; 5 is tolerable but wasteful;
**6 is past the knee** — slower, resource-stressed, and (worse) less accurate.
"Bigger council = better review" is false here: beyond ~4, added reviewers add
coordination cost and dilute consensus faster than they add coverage.

Scale the *standing* council to ~4 and reach for more only per-PR when a change is
unusually large/risky — and even then watch convergence.

## Honest limits of this read

- **Count, not recall.** Findings totals conflate signal and noise; a rigorous
  marginal-recall-vs-golden number is A4's judge (`~/Documents/zeabur/ocp-eval/`).
  Each run's verdict log is saved under the C10 scratchpad, so recall can be
  computed post-hoc on these exact outputs without re-running councils.
- **n = 1 per cell.** The 4→5 flatness and 5→6 breakdown are each seen across 3
  PRs, but run-to-run variance is real (PR37038's 🔴 count wandered 3→1→2). Treat
  the knee as directional, not a decimal.
- **Local node ceiling.** The 7-bot restart is a docker-desktop limit, not a
  product limit — production sizing would differ; the *signal* knee (~4) is the
  transferable finding.
