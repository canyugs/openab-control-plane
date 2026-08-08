# ADR 039 — Bot-initiated intents and the world-controller profile

Status: proposed · 2026-08-08

Builds on: [ADR 007](007-control-plugins-and-oab-father.md) (control plugins),
[ADR 008](008-external-controller-protocol.md) (external controller protocol),
[ADR 018](018-stage3-extraction.md) (action vocabulary — **corrected here**),
[ADR 031](031-provider-neutral-kernel.md) (provider-neutral kernel, controller
owns product side effects), `docs/design.md` (mechanism / policy / substrate).

## Context

PR review is OCP's first product profile. The second is a **general agent
control center**: agents delegate work to each other, a controller tracks a
cross-session task DAG (B1 done → B2/B3/B4 unlock), any agent can query
progress, and long-lived agent presence is normal. (Exploration and the arena
MVP alternative are recorded in
`../openab-arena/docs/adr/002-agent-world-memory-mcp-waker.md`; this ADR is
the OCP-first path.)

A 2026-08-08 kernel survey found the gap smaller than assumed, and located it
precisely:

- **Every coordinator hook reacts to a done-signal or roster change.** There
  is no hook for an arbitrary bot request, and `Action`
  (`src/coordinator.rs:54-79`) is closed over a single session — it cannot
  create or reference another session. Both are correct boundaries to keep.
- **No bot-initiated signal can reach a controller.** Outbound controller
  events are session-lifecycle only (`session.opened/progress/terminal/
  timeout/superseded`, `action.failed`). A bot asking for something has no
  path out except buried in `session.progress` message text.
- **The prior art already exists end-to-end.** `[[recruit:<bot_id>]]` is a
  working bot-initiated structured request: parsed from unfenced message text
  in `on_send` (`src/orchestrator.rs:1059-1157`), authorized (chair-only),
  admitted, emitted north, with a `provision_requested` escape hatch. The
  kernel's whole structured-content idiom is text-embedded (`[done]`,
  `[[verdict:…]]`, `<!-- openab-findings -->`), no new wire types.
- **Correction to ADR 018:** its "implemented vs reserved" split is stale.
  All five `ControllerAction`s (`OpenSession`, `PostMessage`, `AddRoster`,
  `CloseSession`, `EmitStatus`) are implemented (`src/controller.rs:96-100`),
  and the external transport is live in both directions: inbound
  `POST /v1/controller/actions` with durable idempotency, outbound
  HMAC-signed webhooks with retries and dead-letter
  (`src/controller_api.rs`, `src/controller_events.rs`).
- **Sessions carry no metadata**, and `trigger_ref` is simultaneously
  identity, idempotency key, and policy marker (`src/store.rs:855-862`,
  `src/coordinator.rs:29-33`). Task identity must not be forced into it.
- **Long-lived presence already exists**: Solo sessions reopen on client
  messages (`src/coordinator.rs:445-447`), and the watchdog is anchored on
  activity, not creation (`src/main.rs:42-58`) — a chattering session never
  times out.

## Decision

### 1. Intents: bots may *request*, the plane still decides

A bot emits a structured intent as a message trailer, same idiom as recruit:

```
[[intent:delegate to=<bot_id|role> task="…" deps=<task_id,…>]]
[[intent:status task=<task_id>]]
```

Parsed in `on_send` on unfenced lines, modeled line-for-line on
`maybe_recruit`. An intent is **a request, not a command**: the mechanism
stores it, emits it, and asks policy — determinism is preserved. Authorization
is roster membership at the mechanism layer; anything finer (who may
delegate) is coordinator/controller policy.

### 2. One new controller event: `session.intent`

The only wire change. A parsed intent is enqueued as a grant-gated controller
event carrying `{session_id, bot_id, intent}` — same durable signed-webhook
path as every other event, added under the existing protocol-version
negotiation (`crates/controller-protocol`, additive). This closes the "no
bot-initiated signal reaches a controller" gap; everything downstream of an
intent is controller work.

### 3. `Coordinator::on_intent`, default empty

`on_intent(&Ctx, bot, &Intent) -> Vec<Action>` with a default `vec![]` body —
source-compatible with all four existing coordinators. It exists for
*in-session* responses only (e.g. `Relay` a question to a peer already on the
roster, per the substrate invariant: bot→bot only via coordinator-ordered
Relay). Cross-session consequences never happen here.

### 4. The DAG lives in a world controller, not the kernel

A **world controller** (external controller per ADR 008, same skeleton as
`github-pr-controller`) owns the task graph in its own store:
`tasks(id, assignee, deps, status, spec, result)`. It subscribes to
`session.terminal` + `session.intent`; scheduling is one sentence — *all deps
done → open that task's session* via the existing `OpenSession` action. One
task = one bounded solo/pipeline session; the kernel's session model, CAS
transitions, and per-task watchdog apply unchanged. Task identity rides the
controller's own store plus the `scope` string on `controller_sessions` —
the `sessions` schema is not touched.

### 5. Stranger agents enter through a sidecar, never through `ws.rs`

Opening the world to non-OAB agents (ACP speakers) is an **ACP↔gateway-wire
sidecar adapter** — a separate process holding per-bot tokens on their
behalf. The south gateway stays substrate-opinionated (design.md: being
substrate-neutral is a different project), and the open-world trust boundary
stays outside the kernel.

### 6. Shared namespace memory stays outside the kernel

Knowledge/memory is a separate MCP service (stateless Streamable HTTP,
MCP 2026-07-28; progress handles via the official Tasks extension). The
kernel's only future involvement is identity/ACL, and only when a second
consumer demands it — not now.

## Invariants preserved (the non-goals)

- No cross-session `Action` variant; `Action` stays closed over one session.
- No `sessions.metadata` column; the frozen read surface stays frozen.
- No mechanism-side bot→bot fanout; `Relay` remains the only peer channel.
- The plane still never calls an LLM and never performs product side effects.

## Phasing

1. **Kernel (small, one PR each):** intent parser + `session.intent` event;
   then `on_intent` hook when the first in-session policy needs it.
2. **World controller (new crate, the real work):** task store, DAG
   scheduler, `session.intent`/`session.terminal` consumers, progress
   answers via `PostMessage`.
3. **Memory MCP service** (independent; can proceed in parallel).
4. **ACP sidecar adapter** (last — it imports the open-world trust problem,
   built from openab-arena's ACP client code).

## Open questions

- Intent grammar: key=value trailer (recruit-style) vs a JSON body in an
  HTML comment (findings-style)? Trailer first; revisit when an intent needs
  nesting.
- Rate/abuse control on intents — per-bot budget at the mechanism layer, or
  purely controller policy?
- Does `delegate to=<role>` (plane picks the bot, like `find_spare`) belong
  in v1, or only `to=<bot_id>`?
- Progress surface for bots: answered in-session via `PostMessage`, or do
  bots also get read access to the memory service's task table?
