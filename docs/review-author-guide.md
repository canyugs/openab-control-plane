# Talking to the review council — a guide for PR authors

The council reviews your PR and posts a verdict comment with a findings
table. This page is about what you can say back. Everything here happens in
ordinary PR comments — there is no dashboard, no config, no side channel.

The bot's name below is written `@openab-council`; use whatever name signed
the verdict comment on your PR (after an App rename, both the old and the
new name work).

## The short version

```text
@openab-council dismiss F2 the pool is bounded upstream, this can't grow
@openab-council waive F1 known cost, fix scheduled for the storage rewrite
@openab-council reopen F1
@openab-council review switched to the retry approach discussed above
@openab-council why does F3 think the lock is held across await?
```

One command per comment. The `F<n>` ids are the rows of the findings table
in the verdict comment. Lower case (`f2`) works too.

## The two ways to clear a finding — say which one you mean

They look similar and do very different things:

| You believe | Say | What it records |
|---|---|---|
| "This is **not a defect**" | `dismiss F<n> <why>` | The finding was wrong. Feeds the council's precision stats — dismissals teach the council what not to flag. |
| "It **is a defect**, and we accept it" | `waive F<n> <why>` | The defect is real and consciously accepted. Mints a **repo-scoped waiver** (90 days; **30 days for a 🔴 finding**) that future rounds see: the same defect won't block you again while the waiver lives. |

The distinction is the record. A dismissed finding says the council was
wrong; a waived one says the council was right and the team chose the
trade-off. Picking the flattering verb poisons the council's memory — the
next round is steered by what you wrote.

Rules that differ between the two:

- **`waive` requires a reason.** A waive without one is refused (the reply
  tells you exactly what to add). The reason is for humans reading the PR;
  the waiver the ledger keeps carries the council's own wording of the
  finding, not your prose.
- **`dismiss` takes a reason too** — it is what the next round's chair reads
  as your claim — but a missing one is tolerated.
- **Waivers expire** (90 days by default; a 🔴 finding's waiver gets 30 —
  accepted security defects stay conspicuous and come back for
  re-examination sooner, ADR 038). A waiver that keeps being renewed is a
  rule wearing a waiver's clothes: promote it to the repo's
  `review-boundaries` file and let the waiver die.

## What happens when a judgement is accepted

The controller re-computes the verdict from the remaining open findings and
rewrites all three artifacts in place:

1. the verdict comment — the finding moves to a visible `Dismissed` /
   `Waived` section,
2. the commit status (`openab/council`),
3. and if nothing blocking remains, a fresh **APPROVE review** — the thing
   that actually unblocks your merge.

No new council round runs; the recompute is arithmetic over the findings
that already exist. Every command gets a reply, including ones that change
nothing ("already waived", "no such finding") — silence is never the answer.

## `reopen` — the undo

`reopen F<n>` puts a dismissed or waived finding back to open, and if a
waive minted a waiver, revokes that waiver in the same stroke. To renew a
waiver that is about to expire: `reopen F<n>`, then `waive F<n> <reason>`
again.

## Who may judge

Convening a review (`review`, or just opening the PR) needs org membership.
**Judging a finding needs write access to the repository** — probed live
against GitHub at the moment of your comment, failing closed. That is
deliberate: clearing the last blocking finding makes the controller submit
an APPROVE, and merge authority should not come from org membership alone.
If you are refused, the reply says why and what you can do.

## Re-running and asking

- `@openab-council review <optional notes>` — run a fresh round against the
  current head. Your notes ride into the chair's briefing. Pushing new
  commits also triggers a round on its own.
- `@openab-council <anything else>` — treated as a question and answered in
  place. It never modifies findings, so it is always safe.
- `/review` and `/ask <question>` work as slash-command spellings of the
  same two things.

## Worked example

The council posts a verdict: `request_changes`, F1 🔴 "connection pool
unbounded under retry storm", F2 🟡 "test asserts on log text".

```text
you>  @openab-council dismiss F2 the assertion is on the stable public
      error code, not the message
bot>  F2 dismissed · verdict recomputed: request_changes (F1 open) …

you>  @openab-council waive F1
bot>  a waive needs a reason … Usage: @openab-council waive F<n> <why> …

you>  @openab-council waive F1 retry cap lands in the queue rewrite (ETA
      next sprint); pool growth is bounded by the LB in the meantime
bot>  F1 waived until 2026-11-11 · verdict recomputed: approve · APPROVE
      review submitted …
```

The PR is now mergeable, and the next PR that trips the same pool finding
inside 90 days sees the waiver instead of a fresh block.

## Where the deeper rules live

- [ADR 038](adr/038-trusting-the-author.md) — why authors are trusted by
  default, and why dismiss and waive are different claims.
- [ADR 035](adr/035-review-memory-waivers.md) — the waiver ledger: scope,
  expiry, and how waivers reach the chair.
- [github-pr-controller.md](github-pr-controller.md) — the operator-facing
  reference for everything above.
