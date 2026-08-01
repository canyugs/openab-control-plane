# PLAN — ADR 033 phase 2: Postgres backing store for the kernel

Spec of record. Branch `feat/adr-033-kernel-postgres`, worktree
`openab-control-plane-worktrees/adr-033-kernel`. Builds on the shipped
controller phase (#318, v0.1.49) — same ADR, same dialect mapping, same
lane infrastructure (dev marketplace `postgresql` service, CI postgres).
SQLite stays the default and fully supported; Postgres is additive.

## Survey (2026-08-01, `src/store.rs` @ f490722)

- `pub trait Store: Send + Sync` — **sync**, ~223 methods, returns
  `anyhow::Result` (backend-agnostic already; no error-type work needed).
- `SqliteStore` sole impl; 17 `dyn Store` usage sites; callers everywhere
  (api/orchestrator/coordinator/controller_api/...) — caller churn must be ZERO.
- ~20 tables in one idempotent `SCHEMA` batch + `migrate(conn)` guarded
  additive fixups (not a versioned list like the controller).
- Backend chosen by `OABCP_DB` (default `plane.db`) in `main.rs`.

## Design decisions

1. **The trait stays sync.** Making 223 methods async would touch every
   kernel module for no behavioral gain. Instead `PostgresStore` owns a
   dedicated tokio runtime thread; each sync method does
   `Handle::enter()` + `futures::executor::block_on(...)` over a deadpool
   pool (the `reqwest::blocking` pattern). Blocking the caller on a ~1ms
   lane-internal query matches today's cost, where every call already
   serializes behind `Mutex<Connection>`.
   - Do NOT use `tokio::task::block_in_place` (panics on current-thread
     runtimes — `#[tokio::test]` default) and do NOT `Handle::block_on`
     from async context (panics). `futures::executor::block_on` polling
     channel-backed tokio-postgres futures is safe from any thread; the
     dedicated runtime drives connections and timers.
2. **Backend selection**: `OABCP_DB` starting `postgres://`/`postgresql://`
   → `PostgresStore::open(url)`; else SQLite path (unchanged default).
   Factory `store::open_store(db: &str) -> Result<Arc<dyn Store>>`.
3. **Schema**: translate the `SCHEMA` batch to PG in
   `src/store/postgres.rs` (store.rs declares `mod postgres;` — file moves
   are unnecessary, the kernel file keeps trait + sqlite + tests).
   Dialect mapping proven in the controller phase:
   `INTEGER PRIMARY KEY AUTOINCREMENT`→`BIGSERIAL`, ints→`BIGINT`,
   `INSERT OR IGNORE`→`ON CONFLICT DO NOTHING`, `?N`→`$N`,
   `last_insert_rowid()`→`RETURNING id`, IMMEDIATE read-decide-write→
   `pg_advisory_xact_lock(hashtext(key))` per idempotency key, outbox
   claims→`FOR UPDATE SKIP LOCKED`. Fresh-DB-only: apply full schema +
   a `schema_migrations` table for the future; the SQLite `migrate()`
   legacy fixups do not port (they exist for pre-framework files).
4. **TLS**: reuse the controller's connector approach verbatim (rustls,
   platform roots, `sslmode=disable` opt-out) — copy
   `crates/github-pr-controller/src/store/postgres.rs::tls_connector`.
5. **Tests**: existing suite stays SQLite (fast, untouched). New
   `store::postgres::tests` gated on `TEST_POSTGRES_URL` (skip loudly),
   throwaway schema per test via `options=-c search_path=…`. Port the
   highest-value behaviors: outbox claim/lease, session lifecycle,
   controller-installation tokens/idempotency, findings append, dedupe
   keys. CI already runs a postgres:16 service.
6. **Method porting order** (each slice compiles + tests green before the
   next; trait impl can `todo!()` only while the backend is unreachable
   from `open_store` — gate merging on zero `todo!()`):
   a. bots / settings / roster
   b. sessions / session_bots / threads / messages / reactions
   c. outbox (claims!) + dispatcher state
   d. installation_tokens / controllers / controller_* (tokens, grants,
      bindings, idempotency, sessions, events, event_grants)
   e. pr_review_findings / pending_reviews / compatibility_usage
7. **Cutover** (ops repo, after code ships + dev soak of the build):
   plane is stateful over WS — window = stop plane (bots drop), import,
   start on PG, bots auto-reconnect (existing failover behavior), sweep
   webhook redeliveries. Same import tooling as the controller cutover
   (`backups/2026-08-01-dev-controller-sqlite/` generator pattern), plus
   `ops_ro`/`plane_rw` roles on the same lane instance, separate database
   `plane` (shared instance, separate DB per ADR).

## Steps

1. [x] Worktree + branch off f490722; survey
2. [x] `store::open_store` factory + `OABCP_DB` URL detection + main.rs wiring
3. [x] `src/store/postgres.rs`: runtime shim, pool, TLS, schema, migrations table
4. [x] Port slices (a)–(e) — 86/86 methods, 15 PG tests, each with PG tests for its invariants
5. [x] Dual-backend green locally (350 lib tests incl. PG; workspace 485); clippy --locked -D-grade clean
6. [ ] PR → dev council; release
7. [ ] Ops: `plane` database + roles on lane instance; cutover checklist
       (mirror `docs/adr033-prod-cutover-checklist.md` learnings); dev plane
       cutover in an idle window; soak; prod later (currently FROZEN at
       0.1.47 by operator order — coordinate)

## Non-goals

- No trait signature changes, no caller changes, no schema redesign.
- No multi-replica plane (ADR non-goal; ingress/leader is its own decision).
- Controller store: done, untouched here.
