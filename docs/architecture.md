# Architecture

## North / Core / South

The control plane uses a **directional data-flow model** borrowed from SDN
(Software-Defined Networking): a controller sits in the middle, with a
northbound interface for management and a southbound interface for the devices
it orchestrates.

```
 North (clients)                South (agents)
 ───────────────                ─────────────────
 REST API + SSE                 Gateway WebSocket

 who triggers                   who does the work
 and consumes                   and reports back
        │                              │
        ▼                              ▼
 ┌─────────────────────────────────────────┐
 │                  Core                    │
 │  identity · sessions · routing           │
 │  orchestration · store · output          │
 └─────────────────────────────────────────┘
```

| Layer | Faces | Protocol | Examples |
|-------|-------|----------|----------|
| **North** | Users, CI, webhooks, dashboards | REST + SSE (Bearer auth) | `POST /v1/sessions`, GitHub webhook, web UI, Slack-as-client |
| **Core** | Internal | — | Session state machine, quorum, fanout, roster isolation, durable delivery |
| **South** | Agent pods | Gateway WebSocket (per-bot token) | Stock OpenAB pods (Claude, Codex, Gemini, any ACP backend) |

## Why this works for extension

The two edges don't know about each other:

- **Add a north client** (webhook, dashboard, CLI) → south unchanged, bots
  don't know who triggered the session.
- **Add a south agent** (new LLM backend, MCP agent) → north unchanged, users
  don't know which agent is behind the bot.
- **Add a core capability** (new panel type, output adapter, quorum rule) →
  both interfaces stay stable.

Design doc calls this "stable middle, replaceable edges."

## Source layout

```
src/
├── main.rs           entry point, seed_roster, serve
├── api.rs            north: REST + SSE endpoints
├── ws.rs             south: gateway /ws server
├── orchestrator.rs   core: lifecycle, fanout, quorum, close
├── session.rs        core: state machine (open → deliberating → quorum → closed)
├── routing.rs        core: who receives what
├── state.rs          core: AppState, connection registry
├── protocol.rs       south: gateway wire types (GatewayEvent/Reply/Response)
├── store.rs          core: SQLite persistence (bots, sessions, roster, outbox)
├── output.rs         core: verdict side-effects (GitHub PR)
├── identity.rs       core: per-bot token hashing
└── north/ south/     reserved for when files outgrow flat layout
```

## Data flow (PR review example)

```
1. North: client POST /v1/sessions + /messages (with PR diff)
2. Core:  orchestrator fans out trigger to rostered bots
3. South: bots receive GatewayEvent, review the diff, reply + react 🆗
4. Core:  quorum detected → prompt chair to synthesize
5. South: chair posts verdict, reacts 🆗
6. Core:  session closed, verdict emitted via SSE
7. North: client receives verdict; chair gh-comments on the PR
```
