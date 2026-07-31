# ADR 033 — PostgreSQL backing store for the plane and the controller

Status: proposed · 2026-08-01

Builds on: [ADR 031](031-provider-neutral-kernel.md) (controller-owned
review closing), [ADR 032](032-review-calibration-loops.md) (the findings
ledger as read infrastructure).

## Context

Both processes persist to pod-local SQLite: the kernel's `plane.db`
(sessions, messages, bots, controller installations/tokens, outbox) and the
controller's `github-controller.db` (webhook deliveries, session targets,
review rounds/findings, the GitHub-writes outbox). SQLite carried the
product from prototype to the org-wide cutover and its transactional
semantics (IMMEDIATE claims, `user_version` migrations) have been correct
throughout. The first evening of full production load exposed the walls,
none of which are about correctness:

1. **Operability.** Live state is only reachable by copying the whole
   database out of the pod (`exec | gzip | base64`) — every incident this
   week paid that tax, the output-SLO watchdog pays it on every poll, and
   the ADR 032 weekly calibration job would pay it too. There is no way to
   run one query against production.
2. **Durability.** The database lives and dies with the volume. The
   2026-07-27 server deletion erased ~1200 sessions and the entire findings
   ledger — precisely the corpus ADR 032 now depends on. Backups are
   volume snapshots at best; there is no point-in-time recovery.
3. **Single-instance coupling.** One pod owns the file, so every deploy is
   a hard handover. Two webhook deliveries were dropped during a ~60-second
   credential-switch restart on 2026-07-31 (recovered only because GitHub's
   redeliver API exists and someone was watching); a shared store is the
   prerequisite for ever running a second replica behind the ingress.
4. **Analytics pressure.** ADR 032's loops join rounds × findings ×
   outcomes weekly. On SQLite that means shipping copies around; on
   Postgres it is a scheduled query with a read-only role.

## Decision

Adopt PostgreSQL as the production backing store for both processes, in
two independent migrations — controller first, then the kernel. SQLite
remains a fully supported backend for tests and local development.

### Shape

- **Both store boundaries already exist**: the kernel's `trait Store`
  (`src/store.rs`) and the controller's `ProductStore`. The migration is
  an alternate implementation behind each boundary, not a rewrite of
  callers. Where the controller's store is concrete today, extracting the
  trait is step zero.
- **Driver**: `sqlx` with runtime-checked queries against both backends,
  or hand-written per-backend SQL behind the trait where dialects diverge
  (`INSERT OR IGNORE` → `ON CONFLICT DO NOTHING`, `user_version` → a
  migrations table, datetime handling). No ORM.
- **Outbox claims get better, not just ported**: the IMMEDIATE-transaction
  claim loop maps to `SELECT … FOR UPDATE SKIP LOCKED`, which is the
  canonical Postgres outbox pattern and removes claim contention outright.
- **One instance per lane** (dev, prod), provisioned as a managed/prebuilt
  Postgres service in the same project as its plane; the controller and
  kernel share the instance with separate databases (or schemas), separate
  roles, least privilege. A read-only role serves the watchdog, the
  calibration job, and humans.
- **Backups**: daily base backup + WAL archiving to object storage (the
  R2 bucket infrastructure already exists for steering). Restore drill is
  part of acceptance, not an afterthought — ADR 032's ledger is the asset
  being protected.

### Migration path (per process)

1. Ship the dual-backend build; CI runs the full suite against both.
2. Stand up Postgres in the lane; run the schema migrations.
3. Quiet window: stop the process, one-shot import (tables are small —
   megabytes), start against Postgres. The window is minutes; the
   webhook-redelivery sweep covers anything that lands inside it.
4. Keep the final SQLite file as the rollback artifact for a soak week;
   rollback is the reverse import.

Controller migrates first: five tables, an existing migrations framework,
and its outbox benefits most. The kernel follows once the controller has
soaked.

## Consequences

- The exec-and-copy operational pattern (and the tooling built around it:
  watchdog DB pulls, incident-time snapshots) is replaced by SQL against a
  role-scoped connection — the watchdog loses its heaviest dependency on
  the zeabur CLI.
- A new stateful service per lane to operate, monitor, and upgrade. On the
  current node the memory cost is modest (Postgres idles far below one
  agent pod's footprint), but it joins the capacity math.
- Test speed stays SQLite-fast; backend drift becomes a CI concern — the
  dual-backend suite is the guard.
- The 07-27 class of loss (volume gone → history gone) is closed by WAL
  archiving, and the ADR 032 corpus becomes restorable.

## Non-goals

- No multi-replica planes or controllers in this ADR — Postgres removes
  the storage obstacle; the ingress/leader story is its own decision.
- No schema redesign: tables port as they are, dialect differences aside.
- No managed-cloud lock-in decision here; "a Postgres the lane can reach"
  is the requirement, its hosting is an ops choice per lane.
