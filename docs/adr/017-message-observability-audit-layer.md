# ADR 017 — Message observability / audit layer

Status: proposed · 2026-07-05

## Context

OAB routes all message traffic through Discord. The full record of what was
sent and received lives only in Discord, so any query — support triage, an
audit, "what did the council actually decide on session X" — has to go back
through a **Discord bot token** against the Discord API. There is no
platform-agnostic, queryable history of the conversation, and none at all of
the plane's own decision trail.

**Goal:** a platform-independent message record / observation layer with
observability, audit, and query capability:

- **Coverage:** at minimum inbound/outbound message content; ideally also the
  agent's tool calls, decision path, and errors.
- **Platform-independent:** not bound to Discord or any single platform;
  querying must not require a Discord bot token.
- **API-first:** get the data model and query API right; frontend/dashboard is
  out of scope for now.
- **Time range:** record from adoption onward only. No backfill of existing
  Discord history.

### What OCP already provides (source-verified 2026-07-05, `origin/main` @ `b9a1671`)

- **Generic, platform-agnostic ingest already exists.** `POST /v1/sessions`
  (`open_session`, `api.rs:382`) and `POST /v1/sessions/:id/messages`
  (`post_message`, `api.rs:618`) take only title / trigger_ref / roster / mode /
  content — nothing GitHub-specific. `github_webhook.rs` is *just one adapter*:
  `verify_signature` → `parse_trigger` → `council::convene_for_pr` →
  `store.create_session`. A Discord/OAB adapter is the symmetric case — parse a
  platform event, call the same core (`create_session` /
  `orchestrator::post_client_message`, `api.rs:625`). **Feeding messages in is
  an adapter problem, not a core rewrite.**
- **Message storage is already persistent and platform-agnostic.** The
  `messages` table (`store.rs:354`) holds `author_kind`
  (`"bot"｜"client"｜"system"`), `author_id`, `content`, `reply_to`,
  `created_at`, `thread_id` — inbound (client) and outbound (bot) content both
  fit today; plus `reactions` and a durable `outbox`. `session-log` is rebuilt
  from `messages` via `render_session_log` (`api.rs:472`) over the north API — it
  **never touches a Discord token**. The message-audit half of the goal is
  substantially already met.
- **⚠️ The real audit gap: lifecycle / decision events are not persisted.**
  `emit_north` (`state.rs:219`) builds a fully structured event
  (`{type, session_id, payload, ts}`) and only calls `north_tx.send()` — a
  broadcast to live SSE subscribers, with **no write to any table**. There are
  **24 call sites** (state transitions, `roster_add`, `council_roster`, quorum,
  `thread`, timeout/provision, and the #98 WS connection lifecycle). Every one of
  these decision-trail events is lost the moment no one is subscribed.
- **⚠️ tool-call trace crosses the plane/pod boundary.** The plane only sees
  bot-produced messages and reactions. The agent's internal tool calls (a bot
  self-fetching a PR and producing findings inside its own pod) are invisible to
  the plane. "Record agent tool calls / decisions as completely as possible" is
  **not** solved by adding one plane-side table — it needs the bot/pod to report
  actively. This is the one place complexity explodes.
- **Existing observability is point-in-time, not historical.** `/v1/stats`
  (C6, #97, `api.rs`) returns a live snapshot (bot connection/health counts) —
  useful, but **not** an event log. It confirms the shape of the gap: OCP has
  live-state observability and no historical audit trail.

### Why not a greenfield design

Nothing in the goal requires a rebuild. OCP already has the three hard parts —
platform-agnostic **persistent message storage**, a token-free **query API**,
and **generic ingest**. A greenfield system would re-implement exactly those and
still face the same tool-trace boundary. The **only** honest greenfield
advantage is at-scale storage: OCP's store is **SQLite on the `/data` PVC**
(#96, C9). If audit later needs high write throughput, long retention, or
analytical queries, SQLite becomes the ceiling and shipping events to an
external append/columnar sink genuinely differs. For the stated scope
(record-from-adoption, message + lifecycle, API-first) SQLite is sufficient —
**this is a future knee, not a now-blocker.** Extending OCP is the right path.

## Decision

Extend OCP. Close the audit gap and platform-independence goal with three core
pieces plus two the original scoping missed, and hold tool-trace at a
deliberately coarse altitude.

1. **(a) Discord/OAB ingest adapter** — a new `src/discord_webhook.rs`,
   symmetric to `github_webhook.rs`: verify → parse → `create_session` /
   `orchestrator::post_client_message`. Wire its route in `api.rs`. *Low
   complexity, high fit.*
2. **(b) Persistent `events` table + persist `emit_north`** — add
   `CREATE TABLE events` and an insert in `store.rs`; have `emit_north`
   (`state.rs:219`) write the event it already constructs before/after the
   broadcast. Because the event is already structured JSON, this is ~one insert.
   *Low complexity, high fit.*
3. **(c) tool-call trace — bounded** — do **not** build a pod→plane distributed
   tracing pipeline. Have the bot emit a **coarse per-turn activity/summary**
   north event (that a tool round happened + outcome/error), persisted by (b)'s
   same path. Full tool arg/response payloads stay in the pod's own logs, not the
   plane's audit table.
4. **(d) Events query API** *(gap the original scoping missed)* — a read
   endpoint, e.g. `GET /v1/events?session_id=&kind=&since=`, plus a `store` query
   fn. `session-log` rebuilds **messages** only; without this, audit data is
   written but not retrievable — which defeats the "API-first, queryable" goal.
5. **(e) Ingest adapter auth parity** *(gap the original scoping missed)* —
   `github_webhook` gates on HMAC `verify_signature` + repo allowlist +
   `author_association`. The Discord/OAB adapter needs an equivalent
   (signature/verification, token-independent inbound auth) or it becomes an
   unauthenticated ingest hole.

## Scope Rules

- **Do** persist `emit_north` into a new `events` table and add the events query
  API — (b)+(d) are the core of "auditable + queryable".
- **Do** add the Discord/OAB adapter symmetric to `github_webhook`, with auth
  parity — (a)+(e).
- **Do** keep tool-trace at the coarse "a tool round happened + outcome/error"
  altitude, reusing the persisted-event channel — no new transport, no new table.
- **Do not** build a general pod→plane distributed-tracing system, capture full
  tool arg/response payloads on the plane, or add a dashboard/frontend — all are
  out of scope and the payload path is where complexity and PII/secret surface
  explode.
- **Do not** move OCP off SQLite for this work. Revisit only if a concrete
  driver hits the write-throughput / retention / analytical ceiling.
- **Do not** backfill Discord history — record from adoption onward only.

## Consequences

### Positive

- The plane's decision trail (state transitions, quorum, roster changes, WS
  lifecycle, timeouts) becomes durable and queryable over the north API — no
  Discord token, no SSE subscriber required at the moment of the event.
- The Discord/OAB adapter reuses the exact core the GitHub path already uses;
  no message-storage rewrite.
- Message-audit is already substantially met by the existing `messages` table +
  `session-log`; this ADR mostly closes the *lifecycle-event* half.

### Negative

- The `events` table is append-only and grows unbounded; a retention/prune
  policy is deferred (low priority given the record-from-adoption scope) but must
  be named, not silently ignored.
- Coarse tool-trace means deep tool-call payloads are **not** in the plane audit
  — a specific deep debug still requires the pod's own logs. This is the
  deliberate trade against over-engineering (c).
- SQLite remains the store; the scale ceiling above is accepted, not solved.

### Neutral

- The generic `/v1/sessions` ingest and `messages` schema are unchanged; only a
  new adapter, a new table + its query endpoint, and a persistence line in
  `emit_north` are added.
- `/v1/stats` (C6) stays as the live-state view; the events API is the
  complementary historical view.

## Open Questions

1. **(c) tool-trace altitude** — is "a tool round happened + outcome/error"
   enough, or does any concrete audit need per-tool identity (name only, no
   payload)? Name-only is still cheap; payloads are the line not to cross.
2. **(e) Discord adapter auth** — Discord interaction signature (Ed25519) vs a
   shared HMAC on an OAB-side relay. Depends on whether OAB posts to the plane
   directly or via a relay.
3. **Retention** — is any prune needed within the initial horizon, or defer to a
   later ADR once volume is observed?

## References

- [ADR 001 — Three planes](001-three-planes.md)
- [ADR 008 — External controller protocol](008-external-controller-protocol.md)
- [ADR 013 — Decision → review-state](013-decision-review-state.md) (the
  `verdict` north event this layer would persist)
- Source: `src/api.rs` (`open_session`, `post_message`, `render_session_log`,
  `/v1/stats`), `src/state.rs:219` (`emit_north`), `src/store.rs:354`
  (`messages`), `src/github_webhook.rs` (adapter shape to mirror)
