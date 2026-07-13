# ADR 024 — Blue-green chair: an alternate-provider standby for the last monoculture

Status: proposed · 2026-07-13

## Context

On 2026-07-13 a shared `KIRO_API_KEY` hit its usage quota. Both lanes' kiro
**chair** then returned `-32603` on every request while staying `connected`, and
prod's council was silently unable to produce a verdict for ~a day (found only by
a manual PONG smoke). ADR 023 addresses *detecting* that state
(`connected ≠ healthy`). This ADR addresses *routing around* it for the chair.

Reviewers are already multi-provider with blue-green standbys (claude / codex /
grok, connected-but-off-roster). A degraded reviewer is a pure roster `PUT`
away from being routed around. **The chair cannot be** — issue #227. It is the
last single-provider monoculture: a chair-provider outage has no automatic
route-around and is alert-only.

### What the code already gives us (the surprise)

Auditing the convene + token paths, the blue-green chair is **mostly already
mechanized** — the chair is not special the way the issue assumed:

1. **Chair is positional, not an identity.** `chair_bot = eff_roster.first()`
   (`council.rs:410`). The chair is whatever bot sits at `roster[0]`; its bot id
   need not be `"chair"`.
2. **Write scope is per-bot, from the DB `role` column.**
   `Role::from_bot_role(&bot.role)` → `Chair` grants `pull_requests:write`
   (`github_app.rs:47`, `api.rs:1473`). A *second* bot with `role = "chair"` on a
   different provider mints the same write-scoped installation token.
3. **The GitHub App identity is the plane's, not the pod's.** The standby posts
   as the **same** App (opencodezebra) via a minted token — no second App, no
   second identity to register. This is the real enabler.
4. **Roster override is a runtime DB `PUT`, no restart.**
   `set_standing_roster` (`api.rs:880`) is the failover lever that already exists.
5. **`validate_standing_roster` keeps promotion atomic** (`api.rs:940`):
   `roster[0]` must be `role == "chair"`; **no other entry may be**. So the roster
   never holds two chairs at once — a promotion `PUT` swaps in one step.

So manual failover is possible **today**: register a second `role = "chair"` bot
on another provider, keep it connected-but-off-roster, and on a chair outage
`PUT` a roster with the standby at position 0.

## The real gap (and it is security-shaped)

Because write scope follows the static per-bot `role` and **not the live chair
slot**, `bot_github_token` (`api.rs:1455`) mints `pull_requests:write` for *any*
bot whose `role == "chair"`, with **no check that it is the currently-active
chair** (`roster[0]`). Stand up a connected standby chair and you now have **two
pods able to write to PRs as the App simultaneously** — the off-roster one holds
a *latent* write capability it never legitimately needs (it receives no session
triggers). That widens the write-capable surface the ADR 019 least-privilege
model exists to shrink.

This is the crux that makes #227 "a project, not a config flip": not the App
wiring (shared) and not the failover lever (exists), but that **write capability
is bound to a role label instead of the active slot.**

## Decision

1. **Bind chair write scope to the active chair slot, not the `role` label.**
   `bot_github_token` grants `Role::Chair` scope only when the requesting bot is
   `roster[0]` of the current standing roster; otherwise it downgrades to
   `Role::Reviewer` (read). Write capability then *follows the chair slot* — a
   standby chair holds no latent write power until it is actually promoted, and
   promotion (the roster `PUT`) is the single audited event that transfers it.
   This is strictly more least-privilege than today even for the single-chair
   case.

2. **Standby is connected-but-off-roster, mirroring reviewers.** A second
   chair-capable pod on a different provider (claude), carrying the chair task
   template + steering + the plane-token path (ADR 019 D1), seeded `role =
   "chair"`, kept off-roster. Provisioning follows the reviewer blue-green
   runbook `openab-control-plane-ops/envs/prod/mixed-provider-promotion.md`.

3. **Failover = atomic roster `PUT`, bounded + alerted (ADR 023 Decision 5).**
   On the primary chair going `degraded` (ADR 023 Phase 1 signal), promote the
   standby by `PUT`-ing `[standby, ...reviewers]`. Manual now; automatic promotion
   is gated on ADR 023's degraded signal landing, must be bounded (one promotion
   per cooldown), and must alert — never a silent flap.

4. **No auto-recovery of the demoted chair.** A demoted primary stays off-roster
   until a human clears the external cause (quota raised, key rotated, re-auth) —
   the same human-gate ADR 023 Decision 4 draws around the external failure class.
   Auto-promote is safe (route to a known-good standby); auto-demote-back is not
   (it re-points at the still-broken provider).

## Build order

1. **Decision 1 — the write-scope gate.** Code + tests in `bot_github_token`:
   look up the standing roster, grant `Chair` scope iff `bot.id == roster[0]`,
   else `Reviewer`. This is the whole security-relevant change and it stands
   alone — it hardens the *single*-chair deployment immediately, independent of
   ever provisioning a standby. Ships through the dev→prod deploy gate.
2. **Standby provisioning (ops).** Second-provider chair pod, off-roster, per the
   reviewer runbook. No plane code.
3. **Manual failover runbook.** Document the promotion `PUT` and the
   no-auto-recovery rule in the ops deploy-gate runbook.
4. **Automatic promotion.** Gated on ADR 023 Phase 1. Bounded + alerted.

## Consequences

- Removes the single-provider-chair SPOF the 2026-07-13 incident exposed.
- Decision 1 tightens least-privilege for **every** deployment, not just
  blue-green ones — the highest-value, self-contained slice, so it ships first.
- Two `role = "chair"` bots can coexist safely because write power is slot-bound,
  not label-bound — the standby is inert until promoted.
- Cost: a second always-on chair-provider pod (idle compute) and one more
  provider account/quota to keep funded.

## Related

- ADR 023 (bot agent-level liveness) — Decision 5 names this the highest-value
  follow-up; automatic chair failover is blocked on ADR 023 Phase 1.
- ADR 019 (untrusted-PR-input boundary / D1 plane-delivered tokens) — Decision 1
  extends its per-role least-privilege model from role-bound to slot-bound.
- Issue #227.
