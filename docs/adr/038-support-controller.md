# ADR 038 — Support controller: the agent as an ordinary forum account

Status: proposed · 2026-08-02 · companion to ADR 037 (shared controller
skeleton)

## Context

The forum support agent exists and is live, but in a shape this ADR
supersedes the trajectory of:

- Forum PR #211 (2026-07-18) shipped the north client — staff-only ticket
  panel, five proxy routes, per-trigger lock, audit log. PRs #215/#220
  refined it. (`docs/forum-north-client-plan.md`'s "parked, not built" note
  is stale and corrected alongside this ADR.)
- A dedicated plane (`forum-ocp`, 0.1.38) runs Allen (`allen:chair`,
  `mode:"solo"`) with a full tool belt: `zforum-post/reply/search`,
  `zadmin-*`, deployment logs, Grafana, RAG. The pod already writes forum
  replies **as its own account** — the write path needs nothing new.

The product direction: the agent participates as an **ordinary forum
account with no special entry point**. Users post tickets; the agent shows
up and replies like any other participant. Staff stop being the trigger.

That flips the driver of the loop from human to machine, which per the
ADR 022/037 pattern demands a durable controller: today's thin proxy has no
state machine, no admission control against untrusted triggers, no
iteration caps, no fail-closed parking.

## Decision

Ship a **support-controller**: a small external service that turns forum
events into OCP sessions and enforces policy over Allen's declared actions.
Zero kernel diff, zero new mode — sessions stay `solo` on `forum-ocp`.

### 1. Shape

```
forum (webhook: ticket created / user replied)
   │ HMAC
   ▼
support-controller: ingress → admission → state machine ⇄ journal (Postgres)
   │ open_session (installation token, open_session grant only)
   ▼
forum-ocp ──ws──► Allen pod ── zforum-reply (its own account)
   │ session terminal + [[action:…]] trailer
   └─► controller reads trailer → enforce / park
```

### 2. Admission is deterministic, intelligence stays in the pod

The controller contains no decision an LLM would be needed for. Admission:
category allowlist, per-ticket and global rate caps, staff-assigned tickets
are skipped, dedupe by `trigger_fingerprint = hash(ticket_id,
latest_post_id)` (one response per user turn, plane-side active-session
rule as the second layer). How to investigate and what to write is steering
(Allen's), never controller code — the boundary proven by the
`[[verdict:]]` philosophy (design B4).

### 3. Run state machine, capped and fail-closed

```
idle ─event─► investigating ──[[action:replied]]──► watching ─user reply─► investigating
                   │ [[action:escalate]] / unparseable trailer / timeout / cap (5)
                   ▼
              needs_human
```

`needs_human` parks the run: staff-only marker on the ticket + Discord
alert; humans take over through the existing staff panel, which is demoted
to operator escape hatch (session log, fresh session, audit) — not removed.

### 4. Autonomy ladder, enforced at the opening input

Per-category autonomy level: `off` / `draft-only` / `auto`. Because the pod
posts replies itself mid-session, the gate cannot sit after the action the
way ADR 037's council gate does — so **the level is injected into the
opening input** ("this category: do not post; end with `[[action:draft]]`
and the draft"), and the controller's post-hoc check only verifies the
declared trailer matches policy; a mismatch parks the run and alerts.
Rollout mirrors the github-pr-controller `OperatingMode` ladder: everything
`draft-only` first (drafts land in the staff panel), `auto` per category
only on quantified evidence, the promotion recorded in this file's
amendments.

### 5. Journal targets Postgres from day one

ADR 033's migration cost was paid once; not again. Schema (`runs`,
`transitions`, fingerprint uniqueness) follows github-pr-controller's
store conventions, Postgres-only in deployment; SQLite at most for local
tests. The instance lives in the forum-ocp domain — never shared with the
code-review lanes' databases (failure-domain separation, third application
of the rule after ops-watchdog and forum-ocp itself).

### 6. Credentials stay asymmetric

- Toward OCP: new controller installation (`support-controller`), action
  token granting `open_session` only; v1 polls `/v1/sessions/:id` for the
  terminal (ADR 037's tradeoff), event grants are v2.
- Toward forum: **inbound webhook only, no forum write credential.** All
  writes are Allen's own tools. A compromised controller cannot post.
- Deployed off the forum-ocp box (the ops-watchdog precedent).

### 7. Operational promotion of forum-ocp is a prerequisite

forum-ocp is tier-labelled dev but is the only instance behind the
production forum. Before any user-facing autonomy: add its `/healthz` and
the controller's to the ops-watchdog patrol, add it to the backup schedule,
and treat its upgrades as production (the envs/dev/forum-support.md note
becomes policy).

## Alternatives considered

- **Keep staff-triggered only** — works, is live, but never reaches the
  product goal; the human trigger is the bottleneck by design.
- **Forum-side bot, no OCP** (a worker that calls an LLM API from the forum
  app) — loses sessions, roster, liveness watchdog, SSE audit trail, and
  the already-running Allen pod; rebuilds coordination badly.
- **Auto-trigger logic in the plane** — a `/v1/forum` endpoint was already
  rejected once at triage scale (ADR 014); provider ingress belongs to
  controllers (ADR 031).

## Consequences

- Third external consumer, second machine-driven controller. The skeleton
  shared with ADR 037 (journal, fingerprints, caps, `needs_human`) is now
  duplicated twice by design — extraction into a shared crate is warranted
  the moment a third machine-driven controller appears, and the two
  schemas should not diverge gratuitously in the meantime.
- The agent becomes user-facing under its own identity: honest (users see
  the account) but irreversible in reputation terms — hence the ladder and
  the fail-closed parking are load-bearing, not optional hygiene.
- New standing surfaces to operate: forum webhook secret rotation, the
  installation token, the Postgres instance, controller deploy — all enter
  the ops inventory on day one.
- Known debts at birth: terminal polling (v2: event grants), post-hoc
  trailer verification for `auto` categories (the pod acts before the
  check; the mitigations are steering discipline and the ladder), staff
  panel remains root-key-based (scoped read tokens are a plane roadmap
  item, not blocked on).
