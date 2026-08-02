# ADR 030 pt.2 — Adversarial finder tier: implementation plan

Status: draft 2026-07-22 · re-verified against source 2026-07-23 · grounds
ADR 030 "adversarial finder tier (proposed)"

> **Parked (committed 2026-08-02).** Written alongside ADR 030 but never
> staged. The finder tier itself is unimplemented — only the steering probes
> shipped. Code references (line numbers, `council.rs` internals) reflect the
> 2026-07-23 tree, before the external-controller cutover; re-verify against
> current source before executing.

## Re-verification 2026-07-23 — plan holds, two new wrinkles

Verified verbatim: chair sets `openab/council` via `gh api` from its own
verdict (chair-task.tmpl step 5 — prose gate, no any-red-blocks code);
quorum = distinct non-chair reviewers with 🆗 (`src/session.rs:10-29`);
`quorum_n = eff.len()-1` = ALL participating reviewers (`council.rs:301`);
reviewers self-fetch repo-at-head (`tasks.rs:110-116`). Core thesis intact.

**Wrinkle 1 — preset/angle trimming (`assign_angles`, `council.rs:279-302`).**
`participating = reviewers[..angles.len()]` in ROSTER ORDER; extras are trimmed
from the session. lite=1 / quick=3 / standard=5 angles. Consequences:
- Under `lite`, a finder placed 2nd+ in the roster is silently trimmed — never
  runs. Under `quick` with 3 reviewers (claude + codex + finder) all three
  participate, so dev (preset=quick) is fine, but roster ORDER decides who sits
  out when angles < reviewers. Phase 1 must pin the finder's roster position and
  run a preset whose angle count ≥ reviewer count.
- A participating finder gets a POSITIONAL angle ("→ integration"), which
  conflicts with its black-hat brief. Phase-1 fix: finder steering explicitly
  overrides ("ignore any assigned review focus; you are the adversarial
  finder"). Phase-2 fix: the `finder` role skips angle assignment entirely.

**Wrinkle 2 — steering delivery is one zip for ALL pods.** sync-steering
uploads a single `steering.zip` that every pod pre_seeds. A finder needs a
DIFFERENT doc. Fix: per-pod pre_seed URL — the finder pod's config.toml points
at its own R2 key (`steering-finder.zip`). Config-only, blue-green tooling
already writes per-pod config; sync-steering grows a second artifact.

Quorum-stall risk (finder counts toward quorum when participating) is confirmed
in code, as assumed — mitigation choice (a)/(b) below unchanged.

## The load-bearing discovery

Two facts from the plane make this **mostly steering, not code**:

1. **The merge gate is chair prose, not a code rule.** `openab/council` commit
   status is set by the *chair agent* via `gh api ... /statuses` from its own
   synthesis (`scripts/pr-review-chair-task.tmpl:55-62`; confirmed
   `src/github_app.rs:64,395`). There is **no** `any-red-blocks` arithmetic in
   the plane — `verdict.rs` only *parses* the chair's `[[verdict:…]]` trailer
   into a mirror (`src/plugins/pr_review/verdict.rs:12-48`). So "a finding blocks
   merge" is already a chair decision for *every* reviewer. "Non-voting" is not a
   new gate behavior — it's a chair instruction.

2. **Reviewers already self-fetch repo-at-head.** Reviewer tasks hand a PR
   pointer, not a diff, and tell the bot to `gh pr diff` / `gh pr checkout`
   (`src/plugins/pr_review/tasks.rs:110-116`, ADR 004). ADR 030 pt.3 ("give the
   finder the repo context the audit denied it") needs **no plumbing** — just a
   finder prompt that says "read definitions of consumed symbols before emitting."

So the finder tier ≈ one aggressive bot + two steering docs + one small plane
guard so it can't become a SPOF. That's the whole thing.

## What the plane does NOT have (net-new if we want it)

- Role is a bare 2-value string `chair|reviewer` (`src/store.rs:26-31`); unknown
  roles collapse to `Reviewer` (`src/github_app.rs:50`). No advisory/weight.
- **quorum_n = reviewer_count** — every non-chair roster member must post 🆗 or
  the chair is never prompted (`src/session.rs:10-29`, `council.rs:345`,
  `coordinator.rs:120-136`). A finder added naively becomes a **new stall SPOF**
  (cf. the 2026-07-13 bot-health incident — a quorum member going quiet took
  prod down).
- Findings carry a free-form `raised_by` + a `status` (open|resolved|dismissed)
  already (`src/plugins/pr_review/findings.rs:27-49`). Enough to tag + promote
  without schema change.

## Phase 1 — MVP, steering-only, DEV (no plane build)

Ship the behavior first; earn the code later.

1. **Add a finder bot to the DEV roster** as a normal `reviewer` (register_bot +
   roster/replace, blue-green tooling already does this). Provider = codex (the
   proven-aggressive finder from the 07-20 audit).
2. **Finder steering** (`steering/pr-review-finder.md`, delivered via the same
   R2 pre_seed the reviewers use, per-bot doc): black-hat brief — hunt the
   ADR-030 high-yield surfaces (shared state, guards, credential/permission
   boundaries, destructive actions); **read repo at head** for every consumed
   symbol before emitting; emit candidates as **yellow/advisory, never red**;
   verbosity is fine, this bot is *supposed* to over-produce.
3. **Chair steering delta** (`docs/steering/pr-review.md` chair section): "The
   finder (`raised_by == <finder bot name>`) is a non-voting black-hat. Its
   findings are *candidates*, not peer verdicts. Promote a candidate to a
   blocking 🔴 **only** after you or another reviewer independently verify it
   against real code; otherwise carry it as a note. Finder candidates never set
   `request_changes` on their own." The precision gate at the status boundary is
   untouched, so block-precision stays high by construction.
4. **Tag for measurement**: finder findings already land in the ledger with
   `raised_by = finder`. Its **promote rate** (candidates → chair-verified reds)
   is the ADR-030 verifier-calibration metric; read from `GET /v1/review/findings`
   / `pr_review_findings`. ~0% or ~100% promote = miscalibrated.

**Measure on DEV**: does council recall on the M4 gap-classes rise while the
`openab/council` block-precision (the 100% we defended) holds? That is the exact
property ADR 030 buys. This is also the *definitive M4* the offline harness
couldn't do — a real full-council run with the finder in it.

### Phase-1 open risk: the stall

As a plain roster reviewer, the finder is a quorum member. Two mitigations, pick
per DEV behavior:
- **(a) accept it** for MVP — a healthy bot reports like any other; rely on the
  existing bot-health failover (ADR 023). Simplest.
- **(b) run the finder off-roster** — a separate pass whose candidates are
  injected into the chair's context, never a quorum member. No stall, but needs
  a new orchestration hop. Defer to Phase 2.

Recommend (a) for the DEV MVP; if the finder's repo-reading latency drags close
time or a finder hiccup stalls quorum, that empirically justifies Phase 2.

## Phase 2 — earn the code (only if Phase 1 proves the value)

1. **`finder` role, excluded from quorum.** Add a third role value; exclude it
   from `reviewers()` / `quorum_reached` (`src/session.rs:10-29`) and the
   `assign_angles` quorum derivation (`council.rs:323-347`). Finder becomes
   best-effort: chair sees its candidates if present, proceeds without it if not.
   Kills the stall-SPOF. Map the role to read-only token scope in
   `Role::from_bot_role` (`github_app.rs:37-59`) — it already defaults unknown
   roles to Reviewer, so this is a targeted branch.
2. **Chair-waits-but-doesn't-require semantics.** Chair should fold in finder
   candidates that arrive before quorum close, but never block on them. Small
   coordinator tweak (`coordinator.rs:120-136`).
3. **Promote/dismiss as first-class status.** Use the existing finding `status`
   (open|resolved|dismissed) so a chair-verified candidate flips open→promoted
   and a rejected one open→dismissed — makes the promote-rate metric exact
   (`findings.rs`, `orchestrator.rs:603-625`).

## Sequencing & guardrails

- DEV only until Phase 1 recall↑ / precision-hold is shown (deploy-gate rule).
- Bounds from ADR 030: run the finder only on diffs touching high-yield surfaces
  (don't pay an external-model pass on every trivial PR). Phase 1 can gate this
  in finder steering ("if the diff is docs/config-only, return no candidates").
- Cost: +1 reviewer pass/PR + the verify hop. Watch close-time A/B.
- Linear: this is SEI-802's per-angle SNR consumer + a new finder arc; open an
  issue, link ADR 030.

## First concrete step

Register a codex finder bot on DEV + write `pr-review-finder.md` + the chair
steering delta, sync via R2, convene one review on a known gap-class PR, read
the ledger for finder candidates + promote rate. Pure steering, reversible,
no plane build.
