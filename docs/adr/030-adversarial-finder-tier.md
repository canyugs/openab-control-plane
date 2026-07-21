# ADR 030 — Adversarial finder tier: a high-recall external judge behind council verification

Status: proposed · 2026-07-20

## Context

A three-source review-quality audit (2026-07-20) compared the council against
CodeRabbit and a Codex-highest judge (`gpt-5.6-sol`, each PR's diff piped to
`codex exec --output-schema`) over 52 prod PRs. Codex raised **41 reds** to the
council's 18 and CodeRabbit's 10, blocking 24 PRs to the council's 6. The
question this ADR settles: is that aggression the direction the council should
move?

Fifteen context-aware adjudicators then read the actual code at each PR head to
classify the contested reds (codex-solo + the PRs where council alone passed):

- **12 real recall gaps** the council genuinely missed (stale `hasK3s` after a
  skip-path reinstall; `getIamPolicy` without policy v3 erasing conditional
  bindings; case-insensitive GUID rebind bypassing an export guard; a one-click
  cascade delete where the app's own `ConfirmDialog` convention was skipped).
- **4 minor** — real but the council's LGTM was defensible.
- **9 flat false positives** — every one a Codex artifact of judging the diff
  *without repo context* (gqlgen generated code that IS committed; config
  defaults that ARE handled elsewhere).

So on contested reds: 48% real, 36% false. **If Codex reds were the merge gate,
block-precision would be ~57%** — roughly four in ten unilateral blocks wrong.

That is exactly the noise regime the precision gate (11%→100%, ADR 021 lineage)
was built to kill, and the reason CodeRabbit was replaceable. Precision at the
*blocking boundary* is not optional: a gate that wrongly blocks one PR in three
trains developers to ignore it. Yet the same aggression surfaced 12 real defects
the precise council missed. Both facts are true.

## Decision

Aggression is the right posture for a **finder**, not for a **verdict**. We
separate the two roles rather than dialing one knob.

1. **Targeted recall, not a lowered bar (shipped).** The verified miss *classes*
   — not the raw aggression — become steering probes: skip-path invariant
   staleness, read-modify-write completeness, identity normalization, and
   destructive-default confirmation (PR #268). This adds recall only in
   directions we have evidence for, leaving the blocking bar precise. The
   config-default false-positive class is deliberately excluded.

2. **An adversarial finder tier (proposed).** A high-recall/lower-precision
   reviewer (Codex-class, and eventually a repo-context-fed variant) joins the
   roster as a **non-voting** black-hat: it generates candidate reds but its
   findings do NOT block merge on their own. A candidate red becomes a blocking
   `🔴` only after the chair or another reviewer independently verifies it
   against real code — the same find→adversarially-verify shape the council
   already runs internally. This buys the recall without importing the 36% into
   the gate.

3. **Give the finder context the audit denied it.** Codex's false-positive half
   was largely diff-only blindness. The finder tier reads the repo at head
   (definitions of consumed symbols, callers, conventions) before emitting —
   closing the gap that produced the gqlgen/config-default noise.

The blocking verdict stays governed by the precision gate. Only 🔴 blocks; the
finder's unverified candidates ride as notes until a verifier promotes them.

## Consequences

- **Recall rises without precision loss at the gate** — the expensive property.
  The finder is free to be noisy because a skeptic stands between it and merge.
- **Cost.** A per-PR external-model pass plus a verification hop. Bounded by
  running the finder only on diffs that touch its high-yield surfaces (shared
  state, guards, credential/permission boundaries, destructive actions), not
  every PR.
- **A new asymmetry to watch:** the verifier can rubber-stamp or over-reject the
  finder. Its promote/reject rate is itself a metric (feeds SEI-802 per-angle
  SNR); a verifier that promotes ~0% or ~100% is miscalibrated.
- **Measurement is now repeatable.** The archive + comparison + adjudication
  pipeline (`build-review-archive.py`, `codex-judge.py`, `review-compare.py`,
  the recall-adjudication workflow) can rerun monthly to track whether the gap
  is closing — the recall evidence M4 requires.
- **Relation to ADR 021 / 029:** this extends the human-gated improvement loop
  with an automated high-recall source; it does not auto-tune the bar. The
  finder is an input to judgment, not a new authority — same stance ADR 029
  takes toward the external quota reporter.
