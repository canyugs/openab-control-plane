# ADR 026 — A debate Coordinator: resolve conflicting findings before they reach the requester

Status: proposed (parked 2026-08-02) · written 2026-07-13

> **Parked, not scheduled.** Written 2026-07-13 but never committed; recovered
> 2026-08-02 to close the numbering gap (027–035 are on main) and preserve the
> design. Nothing here is implemented — no `Broadcast` action, no `Debating`
> state, no finding trailers exist in the code. ADR 030 later attacked the SNR
> problem from a different side (external high-recall finder behind council
> verification); it does not supersede this ADR — the gap 026 targets
> (two reviewers disagree on the same line, chair synthesizes over the conflict
> unadjudicated) remains open. Revisit if the weekly quality report (ADR 032
> Loop 2) shows disagreed-finding volume worth adjudicating.

## Context

OCP's council is cooperation-only. Reviewers fan findings in-thread; at quorum
the chair synthesizes one verdict (`council_on_done`, `coordinator.rs:184`).
Nothing handles *disagreement*: if rev1 flags a line 🔴 and rev2 clears the same
line 🟢, the chair synthesizes *over* the conflict with no adversarial step. The
survey of multi-agent collaboration (arXiv 2501.06322,
[prior-art-mas-survey.md](../eval/prior-art-mas-survey.md)) names this the gap —
its §4.2.2/§4.2.3 argue competition/coopetition, not more cooperation, is what
lets a council exceed the sum of its members. `design.md` §"residual leaks" cut
the `Rounds`/`AllAngles` debate scaffolding as **speculative policy with no
consumer**. This ADR only earns its place if a real consumer now exists.

### The consumer is the SNR problem

It does. [prior-art.md](../eval/prior-art.md) makes the false-positive tail the
headline metric: **cut noise, don't chase recall.** Conflicting findings are
exactly where false positives hide — one reviewer's 🔴 that another reviewer can
refute is a candidate false positive reaching the requester unchallenged. A
debate round that forces the flagging reviewer to defend against a peer's
refutation, *before* synthesis, is a direct SNR lever: it turns "two reviewers
disagreed, chair guessed" into "the disagreement was adjudicated." That is a
concrete A4 consumer, not a speculative mode — the second completion condition
`design.md` said to wait for.

## What the code already gives us

The debate mode is **a new `Coordinator` impl plus one new `Action`** — the seam
is built for exactly this:

1. **Policy is already swappable.** `Coordinator::on_done` (`coordinator.rs:57`)
   returns `Vec<Action>`; `lookup`/`for_session` selects the impl by `mode`.
   `DebateCouncil` is a sibling of `QuorumCouncil`, no mechanism change.
2. **The quorum → chair path is reusable.** `quorum_actions` (`coordinator.rs:114`)
   already emits the guarded `Transition{Deliberating→Quorum}` + chair `Prompt`.
   Debate inserts a bounded round *between* reviewer quorum and that transition.
3. **CAS guards keep it once-only.** `Transition`/`Close` fire only from their
   `from` state (`coordinator.rs:38`), so a debate round can't double-transition
   or race the watchdog close.
4. **Liveness is free.** `force_close_timeout` still force-closes with a `TIMEOUT`
   verdict regardless of debate state — a reviewer who ghosts a rebuild round
   can't hang the session. No new liveness surface.

## The real gap (and it is the "plane never reasons" line)

Detecting a *semantic* conflict — "these two findings contradict" — is LLM
reasoning, which the plane must not do (`design.md`: "Pipe, not container — the
plane coordinates but never reasons"). If the coordinator parsed free-text
findings to decide they conflict, that crosses the C3 line ADR 019 draws.

The resolution is the same trick the verdict already uses: **compare structured
trailers, not prose.** `parse_structured_verdict` (`coordinator.rs:164`) already
reads a machine trailer off the chair's final (`plugins::pr_review::verdict`).
Extend that to *reviewer* findings: a reviewer emits
`<!-- finding: file=<p> line=<n> sev=RED|YELLOW|GREEN -->` trailers the plane can
compare as tuples. `(file, line, RED)` vs `(file, line, GREEN)` is a **mechanical
tuple mismatch** — the same class of operation as `quorum_reached` counting
done-votes (`session.rs`), not reasoning about content. The plane detects the
*shape* of a conflict; it never judges who is right. Chair (LLM) still adjudicates.

## Decision

1. **Add one Action: `Broadcast { content }`.** Deliver a system/relayed message
   to the full roster, coordinator-ordered — the explicit peer-visibility
   primitive `design.md` reserved ("add debate as an explicit coordinator
   `Action` (`Broadcast`/`Relay-on-message`) rather than reintroducing implicit
   mechanism-side fanout"). This is the *only* new mechanism. The substrate
   invariant holds: peer visibility happens **only** through a coordinator-ordered
   `Broadcast`, never mechanism-side fanout of bot messages.

2. **`DebateCouncil` Coordinator, conflict-triggered, bounded.** At reviewer
   quorum, before the `Deliberating→Quorum` transition, it scans settled reviewer
   findings (via `Ctx::latest_settled` + finding-trailer parse) for tuple
   conflicts. If any: `Broadcast` the conflicting pair with a fixed rebuttal
   prompt, hold in a new `Debating` state for **one** round (config
   `debate_rounds`, default 1 — the deferred per-coordinator column
   `design.md` said to add only when a second coordinator needs its own config).
   No conflict → behaves exactly as `QuorumCouncil` (straight to synthesis).

3. **Convergence is bounded, then synthesis is unconditional.** After
   `debate_rounds` rounds (or no remaining conflict), emit the normal
   `Transition{Deliberating→Quorum}` + chair `Prompt`. The chair synthesizes the
   *post-rebuttal* view. Rounds are hard-capped; the watchdog is the backstop.
   The plane never loops on verdict *content* — it loops a fixed count, exactly
   like a for-loop, not until "the argument is settled."

4. **Diversity is a provisioning choice, reused not invented.** Mixed-provider
   rosters already exist to dodge the N≈5 quota knee (`scale-knee.md`). Debate
   repurposes that heterogeneity as adversarial diversity — a claude finding
   rebutted by a gemini reviewer catches single-model blind spots (the survey's
   §4.2.2 competition-drives-robustness claim). No new provisioning: the same
   blue-green fleet, pointed at refutation instead of idle standby.

5. **Default OFF, opt-in per lane (`OABCP_COUNCIL_DEBATE`).** Ships dark like
   ADR 024/025 failover: enable in dev, confirm on a real conflicting PR against
   the A4 ledger (does it lift SNR?), then prod under the deploy gate. Merging
   changes nothing until the flag is set and the mode is selected.

## Non-goals

- **Not competition on the verdict.** Reviewers debate *findings*; the chair
  still owns the single verdict. No voting on the outcome, no adversarial chair —
  that would reopen the shared-decision-making problem the survey (§6.1) leaves
  genuinely open, and it is out of scope here.
- **No unbounded argument.** `debate_rounds` is a hard cap, not "debate until
  consensus." Consensus-seeking is probabilistic; the plane guarantees
  termination, so it caps rounds and lets the chair decide.
- **No semantic conflict detection in the plane.** Only structured-trailer tuple
  comparison. If a finding carries no trailer, it is not eligible for debate — it
  flows through as today. The plane never reads prose to decide conflict.
- **Not a precision fix on its own.** Debate adjudicates *disagreed* findings; a
  false positive all reviewers share is untouched. It attacks one slice of the
  SNR tail, measured against A4 before any recall claim.

## Build order

1. **Finding trailer + parser.** Extend `plugins::pr_review::verdict` (or a
   sibling) with a reviewer-finding trailer and a tuple type; steering asks
   reviewers to emit it. Pure, testable, no coordinator change — lands first and
   is independently useful (structured findings feed the A4 ledger directly).
2. **`Action::Broadcast` + `Debating` state.** The one mechanism change:
   orchestrator executes `Broadcast` (coordinator-ordered roster deliver) and the
   new state's CAS transitions. Guarded like every other `Action`.
3. **`DebateCouncil`.** Conflict scan + bounded round, selected by `mode` behind
   `OABCP_COUNCIL_DEBATE`. Unit-tested on the pure `Ctx` like `QuorumCouncil`.
4. **A4 measurement.** Run a known-conflict PR through debate vs plain council;
   compare SNR/usefulness-rate on the ledger. Ship to prod only on a measured lift.

## Consequences

- Closes the survey's live critique of OCP (cooperation-only) **without** giving
  up determinism: the new capability is one bounded `Action`, conflict detection
  stays mechanical, liveness is unchanged.
- Structured finding trailers (step 1) are valuable even if debate never ships —
  they let A4 classify findings by CR-Bench's severity×category axes
  ([prior-art.md](../eval/prior-art.md)) instead of scraping prose.
- Cost: one extra round of reviewer latency + tokens on conflicting PRs only
  (no-conflict PRs pay nothing); one new `Action` and session state to maintain.
- If A4 shows no SNR lift, the mode stays dark and the trailer work is still
  banked — a cheap, reversible bet.

## Related

- [prior-art-mas-survey.md](../eval/prior-art-mas-survey.md) — the survey read
  that named the type-dimension gap this ADR closes.
- [prior-art.md](../eval/prior-art.md) — the SNR/false-positive framing that makes
  debate a real consumer, not speculative policy.
- ADR 019 (untrusted-PR-input boundary) — the "plane never reasons over PR
  content" line; structured-trailer comparison stays behind it.
- [design.md](../design.md) — the substrate invariant (explicit `Broadcast`
  Action, never mechanism-side fanout) and the "add config when a second
  coordinator needs it" discipline this ADR follows.
- ADR 015 / 020 / 021 (eval harness, effectiveness ledger, feedback loop) — where
  the SNR lift is measured before prod.
</content>
