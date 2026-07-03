# ADR 015 — Eval harness: measuring council review quality (A4)

Status: proposed · 2026-07-03

## Context

Review quality is the product's actual moat; competitors live and die on
noise level (roadmap "Evaluation / Benchmark"). Sources reviewed 2026-07-03
set the design constraints:

- **METR RCT (arXiv 2507.09089)** — developers forecast a 24% speedup,
  post-hoc felt 20%, measured a 19% *slowdown*. Perception is anti-signal:
  no "the council felt useful" self-reports, only ground-truth labels. And
  measure the cost side (time-to-verdict, author time burned on noise), not
  just catch-rate.
- **Hashimoto on Spotify's metrics** — velocity/activity counts prove
  nothing. Every eval number must link a finding to an outcome.
- **SWE-PRBench (arXiv 2603.26130)** — frontier models catch only 15–31% of
  human-annotated PR issues; expect humbling recall. Their surprise: *more*
  context degraded results monotonically (diff-only beat full checkout).
- **DeepSource critique** — every vendor wins its own benchmark; Greptile
  scored 82% → 45% under different scoring rules. Judge rules dominate
  outcomes, so they must be pinned and versioned. Synthetic ground truth is
  called out as untrustworthy.
- **Qodo methodology** — the hit definition worth stealing: a true positive
  must *describe* the issue and *locate* it.
- **Martian Code Review Bench**
  (github.com/withmartian/code-review-benchmark, MIT) — 50 PRs across 5
  repos (sentry, grafana, keycloak, discourse, cal.com) with 136
  human-verified golden comments; LLM-judge pipeline fully open and locally
  runnable; the only third-party leaderboard (CodeRabbit, Greptile, Qodo…).

The decisive discovery from reading Martian's pipeline source: **adding a
tool means forking the golden PRs into a GitHub org where the tool is
installed and letting it review them on GitHub** (step0 recreates each PR in
a repo named `{owner}__{repo}__{tool}__PR{n}__{date}`; step1 downloads the
review comments back via `gh`). That is exactly OCP's native path — webhook
`pull_request opened` → council → chair posts the report comment. The
earlier working assumption (an offline adapter reusing the B1 panel pattern:
inline diff in a template over generic `/v1/sessions`) is unnecessary. The
eval exercises the real product end-to-end, which is also the more honest
measurement.

## Decision

Three instruments, in cost order. All numbers are for tuning angles and
presets, not for external claims (50 PRs is a signal, not a benchmark; 100+
samples before saying anything publicly, per DeepSource).

### 1. Precision ledger over our own dogfood PRs (~free, do first)

`docs/eval/ledger.md`: one row per 🔴/🟡 finding across council-reviewed
dogfood PRs (#59–#71 and onward), columns: PR, finding, severity, raised-by
angle, outcome (**acted-on** = a commit or explicit author agreement
addressed it; **noise** = dismissed or ignored, with the reason). Labeling
rules live in the file header and are versioned with it.

Output: a precision proxy per severity and per reviewer angle — the direct
input for tuning presets. No recall claim is possible here (no ground truth
for what the council missed); the ledger never pretends otherwise.

### 2. Martian offline bench through the real product

- Dedicated bench org (e.g. `oabcp-bench`) with the dogfood GitHub App
  installed and the plane's webhook pointed at it.
- Run their `step0_fork_prs --org oabcp-bench --name openab` to recreate the
  golden PRs. Pace the forks (the plane will open one council per PR-opened
  event; don't fire 50 at once).
- The chair's single report comment is a "general comment" in their model;
  their step2 already LLM-extracts individual issues from general comments,
  so our report format needs no change.
- Local patch to their step1: register `openab` and add it to
  `_NON_BOT_TOOLS` (our chair posts under the owner PAT, not a bot account).
- Run steps 2 / 2.5 (dedup, always on) / 3 with a pinned judge model via the
  OpenAI-compatible `MARTIAN_BASE_URL`/`MARTIAN_MODEL` env.
- Shake out plumbing and cost on a one-repo slice (10 PRs, `sentry.json`)
  before the full 50.

### 3. A/B arms on the same golden PRs

The pipeline treats each tool name as a separate tool, so an arm is just a
second fork pass with a different `--name`:

- `openab-council` vs `openab-solo` (Solo coordinator already exists) — the
  architecture question no public benchmark answers; vendors compare tools,
  not coordination shapes.
- Context ablation: reviewers with a diff-only preset vs today's
  `gh pr checkout` habit. SWE-PRBench predicts diff-only wins; if it does,
  that's a preset change paid for by one experiment.

The judge is blind by construction: its prompt contains only the golden
comment text and the candidate text (verified in
`step3_judge_comments.py` — tool identity never reaches the judge).

### 4. Scoring rules: pinned and versioned

- Martian pipeline at a pinned commit; judge model and prompt recorded next
  to every result (their results dir is already keyed by judge model).
- Report precision, recall, and cost-side numbers together —
  time-to-verdict (already in the plane's session record since A1) and
  findings-per-PR (noise proxy). Never a single headline number.
- Any change to labeling rules, judge model, or pipeline commit starts a new
  result series; old series are kept, not overwritten.

## What this deliberately does not need

- **No plane changes.** The webhook and chair path *are* the thing being
  measured; an eval-specific code path would measure something else.
- **No new panel, template, or shim** — the B1 pattern stays for panels that
  need non-GitHub input; the bench input is GitHub PRs.
- **No synthetic bug injection.** Demoted to a recall smoke-test if ever
  needed; never a headline number (DeepSource: synthetic ground truth is
  untrustworthy).
- **No SWE-PRBench harness integration yet** — its dataset is a possible
  supplement later; its context-ablation lesson is imported as an arm in §3
  instead.

## Dogfood plan

1. Ledger over #59–#71: a re-read and a table, zero infra. Calibrates our
   own noise level before any external comparison.
2. 10-PR sentry slice through the Martian pipeline, council mode only.
   Verify their extraction/judge handles the chair report; get a first P/R.
3. Full 50 with arms (council vs solo, checkout vs diff-only). Tune angles
   and presets from the per-repo / per-severity breakdown; feed conclusions
   back into the pr-review skill and roster presets.

## Deferred

- Martian online track (fresh PRs via BigQuery, anti-contamination) —
  revisit if numbers ever go external.
- Automated author-time-on-noise telemetry — the ledger's outcome column is
  the manual proxy until finding→reaction tracking exists.
- Per-angle recall attribution on the bench (which reviewer angle caught the
  golden issue) — needs reviewer-message-level export, not just the chair
  report; only worth it once §2 runs routinely.
