# ADR 029 — Bot-health arbitration: sequenced evidence, suppression, and the external reporter surface

Status: proposed · 2026-07-16

## Context

ADR 023 established connected≠healthy and shipped the passive half: three
consecutive agent-error frames flip `bots.health` to `degraded`
(`record_bot_frame`), the liveness sweep marks transport-dead bots on an
**active session roster** `unreachable` (idle bots are Phase 2's still-open
territory), and failover routes around both. Living with it since has
exposed four structural gaps — three visible in the code, one about to be
created by ADR 027's external quota watcher:

1. **Recovery is a single observation.** A bot that bursts three error
   frames and then emits one clean frame is instantly `health="ok"` —
   immediately eligible again as a failover standby (`pick_healthy_standby`,
   `find_spare`), and, where `OABCP_AUTO_FAILOVER=1`, swappable back into
   a roster on the next degrade event elsewhere. There is no confirmation
   count and no cooldown: one packet re-arms a flaky bot, and paired
   flapping bots can ping-pong a roster. (The degrade edge itself is
   bounded and flag-gated; the unguarded edge is *recovery*.)
2. **Alarms are one-shot on the edge.** The passive `degraded` transition
   emits a single WARN and — unlike the sweep's `unreachable` — no north
   `bot_health` event at all. The 2026-07-13 incident (both lanes' chair
   dead-while-connected for ~a day, found by a manual smoke) predates the
   detector, but its lesson is forward-looking: a *persisting* condition
   must keep announcing itself. An edge-triggered one-shot reproduces
   that silence whenever the single edge is missed — log rotation, a
   pager gap, or an operator who wasn't looking at that minute.
3. **Health writes are last-writer-wins with no freshness guard.**
   `PATCH /v1/bots/:id` accepts any `is_safe_token`-shaped `health`
   string (format-checked, never semantics- or freshness-checked)
   straight through `update_bot_metadata`. Nothing distinguishes a fresh
   plane-observed degrade from a stale external report arriving late; the
   write path cannot drop a duplicate or out-of-order report. The moment
   ADR 027's quota watcher starts reporting (its Decision 2 points it at
   exactly this surface), a delayed or replayed report can silently
   clobber truth in either direction.
4. **Nothing anchors "this evidence belongs to that dial."** The
   connection hub already mints a per-dial generation (`conn_seq`, the
   C7/#94 zombie fix) — monotonic **within one plane process** — but it
   is never persisted and never attached to frames. A degrade recorded
   against a dead connection's lifetime is indistinguishable from one
   recorded against the current dial, so no recovery rule can say "prove
   you are a new incarnation" — herdr's term; here: **a new connection
   generation** — which is the rule the quota-death failure mode needs.

Prior art: herdr (agent multiplexer; analysis in the sdd-controller repo,
`docs/prior-art-herdr.md`) solved the rotated problem with an evidence
lattice — per-source monotonic sequences with stale reports dropped,
negative observed evidence outranking weak positive self-report,
suppression keyed to a session ref until a different incarnation presents
itself, and asymmetric fast-alarm/slow-calm hysteresis with
level-triggered re-emission.

## Decision

Health stops being one mutable string and becomes **the arbitrated fold
of per-source, sequenced, expirable reports**. Six parts.

### 1. Per-source health reports (new table, additive)

`bot_health_reports`: one row per (bot_id, source), upserted —
`{bot_id, source, seq, status, reason, at_ms, ttl_ms, conn_gen}`.

- `source` ∈ `observed` (the plane's own frame accounting), `transport`
  (sweep + reconnect flip — exactly those two writers; the sweep covers
  only bots on active session rosters, so the no-TTL choice below is a
  deliberate reliance on the reconnect flip between sessions, not an
  assumption of full sweep coverage), `operator` (manual PATCH), and
  `external:<name>` (e.g. `external:quota-watch`).
- **Credential → writable sources, bound explicitly:** the optional
  scoped `OABCP_HEALTH_REPORT_TOKEN` (copying the discovery-token
  pattern) may write `external:*` ONLY; the north bearer may write
  `operator` and `external:*`; `observed`/`transport` are plane-internal
  and never writable via the API.
- **One live writer per source name** is an operating invariant;
  overlapping reporters (blue-green watcher cutover, doubled cron) must
  use instance-suffixed names (`external:quota-watch-blue`) — each is
  then an independent lane with its own TTL, and the fold takes the
  worst.
- `seq` is per (bot, source) monotonic; a report with `seq` ≤ the stored
  one is dropped as a success (`200 {"stale": true, "stored_seq": N}` —
  idempotent replay is not an error, and the echoed `stored_seq` lets a
  restarted reporter detect that its counter is behind and jump past it).
  **External sources MUST derive `seq` from wall-clock unix-ms** (report
  creation time) so a reporter restart cannot silently self-mute for an
  unbounded period. Operator writes get a server-side
  `seq = max(now_ms, stored_seq + 1)` so a same-millisecond correction
  is never dropped.
- `status` ∈ `ok | degraded | unreachable` (unchanged vocabulary) plus
  free-text `reason`. The plane does NOT classify provider errors
  (quota-vs-auth stays with reporters who actually know, per ADR 027).
- `ttl_ms` is **mandatory for `external:*`**: an expired report
  arbitrates as absent, and expiry of a non-ok report emits a
  `bot_health` event — an external watcher going quiet must surface,
  not silently pin its last claim. `observed`/`transport`/`operator`
  reports do not expire; they are superseded by their own lane's next
  report (or retired, below).
- **Expiry driver, named:** the existing liveness sweep additionally
  retires expired report rows, recomputes the fold, and emits the expiry
  event on its normal interval — independent of the re-emission knob in
  §5. Read paths (`GET /v1/bots`, arbitration) also filter expired rows
  at read time, so a lagging sweep can delay the event but never the
  arbitration.

### 2. Arbitration: effective health = worst live report

`bots.health` becomes a **derived cache**: worst status among non-expired
reports (`unreachable` > `degraded` > `ok`), recomputed on every accepted
report, retirement, and expiry. Sources own their lanes — an external
`ok` clears only that source's earlier claim, never the plane's observed
degrade — with exactly two cross-lane effects:

- **Operator override (the escape hatch, made explicit):** an accepted
  `operator` report with `status=ok` **retires every source's non-ok
  rows except `transport`** in the same write — transport reflects
  live socket state the hub can verify directly; an operator cannot
  declare a disconnected bot reachable, and for an idle bot nothing
  would re-assert the retired row until the next active-roster sweep.
  (Reconnect clears the transport lane naturally.) The operator is the top authority; today's
  unpin (`PATCH health=ok`) keeps working and is now replay-safe.
  An operator non-ok pins the bot until the next operator report —
  stickier than today's one-frame clear, which is the point; noted here
  because it IS a behavior change to the manual path.
- **New-generation retirement (the bench-deadlock exit):** a reconnect
  that mints a new connection generation retires an `observed` non-ok
  row recorded under an older generation (see §3). Rationale: a benched
  bot receives no prompts, hence no frames, so a frame-gated recovery
  alone would deadlock — degraded forever, unfixable except by operator.
  A fresh dial is real evidence (re-auth, pod redeploy — the actual
  remedies produce one); the cost of misplaced optimism is bounded by
  the degrade threshold (three error frames re-bench it).

`GET /v1/bots` keeps the `health` string (unchanged consumers: failover,
roster selection, stats) and gains `health_reports` — the live per-source
rows — so an operator sees *why* in one read.

**Failover trigger, unified:** any transition of the arbitrated fold
from ok to non-ok fires the same WARN + `attempt_failover` path as
today's observed degrade edge — regardless of which source moved it,
external and operator included. This delivers ADR 027 Decision 2's
promise that external reports get "the same WARN + Phase 4 failover
machinery", still bounded and flag-gated exactly as today.

### 3. Asymmetric hysteresis + generation suppression (observed lane)

- Degrade: unchanged (3 consecutive error frames; threshold env-tunable).
- The bot row gains a persisted **`conn_gen`**, stamped at
  `register_conn`. Generations must survive restarts: minted as
  `max(now_ms, previous + 1)` (wall-clock with a strict-increase guard),
  seeded at boot from the persisted maximum — the in-process
  `AtomicU64`-from-zero counter is NOT restart-safe and cannot be used
  as-is. Health-accounted frames are attributed to **the generation of
  the connection that delivered them** (threaded through `handle_reply`),
  not to whatever the bot row says at write time — a zombie connection's
  late frames must not masquerade as current-generation evidence.
- Recover (observed lane) requires either **N consecutive clean settled
  frames (default 2) AND ≥ a cooldown (default 30 s) since the last
  error frame**, or **retirement by a newer generation** (§2). One clean
  frame on the same dial that produced three errors no longer flips
  health.
- `unreachable` recovery on reconnect is already generation-anchored by
  construction — unchanged.

### 4. What this amends

This section exists because 029 touches standing decisions:

- **ADR 023 Decision 2 (log-based, one-shot alerting) is amended**: the
  passive degrade path gains the north `bot_health` event the sweep
  already emits, and non-ok conditions re-emit (§5). The
  no-new-notification-system constraint survives — everything stays
  within the existing WARN-log and north-event channels.
- **ADR 027 Decision 2's endpoint deferral ends here**: the watcher now
  has a concrete need, and §1/§6 define the surface it gets.
- ADR 023 Phases 2 (active probing) and 3 (bot-side self-heal) remain
  open and unowned by this ADR.

### 5. Level-triggered alarms

While any live report for a bot is non-ok: re-WARN and re-emit the north
`bot_health` event every `OABCP_HEALTH_REEMIT_SECS` (default 300; 0
disables). **One event per bot per tick**, envelope = the folded health
plus the array of live non-ok reports (`source, status, reason, at_ms`)
plus `since_ms` = the earliest live non-ok `at_ms` — so consumers can
tell a persisting condition from a new one; the expiry event (§1)
carries the same envelope. Default-ON is a deliberate divergence from
the siblings' ship-dark convention (ADR 025): re-emission has no
external side effects — no GitHub posts, no roster mutation — it is
pure log + north-event traffic, and shipping THIS dark would re-create
the silence it exists to kill.

### 6. The external reporter surface (ADR 027's socket)

`PATCH /v1/bots/:id` keeps working for operators — a bare `health`
string maps to `source=operator` with the server-side seq of §1 (and
the override semantics of §2). Structured reports use the same route
with `{source, seq, status, reason, ttl_ms}`; validation: source
permission per credential (§1), mandatory TTL for `external:*` bounded
to [30 s, 24 h], `reason` capped (1 KB, control characters stripped),
`source` an `is_safe_token`-shaped name ≤ 64 chars, `status` strictly
the three-value enum, seq monotonicity. The quota watcher (ADR 027) thus gets an idempotent,
replay-tolerant, least-privilege write path — buildable without
touching the plane again.

### Non-goals

- Active probing (ADR 023 Phase 2) — orthogonal, still open.
- Provider-error classification in the plane — reporters classify.
- Auto-remediation beyond existing failover (ADR 023 D4) — unchanged.
- Rewriting failover/roster logic — it keeps reading `health == "ok"`;
  only the quality of that string improves.

## Consequences

- The 07-13 failure shape becomes structurally loud: a persisting
  degrade re-announces every 5 minutes on two channels, and an external
  watcher's silence expires visibly instead of pinning stale claims.
- Roster flap is bounded on both edges: threshold in, confirmation +
  cooldown (or a newer generation) out. The bench deadlock has two
  designed exits (operator override, new-generation retirement).
- Two additive schema changes (`bot_health_reports`, `bots.conn_gen`);
  `bots.health` semantics preserved for every existing reader.
- Operator debugging: `health_reports` on `GET /v1/bots` shows which
  source claims what, with age — no archaeology through WARN logs.
- Behavior changes to note in release: operator-set non-ok is sticky
  until the next operator write; recovery needs two clean frames or a
  fresh dial; degraded bots re-announce by default.
- Costs: one indexed row upsert + fold recompute per health-relevant
  frame; expiry piggybacks on the existing sweep; re-emission timer is
  bounded by bot count (single digits per lane).

## References

- ADR 023 (liveness parent; D2 amended here, Phases 2/3 stay open),
  ADR 024 (slot-bound chair write scope — capability follows the active
  slot; a cousin of anchoring evidence to the current generation),
  ADR 025 (requester-facing notice; the ship-dark precedent §5
  deliberately diverges from), ADR 027 (external watcher boundary;
  Decision 2's deferred endpoint lands here)
- herdr prior-art analysis (sdd-controller `docs/prior-art-herdr.md`):
  evidence lattice, seq+suppression, asymmetric hysteresis,
  level-triggered alarms
- Incident 2026-07-13 (shared KIRO key quota exhaustion; both lanes'
  chair -32603 while connected; found by manual smoke ~a day later —
  the detector ADR 023 then shipped is edge-triggered, hence §5)
- Code anchors: `record_bot_frame` (src/store.rs), `account_bot_health`
  / `attempt_failover` / `sweep_liveness` (src/orchestrator.rs),
  conn-generation hub (src/state.rs), `patch_bot` (src/api.rs),
  `check_discovery_auth` (src/api.rs — the scoped-token pattern)
