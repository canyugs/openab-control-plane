# ADR 013 — Decision→review-state: structured verdict + GitHub review as source of truth

Status: **accepted** · 2026-07-02 · **§1 amended 2026-07-09** (thin GitHub review
dropped — see Amendment below)

> **Amendment (2026-07-09) — §1 thin GitHub review removed; commit status is the
> single GitHub-side verdict signal.** §1 introduced a thin `gh pr review
> --approve|--request-changes` *because at the time there was no other merge-UI
> signal* — this ADR explicitly **Deferred** "Commit status / Checks integration".
> That deferred item has since landed (the chair sets `openab/council` commit
> status with a `target_url` to the report comment). Running both left two defects:
> the review carried a redundant `"… — see the review comment"` one-liner, and a
> `request-changes` review **lingers per-PR** (GitHub keeps it in the timeline until
> superseded/dismissed) so a fixed PR still showed a stale "changes requested"
> state. The commit status is the better primitive: it is **per-commit**, so a fix
> pushes a fresh `success` on the new head and nothing stale remains. The chair now
> sets only the commit status and no longer submits a `gh pr review`. §2 (the
> machine-readable `[[verdict:…]]` trailer — the *plane's* record) and §3–§5 are
> unchanged. Repos that gated merges on the council's *review* should require the
> `openab/council` *status check* instead (the per-commit primitive). Steering:
> `scripts/pr-review-chair-task.tmpl` + `docs/steering/pr-review.md`.
>
> **Amendment (2026-07-11) — the review is back, but configurable.** The 2026-07-09
> removal made the commit status the *only* signal, which cannot satisfy branch
> protection "Require approvals" (a status check ≠ an approving review). To support
> CodeRabbit-style approval, the chair review is reintroduced behind
> `OABCP_COUNCIL_REVIEW_MODE` (default `approve`): `status` = the 2026-07-09
> behavior (no `gh pr review`); `approve` = submit a formal **APPROVE** on approve
> verdicts only (the request-changes-lingers defect above is avoided by *not*
> submitting REQUEST_CHANGES); `enforce` = symmetric, submit REQUEST_CHANGES too
> (accepts the lingering-review tradeoff in exchange for a hard merge block). The
> commit status is still set in every mode. Chair token already has
> `pull_requests:write`; the repo must enable "Require approvals" for an APPROVE to
> count.

## Context

Today the council's outcome is one edited PR comment (chair-maintained) whose
last line is prose: `Verdict: **approve** | **request changes**`. Nothing
machine-readable reaches the plane — the session closes with a free-text
`verdict` string. Consequences:

- GitHub's merge UI does not reflect the council's decision (no review state,
  no label); a red verdict is just a comment anyone can scroll past.
- The plane cannot answer "how many 🔴 findings did this PR get?" — which is
  the data spine for outcome tracking and, later, the ADR 006 commercial model
  ("free on green, charge on red/yellow").
- The ADR 012 close webhook carries prose, so receivers must parse markdown.

Roadmap Phase 2 names this: "Decision→review-state — chair
approve/request-changes as source of truth + label. Depends on GitHub App
identity" (App identity is live, ADR 004).

## Decision

Split responsibilities the same way as always: **bots do GitHub I/O, the plane
records structure**.

### 1. Chair submits a real GitHub review (bot-side, steering change)

On the quorum turn, after updating the status comment, the chair also runs:

```sh
gh pr review N --repo owner/repo --approve --body "OpenAB Council: approved — see review comment."
# or
gh pr review N --repo owner/repo --request-changes --body "OpenAB Council: changes requested — see review comment."
```

The single edited comment stays the report home; the review is thin and points
at it. The chair's App token already has `pull_requests:write`, which covers
review submission. Labels (`council:approved` / `council:changes-requested`)
are applied with `gh pr edit --add-label` on the same permission; label
creation is a repo-setup step documented in the install guides, not plane work.

### 2. Chair reports a machine-readable trailer (wire convention)

The chair's final plane message (the one ending `[done]`) also carries a
verdict trailer in the existing text-directive style (`[[recruit:]]`,
`[[reply_to:]]`):

```
[[verdict:approve r=0 y=2 g=5]]
[[verdict:request_changes r=1 y=3 g=5]]
```

`r/y/g` = count of 🔴/🟡/🟢 findings in the final report. The trailer is the
**plane's** record; the GitHub review is the **repo's** record. Same
information, two audiences.

The `[[verdict:…]]` trailer is machine-parsed only from the final non-empty
line of the chair's final message, outside code fences. Any other occurrence is
prose.

### 3. Plane parses and stores the structured verdict

At close (normal path), the plane parses the trailer from the chair's final
settled message:

- New nullable columns on `sessions`: `decision TEXT`
  (`approve` | `request_changes`), `findings_red INTEGER`,
  `findings_yellow INTEGER`, `findings_green INTEGER`. Added via idempotent
  `ALTER TABLE` at store init; every existing row stays valid (all NULL).
  Separate columns, not JSON — these are exactly the fields metering will
  query and index.
- Timeout close and a missing/malformed trailer → all NULL plus a
  `tracing::warn`. Nothing breaks; the feature degrades to today's behavior.

### 4. Plane exposes the structure

- `GET /v1/sessions/:id` and the list endpoint include `decision` and
  `findings_red` / `findings_yellow` / `findings_green` (flat, null when
  unset — the `Session` struct serializes directly).
- The north `verdict` event and the ADR 012 close-webhook payload gain the
  same two fields. Webhook receivers stop parsing markdown.

### 5. The plane still never calls GitHub

Unchanged (README principle). If the review submission fails on the pod (e.g.
GitHub forbids a self-review because the App authored the PR), the chair falls
back to comment-only — the trailer still reaches the plane, so the structured
record survives GitHub-side failures.

## Consequences

- **Store**: 4 nullable columns + idempotent migration; `Session` struct grows
  4 `Option` fields.
- **Plane**: one trailer parser (pure function, unit-tested) + wiring in the
  normal-close path; API/event/webhook serialization additions.
- **Steering**: `skills/pr-review/SKILL.md` chair section gains the review
  command and the trailer on the `[done]` line; trigger templates unchanged.
- **Compat**: old chairs (no trailer) keep working — NULL columns, prose
  verdict as today. New chairs against an old plane: the trailer is inert text
  in the thread, harmless.
- Counting findings is the chair's judgment call (it wrote the report). The
  plane does not re-parse report markdown to audit the counts — if counts and
  report ever disagree, the report wins for humans, the trailer wins for
  metering, and the mismatch is a steering bug to fix.

## Deferred

- **Addressed-finding tracking** (did a later commit fix the 🔴?) — needs
  re-review diffing; the metering consumer, not this ADR.
- **`split` decision** for a divided council — chair synthesizes to one of the
  two states today; revisit if real councils deadlock.
- **Commit status / Checks integration** — roadmap Phase 2 "target_url" item,
  separate.
- **Auditing chair counts** against report content — see Consequences.

## Effort

Plane: ~half a day (migration + parser + 3 serialization points + tests).
Steering: one SKILL.md edit, validated by a dogfood PR review.
