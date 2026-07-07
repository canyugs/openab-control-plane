# Precision ledger — council findings vs outcomes (ADR 015 §1)

One row per 🔴/🟡 finding in a council review of a dogfood PR. Ground truth is
what actually happened afterward, never how the review "felt" (ADR 015).

**Labeling rules (v1, 2026-07-03)** — change these only with a new version
header; old labels are not retro-edited:

- **acted-on** — a commit or an explicit author agreement addressed it.
- **known-deferred** — real, but already acknowledged (code comment, plan
  deferred list) before or at review time and consciously postponed.
- **open** — plausibly real, unaddressed, nobody has judged it yet.
- **noise** — factually wrong at review time, wrong premise, or a
  theoretical concern contradicted by live evidence.

Precision proxy = (acted-on + known-deferred) / (total − open). `open` rows
are excluded until judged. No recall claim is possible from this file: there
is no ground truth for what the council *missed*.

## Findings (PRs #59–#72, reviewed 2026-07-02 … 2026-07-03)

| PR | # | Sev | Raised by | Finding (abridged) | Outcome | Evidence |
|----|---|-----|-----------|--------------------|---------|----------|
| 61 | 1 | 🟡 | rev2 | `review_council` mode may not yet be consumed by OCP | noise | mode consumed by plane since #47 (2026-06-30), two days before this review |
| 61 | 2 | 🟡 | rev1 | silent fallback to `council` when REF unset for a PR trigger | noise | deliberate default in a dev-only script; generic council is the safe mode; never bitten |
| 63 | 1 | 🟡 | — | trailer invariant `r>=1 → request_changes` not formally stated in ADR | open | never picked up; cheap doc fix, still valid |
| 68 | 1 | 🟡 | rev1, rev2 | watchdog deadline anchored on `created_at`, no activity reset | known-deferred | limitation already documented at `src/main.rs:54` (ponytail comment, commit 0f3af50, predates review); on the A3 deferred list |
| 68 | 2 | 🟡 | rev1 | 60s grace + 30s sweep = up to 90s detection, tight vs 600s budget | noise | theoretical; A3 live kill-test closed with quorum in 2m53s |
| 68 | 3 | 🟡 | rev1 | CI `cancel-in-progress: true` could kill a bot mid-review | noise | wrong premise: bots are k8s pods; ci.yml concurrency cancels CI builds only |
| 70 | 1 | 🟡 | rev1 | same watchdog anchor issue applies to triage sessions | known-deferred | duplicate of 68-1 |
| 70 | 2 | 🟡 | rev1 | no per-panel/per-session timeout override, only global env | open | real gap adjacent to A3 deferred list; triage round 7 did hit backend latency |
| 71 | 1 | 🟡 | rev2 | template substitution order lets untrusted BODY expand later placeholders | acted-on | bae9129 (single-pass render) |
| 71 | 2 | 🟡 | rev2 | raw API response echoed to stderr on session-open failure | acted-on | bae9129 (truncated echo) |
| 73 | 1 | 🟡 | rev1 | "formula comment is informational, not a defect — math verified correct" | noise | self-described non-defect issued as 🟡; severity misuse |
| 74 | 1 | 🟡 | rev1, rev2 | `candidates` column (sum 76) reads as precision denominator (67=TP+FP) | acted-on | fixed in follow-up commit (dedup explanation added) |
| 74 | 2 | 🟡 | rev1 | 7/10 rows TP+FP ≠ candidates — needs footnote | acted-on | same commit; both reviewers caught it independently |

All-green reviews (no 🔴/🟡): #59, #62, #64, #65, #66, #67, #72, #75 — 8 of
15 council reports.

## Tally (v1 rules)

- 13 findings: **acted-on 4 · known-deferred 2 · noise 5 · open 2**
- Precision proxy: **6/11 ≈ 55%**
- 🔴 count across all 15 dogfood reviews: **0** — the council has never
  blocked a merge on its own repo; but on the keycloak bench it opened 🔴
  on 4/8 PRs, so the timid-chair worry is a same-repo/self-review effect,
  not a calibration failure (see martian-keycloak-slice.md).

Per angle:

| Reviewer | Raised | acted-on | known-deferred | noise | open |
|----------|--------|----------|----------------|-------|------|
| rev2 | 5 (2 shared) | 3 | 1 (shared) | 1 | 0 |
| rev1 | 9 (2 shared) | 2 | 2 (1 shared) | 4 | 1 |

## Observations (2026-07-03)

- Both acted-on findings came from rev2 on #71 (injection-surface and
  error-hygiene angles). All three factually-wrong/noise-by-premise findings
  are speculation about systems outside the diff (deployment, CI, plane
  internals) — consistent with SWE-PRBench's more-context-degrades result;
  the ablation arm in ADR 015 §3 should test this.
- The watchdog-anchor issue was flagged twice (68, 70) despite being
  documented in code. Reviewers restating in-code ponytail comments is a
  noise pattern worth a prompt tweak: "if the code already documents the
  limitation, cite it as known instead of raising it."
- Zero 🔴 on 13 self-authored PRs could mean clean code or a timid chair;
  the Martian bench (ADR 015 §2) has golden Critical/High issues and will
  distinguish these.
- #73's review of this very file produced a 🟡 that describes itself as "not
  a defect" — severity misuse to have something to report. Prompt tweak
  candidate: a finding that requires no action is 🟢 by definition.

## Re-review spend checkpoint (#127)

For each dogfood `@handle review <fix notes>` re-review, record round-1 spend
and round-N spend here until the eval harness has a structured cost table.
Trigger the reduced-panel follow-up if round-N spend exceeds 50% of round-1.

| PR | Round | Trigger | Round-1 spend | Round-N spend | Ratio | Notes |
|----|-------|---------|---------------|---------------|-------|-------|
| _pending dogfood_ | | `@handle review` | | | | |
