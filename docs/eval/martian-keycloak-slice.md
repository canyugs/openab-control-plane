# Martian bench — keycloak slice, first run (ADR 015 §2)

Council reviews of the 10 keycloak golden PRs, scored by the Martian
open-source pipeline (precision/recall vs 24 human golden comments).

- **Date:** 2026-07-03
- **Judge:** `claude-sonnet-4-5` via Zeabur AI Hub (hnd1), temperature 0
- **Mode:** council (chair + rev1 + rev2), `SELF_FETCH=1`, opened via
  `open-council.sh` against private forks in `canyugs/`
- **Pipeline:** withmartian/code-review-benchmark `offline/`, dedup on,
  local patches (see ADR 015 §2)

## Headline (openab council)

| Metric | Value |
|--------|-------|
| Precision | **23.9%** (16 TP / 67 candidates) |
| Recall | **66.7%** (16 / 24 golden) |
| F1 | **35.2%** |

Recall well above SWE-PRBench's 15–31% frontier expectation; precision is
the noise problem quantified — ~3 of every 4 council findings don't map to a
human golden comment. (FP is pessimistic: a real-but-not-golden finding
counts against precision. Same rule for every tool, so comparable.)

## Per-PR

| repo/PR | TP | FP | FN | candidates | golden |
|---------|----|----|----|------------|--------|
| greptile#1 | 2 | 4 | 0 | 9 | 2 |
| kc#32918 | 1 | 2 | 1 | 4 | 2 |
| kc#33832 | 2 | 5 | 0 | 7 | 2 |
| kc#36880 | 1 | 10 | 2 | 12 | 3 |
| kc#36882 | 0 | 5 | 1 | 5 | 1 |
| kc#37038 | 2 | 3 | 0 | 6 | 2 |
| kc#37429 | 2 | 11 | 2 | 14 | 4 |
| kc#37634 | 3 | 3 | 1 | 7 | 4 |
| kc#38446 | 1 | 5 | 1 | 7 | 2 |
| kc#40940 | 2 | 3 | 0 | 5 | 2 |
| **total** | **16** | **51** | **8** | | **24** |

## Council verdicts (r/y/g)

| PR | verdict | 🔴 | 🟡 | 🟢 |
|----|---------|----|----|----|
| 32918 | approve | 0 | 2 | 5 |
| 33832 | approve | 0 | 3 | 3 |
| 36882 | approve | 0 | 2 | 3 |
| 36880 | request_changes | 1 | 2 | 4 |
| 37038 | request_changes | 3 | 1 | 3 |
| 37429 | request_changes | 1 | 3 | 5 |
| 37634 | request_changes | 2 | 1 | 2 |
| 38446 | approve | 0 | 3 | 3 |
| 40940 | approve | 0 | 2 | 2 |
| greptile#1 | request_changes | 2 | 1 | 2 |

## Baseline comparison — same judge, same 10 keycloak PRs

All tools scored with the same `claude-sonnet-4-5` judge on the identical
10 golden PRs (baseline reviews are Martian's, from the shipped
`benchmark_data.json`). Sorted by F1.

| Tool | Precision | Recall | F1 |
|------|-----------|--------|-----|
| qodo-v2 | 48.1% | 54.2% | **51.0%** |
| coderabbit | 33.3% | 62.5% | 43.5% |
| copilot | 28.9% | 54.2% | 37.7% |
| cubic-dev | 26.4% | 58.3% | 36.4% |
| greptile-v4 | 27.9% | 50.0% | 35.8% |
| **openab council** | **23.9%** | **66.7%** | **35.2%** |
| claude-code | 23.3% | 29.2% | 25.9% |

Reading:

- **Council has the highest recall of every tool measured** (66.7% vs
  62.5% for the next best, coderabbit). More angles catch more real issues —
  the architecture's strength shows up exactly where expected.
- **Council precision is near the bottom** (only bare claude-code lower).
  This is the noise cost of more angles, and it is the A4 tuning target.
  F1 lands mid-pack, tied with greptile-v4 / cubic-dev.
- **Council beats single-agent claude-code on all three metrics** (R
  66.7% vs 29.2%, F1 35.2% vs 25.9%) — a directional council-vs-solo
  signal, though claude-code here is Martian's harness, not our Solo mode.
  The controlled A/B is ADR 015 §3.
- **Qodo wins F1 by precision**, not recall — the noise ceiling is where
  the F1 gap to the leaders comes from, not missed issues.

Takeaway for A4: the council's job is to convert its recall lead into an F1
lead by cutting false positives. The ledger's noise patterns (speculation
beyond the diff, restating in-code comments, 🟡 for non-defects) are the
first levers.

## Caveats

- 10 PRs / 24 golden — a signal for tuning, not an external claim
  (DeepSource: 100+ before publishing).
- One judge model; a second judge (per DeepSource "judge rules dominate")
  would test robustness. AI Hub gateway vs direct API assumed equivalent.
- Baseline reviews came from Martian's runs (possibly at different dates /
  tool versions); only the judge and the golden set are held constant.
- Council reviewed via `SELF_FETCH` (diff only, no `gh pr checkout`). The
  context-ablation arm (ADR 015 §3) is untested here.
