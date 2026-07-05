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

## Where the returns stop: usefulness plateaus after 4, turns negative by 6

Precisely: the *last size that pays* is 4. The 5th reviewer adds nothing (the
plateau); the 6th makes things worse (the degradation cliff sits between 5 and 6).
So "aim for 4" — not because 4 is a sharp inflection, but because nothing past it
helps and 6 hurts.

- **4 → 5: flat.** Identical total findings (7/7/10 → 7/7/10), same decisions,
  ttv within noise. The 5th reviewer participated but surfaced no new distinct
  findings — pure cost (an extra pod, extra quota pressure; transient 429s on
  chair/rev3/rev4), no signal.
- **5 → 6: three degradations, each on a different PR** (not a uniform
  cross-PR worsening — each PR failed in its own way):
  1. **Convergence fails** — PR37634 ran 630s and closed with `decision=None`
     (no verdict), > 2× the 4-reviewer baseline and past any usable bound.
  2. **Signal regresses** — PR36880 flipped `request_changes → approve` AND its
     total findings dropped 10 → 7: the 6-reviewer council *missed* the 🔴 the 4-
     and 5-reviewer councils both caught. More voices diluted consensus rather
     than reinforcing it.
  3. **Resource pressure** — first pod restart (rev3) appeared at 7 bots on the
     6-CPU node (a liveness restart under load, not a clean OOM).
  (PR37038 stayed request_changes at all three sizes — so the cliff is
  PR-dependent, which is itself the point: at 6 you can no longer count on a
  verdict, or on the right one.)

## Recall check — the count-knee holds as a *measured* recall knee

The table above counts findings (a signal+noise proxy). To test whether the
plateau is real, each run's 9 verdicts were re-judged post-hoc against the
keycloak golden labels — **no council re-run**. Candidate = *reviewer-union*
(each reviewer's session messages; a golden issue is a hit if ANY reviewer
covered it), scored by #74's exact judge (`claude-sonnet-4-5` via AI Hub). This
measures the council's raw **detection** capacity by size. (The chair's
synthesized verdict isn't in the session — it was written to a pod file and
`--edit-last` overwrote all but the last size on GitHub — so reviewer-union is
the recoverable, internally-consistent candidate across all 9 runs.)

| Reviewers | PR37038 | PR37634 | PR36880 | Aggregate |
|-----------|---------|---------|---------|-----------|
| **4** | 100% (2/2) | 50% (2/4) | 67% (2/3) | **67% (6/9)** |
| **5** | 100% (2/2) | 50% (2/4) | 67% (2/3) | **67% (6/9)** |
| **6** | 100% (2/2) | 50% (2/4) | 67% (2/3) | **67% (6/9)** |

**Recall is dead flat across 4/5/6 — the 5th and 6th reviewer detect zero new
golden bugs.** Not just the aggregate: the *per-PR* recall and the *exact set of
misses* are identical at every size. The count-proxy plateau was real.

Two sharper reads the count table couldn't give:

- **On issues that matter, recall is 86% and still flat.** The 9 golden are 1
  Critical + 6 High + 2 Low. The stable miss set is 2 Low + 1 High, so
  High/Critical recall = **6/7 (86%)** at every size; the 2 Low misses (a Javadoc
  nit, an over-broad `catch`) are arguably correct triage, not a gap. Aggregate
  67% also ≈ #74's 66.7% chair-verdict recall on the 10-PR slice — an independent
  method landing in the same place.
- **The "6 regresses" degradation is a chair-synthesis failure, not a detection
  failure.** On PR36880 the 6-reviewer council flipped `request_changes →
  approve` (Results table) — yet its *reviewers* caught the same 2/3 golden as at
  4 and 5, **including** the orphaned-permissions blocker. The bug was seen and
  then dropped at the chair/consensus step. So "more voices dilute consensus"
  (C10) is now located precisely: bigger councils hurt at **synthesis**, not
  **review**.

**Actionable:** the recall ceiling here is set by review *angles*, not reviewer
*count* — the one serious miss (PR36880 `hasPermission` resource-lookup) is a
stable blind spot no reviewer caught at any size. The A4 lever to raise recall is
angle/prompt coverage; the lever to stop the 6-reviewer regression is chair
synthesis, not fewer reviewers. Harness: `ocp-eval/.../offline/c10_recall.py`
(reads the durable plane + `c10-results.csv`); raw output `c10-recall.json`.

## Takeaway

For this workload, **4 reviewers is the sweet spot**; 5 is tolerable but wasteful;
**6 is past the knee** — slower, resource-stressed, and (worse) less accurate.
"Bigger council = better review" is false here: beyond ~4, added reviewers add
coordination cost and dilute consensus faster than they add coverage.

Scale the *standing* council to ~4 and reach for more only per-PR when a change is
unusually large/risky — and even then watch convergence.

## Honest limits of this read

- **Count vs recall — now resolved.** The Results table counts findings (signal
  +noise); the "Recall check" section above re-judged the same 9 runs against
  golden and found recall flat across sizes (67% aggregate, 86% on High/Critical).
  So the plateau isn't a counting artifact. Caveat inherited: reviewer-union
  recall measures *detection*, not the chair's delivered verdict (see that
  section for why the chair text isn't recoverable per-size).
- **n = 1 per cell — no repeated-run variance data.** Each (size, PR) ran once,
  so this measures no run-to-run noise directly. PR37038's 🔴 count moving 3→1→2
  is *across sizes* (4→5→6), which confounds a real size effect with LLM noise —
  it can't be separated here. So the plateau/cliff is a directional read across 3
  PRs, not a measured decimal; a rerun with ≥2 samples per cell would firm it up.
- **Local node ceiling.** The 7-bot restart is a docker-desktop limit, not a
  product limit — production sizing would differ; the *signal* knee (~4) is the
  transferable finding.
