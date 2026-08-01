# ADR 034 — Controller registrations are mutable, and no change may lose a task

Status: proposed · 2026-08-01

Builds on: [ADR 031](031-provider-neutral-kernel.md) (the controller as the
only GitHub writer, registered on the plane); informed by the 2026-07-31
`github-prod-2` incident.

## Context

A controller registration ("controller installation" in the API — a
plane-internal credential row, not a GitHub concept) carries five kinds of
configuration. Today they have three different mutabilities:

| Setting | Mutable after create? | Route |
|---------|----------------------|-------|
| `enabled` | yes | `PATCH /v1/controller-installations/:id` |
| action tokens | yes (rotate/revoke, overlapping) | `POST/DELETE …/tokens` |
| event endpoint + event-type grants | yes | `POST …/events` |
| `max_concurrent_sessions`, `max_actions_per_minute` | **no** | create-only |
| action grants + scope bindings | **no** | create-only |

The store already has `upsert_controller_installation`,
`set_controller_action_grant`, and `set_controller_scope_binding`; the gap
is purely that no HTTP route exposes them after create.

On 2026-07-31 production needed its limits raised from 5/60 to 20/120.
Because limits are create-only, the only path was: create `github-prod-2`,
repoint the controller, disable `github-prod`. That replacement — not the
limit change itself — cost real work:

1. **The 401 window.** Disabling the old registration before the
   controller restarted onto the new one rejected `open_session` for ~60s;
   webhook-triggered rounds in that window were lost until manually
   redelivered.
2. **Orphaned in-flight sessions.** Sessions opened under `github-prod`
   had their `session.terminal` events signed against a registration that
   was now disabled. Verification fails closed, so five nuphos PRs' rounds
   finished on the plane and never reached GitHub — silent loss, found by
   a human asking "這裡為什麼沒有反應".

The operator's standing requirement, verbatim: **不能丟掉任務** — no
configuration change may lose a task. A capacity knob that can only be
turned by replacing the credential identity is structurally incompatible
with that requirement.

## Decision

### 1. One PATCH route mutates everything but identity

`PATCH /v1/controller-installations/:id` (operator auth) accepts a partial
document:

```json
{
  "enabled": true,
  "max_concurrent_sessions": 20,
  "max_actions_per_minute": 120,
  "actions": ["open_session"],
  "scopes": ["tenant:prod/resource:canary"]
}
```

- Absent fields are untouched (today's `enabled`-only PATCH is the
  degenerate case, so the route stays backward-compatible).
- Limits take effect on the next admission check — they are already read
  per-request from the store, so no restart is involved.
- `actions`/`scopes` are replace-sets routed through the existing grant
  setters. The registration `id` and its token history are immutable.
- **The whole PATCH is one store transaction.** The grant setters run
  inside a single IMMEDIATE (SQLite) / serialized (Postgres) transaction
  with the limits update, and the replace-set clears and re-inserts within
  it. A reader can never observe a privilege-widened intermediate state
  (old scopes + new actions, or a half-applied replace-set), and a failed
  PATCH leaves the registration exactly as it was.
- Every PATCH lands in the event-audit trail (who-free: operator auth is
  a single key today; record the changed fields and the before/after).

### 2. `enabled=false` means "no new work", never "reject finished work"

The semantic that orphaned the five rounds: verification treated a
disabled registration as nonexistent. Redefined:

- **Admission — precisely `open_session`** (any action that creates a new
  session): gated by `enabled`. Disabled = refuse new work.
- **Every other action kind, when it targets a session opened under this
  registration** (message posts, status emissions, close paths — whatever
  the action surface grows): allowed while disabled. Draining includes
  finishing, and finishing may require actions, not just events. Today's
  code gates all action kinds uniformly on `enabled`; that uniform gate is
  exactly what this ADR changes — the gate becomes
  `enabled OR targets-own-in-flight-session`.
- **Runtime-event signing for the controller** and **terminal-event
  verification from the controller's sessions**: valid for any
  registration that *exists*, enabled or not. A session opened under a
  registration may always finish under it.

Disabling becomes a drain operation by construction: flip `enabled` off,
in-flight sessions complete and their events still verify, nothing new
starts. The registration-overlap trap recorded in the ops repository's
runbooks (openab-control-plane-ops, chair-failover runbook trap 5) stops
being load-bearing — the failure it guards against becomes
unrepresentable.

Hard deletion (if ever added) must refuse while sessions opened under the
registration are still open.

### 3. Replacement is no longer a capacity tool

With 1 and 2, changing limits is one PATCH and changing credentials is
token rotation (already overlapping). Creating a `-2` registration remains
possible but is reserved for genuine identity splits (a second controller
deployment), not configuration.

## Consequences

- The `github-prod-2` incident class is closed twice over: limits change
  without replacement, and even a replacement-with-disable cannot orphan
  in-flight sessions.
- `github-prod` (disabled) regains meaning as a drained, auditable
  historical identity rather than a landmine of forever-unverifiable
  sessions.
- Slightly weaker invariant on disabled registrations: their signing
  material stays live for verification. Acceptable — the token is still
  required for actions, and actions are refused; verification-only
  liveness leaks nothing.
- Migration: none. Both stores already hold the rows; the change is
  routes + the verification predicate + tests (a disabled registration's
  in-flight terminal event must verify; its `open_session` must be refused
  with an explicit disabled 403 (not a credential 401 — the identity still
  authenticates, which is what makes the refusal diagnosable); a
  PATCH of limits must apply without restart).

## Non-goals

- Self-service PATCH by the controller itself (operator-only remains).
- Per-repo or per-scope rate limits (out of scope; the global pair has
  been sufficient).
- Renaming "controller installation" away from GitHub's "installation"
  vocabulary — repeatedly confusing, but a separate, mechanical change.
