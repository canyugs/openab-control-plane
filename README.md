# openab-control-plane

A gateway-native **conversation control plane** for OpenAB councils: your client
→ this plane → OpenAB/Gateway → agent. No chat platform required.

It owns what OpenAB deliberately doesn't (it's "a pipe, not a container"):
identity/trust, the conversation/session model, routing/fanout, quorum, and
verdict side-effects. Bots are stock OpenAB pods speaking the gateway protocol.

## Quick Start

Stand up a working PR-review council and get your first verdict in a few minutes.

**1. Deploy** — one control-plane + 3 stock OpenAB Claude pods (1 chair + 2 reviewers).
First gather: a Claude token from `claude setup-token`, a Zeabur project id (from the
dashboard URL or `zeabur project list`), and — optionally — a fine-grained GitHub PAT
with `pull_requests: write` + `contents: read` so the chair can post verdicts:

```sh
npx zeabur@latest template deploy -f zeabur-template.yaml \
  --project-id <PROJECT_ID> \
  --var PUBLIC_DOMAIN=my-council \
  --var CLAUDE_CODE_OAUTH_TOKEN=<OAUTH_TOKEN> \
  --var GH_TOKEN=<PAT>
```

Omit `GH_TOKEN` to run the council without PR write-back — it still deliberates and
produces a verdict, but won't post it to GitHub. The plane comes up at
`https://my-council.zeabur.app`; Zeabur exposes its auto-generated API key as the
`PASSWORD` env var on the control-plane service (referenced by `OABCP_API_KEY`) —
copy it from the dashboard's **Variables** tab.

**2a. Review a PR on demand** (needs `node`):

```sh
PLANE=https://my-council.zeabur.app KEY=<OABCP_API_KEY> \
  scripts/open-council.sh owner/repo#123 --watch
```

The chair posts a single verdict comment on the PR; `--watch` streams session progress and prints the verdict when the session closes.

**2b. Auto-review every PR** (CodeRabbit-style) — set up the **GitHub App + webhook**:
put `GITHUB_APP_*` + `GITHUB_WEBHOOK_SECRET` on the plane and point the App's webhook at
`POST <plane>/api/v1/github_webhooks`. A PR opened / reopened / ready-for-review, or a
`/review` comment, then **convenes a real council automatically** (no per-repo workflow
to copy) and the chair posts one verdict comment back. Full guide:
[deploy.md](docs/deploy.md) · [github-app-validation.md](docs/github-app-validation.md).

> `.github/workflows/council-review.yml` is a **manual fallback** now
> (`workflow_dispatch` only) — the automatic `pull_request` trigger moved to the webhook
> to avoid double-convening. Use it, or `open-council.sh`, to re-review on demand.

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

## Council size (how many OAB pods?)

The plane is **not** an OAB pod — it's a separate service. Each OAB pod = one bot
= one roster seat, with a role (`chair` or `reviewer`). Who exists is seeded at
boot from `OABCP_BOTS` (`name:role,…`); the template default is
`chair:chair,rev1:reviewer,rev2:reviewer` → a one-click deploy is **1 control-plane
+ 3 OAB pods**.

A council = **1 chair + (N−1) reviewers**. `open-council.sh` sets
`quorum_n = max(0, len(roster) − 1)` (**all** reviewers must signal), `chair_bot =
roster[0]`, and `mode = solo` for a 1-entry roster else `council`. It closes when
every reviewer sends `[done]` *or* the chair sends `[done]` (chair is the closing
authority).

| OAB pods | Result |
|---|---|
| 1 | `solo` — no reviewers, not a real council |
| 2 | minimum council: chair + 1 reviewer, `quorum_n=1` |
| **3** | **shipped default / standing-council sweet spot**: chair + rev1 + rev2, `quorum_n=2` |
| 5 | proven in demo (chair + rev1..rev4) |

Proven live at 1 / 3 / 5 bots. **Sizing is manual today** — you pick N by how many
pods you deploy (`OABCP_BOTS`) + which names go in the session roster. Preset-driven
rosters (quick=2 / standard=3 / full=5) and angle assignment are Phase 2 (TODO, see
[Roadmap](ROADMAP.md)). To resize: edit `OABCP_BOTS` + add/remove pod services and
redeploy, or `POST /v1/sessions/:id/roster` to add a bot mid-session (removal/leave
is not built yet).

## South (a stock OpenAB bot)

Register a bot to get its token, then point an OpenAB pod's `[gateway]` at the
plane — no proxy patch, stock image. See `config.toml.example`.

## Test

```sh
cargo test                # 43 unit + 10 integration (1/3/5-bot parity, close-path regressions)
```

`tests/spike.rs` drives mock bots over the real gateway wire protocol: thread
creation, reactions, streaming edits, and full 3-/5-bot councils to a closed
verdict with one-thread-per-session convergence.
