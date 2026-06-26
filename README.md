# openab-control-plane

A gateway-native **conversation control plane** for OpenAB councils: your client
→ this plane → OpenAB/Gateway → agent. No chat platform required.

It owns what OpenAB deliberately doesn't (it's "a pipe, not a container"):
identity/trust, the conversation/session model, routing/fanout, quorum, and
verdict side-effects. Bots are stock OpenAB pods speaking the gateway protocol.

## Docs

- [Roadmap](ROADMAP.md) — phased plan, done log, known issues
- [Design](docs/design.md) — scope (what OCP owns vs doesn't)
- [Architecture](docs/architecture.md) — north/core/south model, source layout
- [Configuration](docs/config-reference.md) — all env vars and seed format
- [PR Review Flow](docs/flow.md) — end-to-end council flow for PR review
- [Deploy](docs/deploy.md) — Zeabur template one-click deploy
- [PR Review Format](docs/steering/pr-review.md) — reviewer + chair output format

## Layout

| File | Role |
|---|---|
| `src/protocol.rs` | gateway wire types (`GatewayEvent`/`Reply`/`Response`) |
| `src/store.rs` | SQLite model + domain types |
| `src/identity.rs` | per-bot token issue/verify |
| `src/routing.rs` | fanout (roster minus author) |
| `src/session.rs` | quorum rule |
| `src/orchestrator.rs` | lifecycle: convene → quorum → verdict |
| `src/ws.rs` | south: gateway `/ws` server |
| `src/api.rs` | north: REST + SSE |

## Run

```sh
cargo run                 # listens on 0.0.0.0:8090, db=plane.db
```

Env: `OABCP_ADDR`, `OABCP_DB`, `OABCP_API_KEY` (bearer key for the north API;
unset = open), `GH_OUTPUT=1` (actually post verdicts via `gh`).

## North API (clients)

```
POST /v1/bots                      {name, role}                  -> {bot_id, token}
POST /v1/sessions                  {title, roster, quorum_n, chair_bot, trigger_ref} -> {session_id}
POST /v1/sessions/:id/messages     {content}                     -> {message_id}
GET  /v1/sessions/:id                                            -> {session, messages}
GET  /v1/sessions/:id/stream       (SSE: message|reaction|state|verdict)
```

## South (a stock OpenAB bot)

Register a bot to get its token, then point an OpenAB pod's `[gateway]` at the
plane — no proxy patch, stock image. See `config.toml.example`.

## Test

```sh
cargo test                # 11 unit + 3 integration (1/3/5-bot parity)
```

`tests/spike.rs` drives mock bots over the real gateway wire protocol: thread
creation, reactions, streaming edits, and full 3-/5-bot councils to a closed
verdict with one-thread-per-session convergence.
