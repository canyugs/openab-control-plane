# Precision audit — council review quality (2026-07-11)

A signal-quality audit of the council's findings, in-domain (its own repo) and
out-of-domain (Keycloak golden PRs), judged with the CR-Bench taxonomy from
[prior-art.md](prior-art.md). Two independent judges (blind to authorship / to
each other). Companion to [ledger.md](ledger.md), [scale-knee.md](scale-knee.md).

## Method

- **In-domain (A):** an independent, skeptical judge re-verified every 🔴/🟡
  finding on 7 merged `openab-control-plane` PRs (#187/188/198/202/203/205/206)
  against the actual code, classifying each TRUE / NIT / FALSE. (Corrects a first
  self-judged pass that was optimistically biased — the author judging their own
  accepted findings rated everything "valid"; the blind judge did not.)
- **Out-of-domain (B):** re-analysed the A4 Keycloak eval. Raw per-reviewer council
  findings are no longer on disk (they lived in a now-down plane session store);
  recoverable **solo-mode** verdicts (same agent, prompt, diffs, golden set) give
  representative FP *shapes*, cross-checked against `martian-keycloak-slice.md` /
  `scale-knee.md` for council counts.

## Results

| | In-domain (A) | Out-of-domain (B, Keycloak) |
|---|---|---|
| Findings judged | 13 (🔴/🟡) | ~67 council (16 TP / 51 FP); solo subset ~28 |
| Precision (strict TP) | **31%** (4/13) | **~24%** (headline) |
| Not-wrong (TRUE+NIT) | **92%** (12/13) | — (many "FP" are valid-but-unrewarded) |
| Signal-to-noise | **0.44** | **0.3–0.4** |
| Outright FALSE/hallucinated | **1/13** | small; mostly over-flagging not fabrication |

**Both judges converge on the same story:** the council **rarely fabricates**
(in-domain 92% not-wrong; the one FALSE was a self-hedged "can't tell from the
diff"; it also correctly *downgraded* a reviewer's bogus RSA 🔴 blocker). Its
quality problem is **noise, not hallucination** — a precise-but-noisy reviewer,
~2–3 low-value findings per real one, and the noise is almost entirely in the 🟡
tail.

## What the noise is (ranked FP/NIT shapes)

1. **Speculation beyond the diff / scope-inflation** — "external scripts will
   break", "downstream extensions", "all deployments", CI/cluster behavior; none
   provable from the diff. (B's #1 shape; `ledger.md` already names it.)
2. **Style / housekeeping nits raised as 🟡 defects** — `static final`, error-wrap
   consistency, encapsulation, digest-pinning, entropy floors. CR-Bench "Coding",
   zero correctness impact. (Dominant in both A and B.)
3. **Forward-looking maintainability non-defects** — "clean up when the feature
   graduates", "track for GA removal" — not about the current diff.
4. **Hedged "could/may/might" + restating known limitations** — re-raising a
   limitation the code already documents.
5. **(most dangerous) confident-but-unverified claims** — B found false 🔴s ("does
   not exist… will not compile", reviewer's own note "not checked"). A false 🔴 can
   wrongly block a merge — the highest-cost failure mode.

By CR-Bench category the FP tail is overwhelmingly **Coding**, then
**Interface/Integration**; almost nothing in Data/Memory/Security/Structural.
Nearly all FP are **🟡 Low/Med** — per `scale-knee.md`, High/Critical recall is
already **86%**, so the noise and the real signal live in different severity bands.

## Highest-leverage lever

**Enforce a scope/severity gate in the reviewer steering prompt: suppress or
green-downgrade any finding that (a) requires no concrete action in *this* diff,
(b) speculates about systems/behaviour outside the diff, or (c) is not verifiable
from the diff.** This is a near-pure noise-for-nothing trade — it barely touches
recall because the findings that matter (High/Critical, 86% recall; concrete 🔴s)
are exactly what the gate keeps. It converts the council's recall lead into an F1
lead — the stated A4 goal. `ledger.md` already prescribes the rules ("a finding
that requires no action is 🟢 by definition"; "cite a documented limitation as
known instead of raising it"); they are simply **not enforced in
`docs/steering/pr-review.md`** today. That is the change to make (and it ships via
the [deploy gate](../bot-operations.md), verified dev-first).

**Orthogonal — do not conflate:** the deepest real misses (authz /
`hasPermission`, missed 9/9 at every reviewer count and angle per `scale-knee.md`)
are a *steering-depth* problem needing an authz checklist. FP reduction won't help
recall there; that's a separate lever.

## Caveats (why this is a signal, not a verdict)

- Small n, one judge per axis; B's classified text is **solo-mode**, so FP *shapes*
  transfer but council *counts* are inferred (the worst-precision council PR,
  kc#36880, has no raw dump). A second judge could shift the TP/FP split.
- In-domain sample skews small/clean/green (releases, mechanical changes); prod's
  real `zeabur-org` PRs sit between in-domain and Keycloak and are **not yet
  measured** — the prod council hasn't posted a real verdict yet.
- "FP" is pessimistic: a real-but-not-golden finding scores as noise, so usefulness
  is a floor.

## Next steps (candidates)

- **Draft the scope/severity gate** into `docs/steering/pr-review.md` + a few
  before/after examples; A/B it on a handful of PRs (in-domain + a large
  out-of-domain one) measuring SNR before shipping via the deploy gate.
- Measure prod SNR on the first real `zeabur-org` verdicts once they exist.
- (Recall track, separate) authz/permission steering checklist for the 9/9 blind spot.

## A/B result — scope/severity gate (2026-07-11)

Offline controlled A/B isolating the **gate** (same reviewer model, same diffs,
old 334-line steering vs new 358-line gate steering; blind reviewers + blind
per-PR judges). Three in-domain PRs (#202/#187/#203). Judged the **actionable
(🟡) findings** TRUE / NIT / FALSE against the real diff.

| | old steering | new (gate) |
|---|---|---|
| 🟡 findings | 9 | 1 |
| TRUE | 1 | 1 |
| NIT (noise) | 8 | 0 |
| FALSE | 0 | 0 |
| precision (TRUE/🟡) | **11%** | **100%** |

- **Noise tail 8 → 0.** The gate removed every NIT (style, "confirm intent",
  speculative fragility, pre-existing template-copy drift) without producing a
  false one.
- **TRUE recall 1/2 → 1/2 (unchanged).** Two distinct TRUE findings existed across
  the set (a token-collision bug in #187, a stale doc-comment in #203). Old caught
  the first, new caught the second — recall held, the specific hit shifted.
- **#203 is the clean demonstration:** the gate dropped 4 NITs and **kept** the 1
  TRUE 🟡 — discrimination, not blanket 🟡-suppression.
- **Caveat:** the #187 TRUE miss under the gate is real; with one reviewer sample
  per cell it looks like reviewer variance (~1 real find/run), not systematic
  signal loss, but n is tiny (2 TRUE total). Absolute SNR here (Opus reviewer) is
  not comparable to the council's 0.44 baseline — only the old→new *delta* is. The
  delta is decisive on noise, neutral on recall. Recall improvement was never this
  lever's job (see the orthogonal authz-depth track).
