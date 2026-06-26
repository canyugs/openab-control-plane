# Design

OCP sits between clients (north) and OpenAB pods (south). The boundaries are
deliberate — anything that OpenAB or the agent CLI already handles stays there.
"Pipe, not container" — the plane coordinates but never reasons.

## Design discipline

The scope tables below are maintained by deletion, in this order (Musk's
"algorithm", applied to scope):

1. **Question every requirement** — each feature names *who* needs it and *why*.
   A requirement that can't name an owner is the first to go.
2. **Delete what another layer owns** — if OpenAB, the agent CLI, or the bot pod
   already does it, it leaves OCP. The "does NOT own" table is this step's output.
   Deleted too much? Add it back — but expect to add back less than you cut.
3. **Simplify what survives** — only after deletion. Never polish a feature that
   shouldn't exist (we don't `[agent.steering]`-inject, don't serve `CLAUDE.md`).
4. **Then accelerate, then automate** — auto-trigger and structured output come
   *after* the manual flow is proven correct, never before (see ROADMAP phases).

The bias is toward a smaller plane. A boundary that feels too aggressive and gets
walked back (e.g. act-as-user, re-scoped not deleted) means it was drawn tight
enough to test.

## OCP owns

| Concern | Why OCP |
|---------|---------|
| Session lifecycle (open → quorum → closed) | Multi-agent coordination doesn't exist in OAB |
| Roster, fanout, isolation | OAB is single-bot; multi-bot routing is the plane's job |
| Quorum detection + chair prompting | Deterministic orchestration, not LLM reasoning — the plane never calls an LLM |
| Durable delivery (outbox, replay) | Cross-bot message reliability |
| Bot identity + per-bot tokens | The plane manages the registry; credentials stay in each pod (`inherit_env`) |
| `/bot-config/:id` — OAB config.toml assembly | Bots fetch gateway/agent/pool config; store is a trait seam (SQLite default, Postgres/libSQL swap) |
| North API (sessions, messages, SSE) | The client-facing interface |

## OCP does NOT own

| Concern | Who owns it | Why |
|---------|-------------|-----|
| Agent steering (`CLAUDE.md`, `AGENTS.md`, `.kiro/steering/`) | Bot deployer via OAB `pre_seed` / `pre_boot` | Agent-agnostic — any CLI OAB supports works without plane changes |
| LLM reasoning / verdict content | The agent (chair bot) | Plane never calls an LLM |
| Agent credentials (`CLAUDE_CODE_OAUTH_TOKEN`, API keys) | Each bot pod via `inherit_env` | Plane never touches model keys |
| PR-specific logic (gh pr diff, gh pr comment, label) | Application shim or chair bot | Code review is an app on top of OCP, not part of it |
| Agent lifecycle (spawn, pool, session TTL) | OpenAB (`[agent]` + `[pool]` config) | OAB's existing session pool management |
| Platform adapters (Discord, Slack, Telegram) | OpenAB gateway | OCP speaks the gateway wire protocol, not platform APIs |
| File/knowledge seeding (S3, git clone) | OAB `pre_seed` / `pre_boot` hooks | Boot-time setup is the bot image's responsibility |