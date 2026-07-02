# OpenAB Control Plane (OCP) — Agent Notes

## Project Snapshot
- Rust service (axum 0.7 + tokio + rusqlite/SQLite), single binary `openab-control-plane`.
- Headless REST + SSE + WebSocket + GitHub webhook server — no web UI, no LLM.
- Purpose: gateway-native runtime coordinating multiple stock OpenAB pods
  (external repo: `../openab`). PR review councils are the first product profile.
- OCP masquerades as an OpenAB gateway: pods dial in over WS speaking
  `openab.gateway.event/reply.v1` (mirrored in `src/protocol.rs` from openab's
  `crates/openab-gateway/src/schema.rs` — keep in sync manually).
- No Kubernetes client. Pods are Zeabur services launched with
  `openab run -c http://…/bot-config/<botid>`; deploy templates in
  `zeabur-template-*.yaml`.

## Commands
- `cargo build` / `cargo run` — build and run locally (see `docs/local-development.md`).
- `cargo test` — unit tests inline in `src/`, integration tests in `tests/`.
- `cargo clippy` / `cargo fmt` — run before claiming done.
- `scripts/dev-*.sh` — dev cluster build/deploy/tunnel/webhook helpers.
- `scripts/open-council.sh` — manually convene a council.

## Layout
- `src/api.rs` — north REST/SSE API (`/v1/bots`, `/v1/sessions`, `/v1/review`,
  `/v1/council/roster`) + `/bot-config/:id` config rendering (legacy, see ADR 010).
- `src/ws.rs`, `src/protocol.rs` — south gateway WS for pods.
- `src/orchestrator.rs` — mechanism: fanout, durable outbox, state transitions,
  done-signal detection (`[done]` / 🆗), watchdog.
- `src/coordinator.rs` — pluggable policy: `council`, `review_council`, `solo`, `pipeline`.
- `src/controller.rs` — declarative `OpenSessionAction` interpreter (ADR 008).
- `src/council.rs`, `src/github_webhook.rs` — PR review convening, presets
  (lite/quick/standard/full), `/review` + `/ask` triggers.
- `src/github_app.rs`, `src/identity.rs` — GitHub App per-role scoped tokens.
- `src/store.rs` — SQLite store behind a `Store` trait (bots, sessions, messages,
  reactions, outbox, tokens).
- `docs/` — design, roadmap, ADRs (001–011); `docs/roadmap.md` is the source of
  truth for phases and known gaps.
- `skills/pr-review/` — steering docs served to reviewer bots.

## Conventions
- English for all code/comments/commits; short imperative commit subjects.
- Session state machine `Open → Deliberating → Quorum → Closed/Aborted` uses
  CAS-guarded transitions — never mutate state outside the store helpers.
- `trigger_ref` is the idempotency key for sessions (e.g. `github:pr/owner/repo#123`).
- Config via `OABCP_*` env vars; see `config.toml.example` and `docs/config-reference.md`.
- Auth: single bearer `OABCP_API_KEY` (north), per-bot tokens (south),
  HMAC (webhooks). Multi-tenancy does not exist yet (roadmap Phase 4).

## Gotchas
- Wire `platform` is hardcoded `"feishu"` to unlock streaming-edit acks in pods.
- `/bot-config` serves plaintext bot tokens — known spike-era gap, not production-safe.
- GitHub App tokens: minted against a single installation only
  (`github_webhook.rs` known gap); stored plaintext in SQLite; revoke does not
  call GitHub's API.
- Upstream OpenAB is moving to deny-by-default L3 identity trust (trust pyramid
  Phase 3); rendered bot configs must keep `trusted_bot_ids` / bot-message
  settings correct or councils silently stop working.
