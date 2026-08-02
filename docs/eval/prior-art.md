# Prior art — evaluating AI code-review quality

A landscape scan (2026-07) of how the field measures AI/LLM code-review quality,
and what it means for OCP's council. Not exhaustive — a baseline to build on.
Companion to [ledger.md](ledger.md) (our A4 eval) and
[martian-keycloak-slice.md](martian-keycloak-slice.md).

## Benchmarks / datasets

| Benchmark | What it is | Distinguishing trait |
|-----------|-----------|----------------------|
| **CodeReviewer** (Li et al. 2022) | Foundational public multi-language dataset (~150K), comment-generation focus | Diff-hunk level only, **no repo context**; ~25–32% low-quality entries |
| **CR-Bench** (2026) | SWE-bench issues recast as review tasks; real bugs traced by `git blame`, "detectable" filtered by an LLM | 584 total / 174 verified from mature repos (Django, SymPy, scikit-learn); real-world utility framing |
| **AACR-Bench** | Automatic code review with **holistic repository-level context** | Directly addresses CodeReviewer's diff-only limitation |
| **SWE-PRBench / SWR-Bench** | Grade agent findings against real PR-review feedback | **Hit-based** Precision / Recall / F1 over change-points |
| **CodeFuse-CR-Bench / CodeReviewQA** | Multi-task / QA-oriented review evaluation | End-to-end generative peer review |

## Methodologies & metrics

- **Classification metrics** — Precision / Recall / F1, hit-matched against golden
  findings. This is what OCP's A4 already does.
- **Text-generation metrics** — BLEU / CodeBLEU / ROUGE (comment-gen; low relevance
  to OCP, which produces structured findings not paraphrased comments).
- **Ranking** — NDCG@k, MRR (retrieval-based approaches).
- **New and most relevant — CR-Bench's `usefulness rate` and `signal-to-noise
  ratio (SNR)`**: actionable feedback / noise. This is the correct *headline*
  metric for "review quality," not F1 — it captures the thing users actually feel.
- **Ground-truth construction** — `git blame` buggy code back to its introducing
  PR + LLM "is this detectable at review time?" classification. (OCP's Martian
  harness does the analogous thing: fork golden PRs, review, judge vs golden.)

## Finding taxonomy (CR-Bench — reusable as-is)

Three orthogonal axes; adopt for classifying our own findings in a precision audit:

- **Category (root cause):** Structural · Interface/Integration/System ·
  Requirements/Features/Functionality · Data · Concurrency · Memory · Security ·
  Coding.
- **Severity:** Low · Medium · High.
- **Impact (ISO/IEC 25010):** Functional Suitability · Performance Efficiency ·
  Compatibility · Usability · Reliability · Security · Maintainability · Portability.

CR-Bench's verified set skews Structural (79.9%) and High-severity (93.1%).

## What maps directly to OCP

1. **Low precision is universal, not an OCP failing.** On CR-Bench, SOTA
   **GPT-5.2 single-shot scores 27% recall / 3.6% precision** (SNR 5.11). OCP's
   council (keycloak slice) measured **66.7% recall / 23.9% precision** — different
   benchmark, not directly comparable, but our precision is *not* out of line.
2. **Recall↑ costs SNR↓ — confirmed at SOTA.** CR-Bench Reflexion (iterative)
   raises recall 27→32.8% but halves SNR (5.11→1.95); smaller models hallucinate
   under iterative pressure (SNR 0.91). This is exactly our A4 result: fan-out
   buys +24pt recall for −3.5pt precision. The precision/recall tension is
   fundamental, not a tuning miss.
3. **Humans prize signal integrity over exhaustive detection.** Validates the A4
   direction: **cut false positives, don't chase recall.**
4. **The headline metric should be SNR / usefulness-rate, not F1.** F1 rewards
   recall we already have; SNR targets the noise that erodes trust in a tool that
   is now the official reviewer.

## Open problems the field hasn't solved

- **False positives / noise "remain unaddressed systematically."** (So an SNR win
  here is genuinely novel, not catch-up.)
- Lack of realistic, workflow-faithful benchmarks.
- Dataset quality (the ~25–32% low-quality-entry problem).

## Takeaway for OCP

A4 is already on the field's main line (fork-golden-PR + P/R/F1 + LLM-judge, same
family as CR-Bench / Martian). The cheap, high-value upgrade: **promote SNR /
usefulness-rate to the headline metric** and **classify findings with CR-Bench's
severity×category axes**, then attack the false-positive tail. First concrete step:
a precision audit of recent *real* council verdicts using that taxonomy.

## Sources

- A Survey of Code Review Benchmarks and Evaluation Practices (Pre-LLM & LLM Era) — arxiv 2602.13377
- CR-Bench: Evaluating the Real-World Utility of AI Code Review Agents — arxiv 2603.11078
- SWE-PRBench: Benchmarking AI Code Review Quality Against PR Feedback — arxiv 2603.26130
- AACR-Bench: Automatic Code Review with Holistic Repository-Level Context — arxiv 2601.19494
- Benchmarking and Studying the LLM-based Code Review — arxiv 2509.01494
- CodeReviewer (Li et al., 2022)
