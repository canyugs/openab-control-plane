# ADR 027 — Quota/billing monitoring lives outside the plane

Status: proposed · 2026-07-14

## Context

The 2026-07-13 incident was a provider **quota** exhaustion (shared
`KIRO_API_KEY`) that surfaced only as opaque `-32603` agent errors. ADR 023
added *symptom* detection (connected ≠ healthy) — but nothing watches the
budget itself before it hits zero.

Provider survey (2026-07, verified):

| provider | usage/limit API | proactive monitoring possible? |
|---|---|---|
| claude | Spend Limits API + native 75/90% alerts | yes |
| grok | management API + native 80% alert | yes |
| codex (subscription login) | none for subscriptions | no — liveness probe only |
| kiro | none (enterprise dashboard only) | no — liveness probe only |

The two providers that actually failed are exactly the two with no quota API.

## Decision

1. **The plane does not own quota/billing monitoring and never holds
   provider billing/admin credentials.** Those keys can spend money or raise
   limits — the same class ADR 023 Decision 4 fences off ("plane never
   auto-raises quota"). Watching budgets is an ops concern, outside the plane
   (`quota_watch` schema + any poller live in the ops repo).

2. **The plane's only quota-related surfaces are:**
   - **Signal ingest**: an external watcher may report a bot's provider as
     budget-degraded through the existing bot-health surface
     (`bots.health` / the ADR 023 update path) — reusing the same WARN +
     Phase 4 failover machinery. No new plane endpoint until a watcher
     actually needs one.
   - **Liveness probing** (ADR 023 Phase 2): the sparse PONG probe is the
     only coverage possible for kiro/codex-class providers, needs no billing
     credentials, and was already decided in ADR 023.

## Non-goals

- No billing API clients, no spend dashboards, no `$` amounts in the plane.
- No plane-side polling of provider consoles.
- The external watcher's design (cadence, thresholds, webhook shape) is not
  fixed here — only the boundary is.

## References

- ADR 023 Decision 4 — the "plane never spends money" line this extends.
- `openab-control-plane-ops/schema/quota_watch.sql` — the external data model.
