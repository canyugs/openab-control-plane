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
dashboard URL or `zeabur project list`), and either a fine-grained GitHub PAT
(`pull_requests: write` + `contents: read`) or a GitHub App setup for clean bot
attribution:

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

**2b. Auto-review every PR** (CodeRabbit-style) — set `GITHUB_WEBHOOK_SECRET` on the
plane and point a webhook at `POST <plane>/api/v1/github_webhooks` (subscribe to Pull
requests + Issue comments). A PR opened / reopened / ready-for-review, or a write-ish
commenter's `/review` comment, then **convenes a real council automatically** and the
chair posts one verdict comment back. This repository's dogfood install uses this
webhook/App track only; it does not carry a repo-local council Action, so one PR event
cannot convene twice. By default the chair can post via a `GH_TOKEN` PAT; to post as a
clean App bot (`zeabur-council[bot]`, not your account) do the **pod-local App-identity
upgrade** ([deploy.md §3](docs/deploy.md)). Per-PR depth: add a
`review:lite|quick|standard|full` label. **Ask a follow-up:** a write-ish commenter can
comment `/ask <question>` (or `@`-mention the bot when `OABCP_BOT_HANDLE` is set) and a
solo session answers in the thread. Guides: [deploy.md](docs/deploy.md) ·
[github-app-validation.md](docs/github-app-validation.md).

## Docs

- [Roadmap](ROADMAP.md) — phased plan, done log, known issues
- [Design](docs/design.md) — scope (what OCP owns vs doesn't)
- [Architecture](docs/architecture.md) — north/core/south model, source layout
- [Configuration](docs/config-reference.md) — all env vars and seed format
- [PR Review Flow](docs/flow.md) — end-to-end council flow for PR review
- [Template page](TEMPLATE.md) — one-page deploy + trigger-track onboarding
- [Deploy & install](docs/deploy.md) — quick-start (PAT), App/webhook dogfood route, and copied-Action option
- [GitHub App validation](docs/github-app-validation.md) — App identity setup + L3 runbook
- [PR review steering](skills/pr-review/SKILL.md) — reviewer + chair output format (source of truth)
- [Decision records (ADRs)](docs/adr/) — 001 three-planes · 002 identity-scope · 003 steering-delivery · 004 bot-identity · 005 cost-governance
- Enterprise hardening — consolidated requirements: issue #29

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
`chair:chair,rev1:reviewer,rev2:reviewer` → a template deploy is **1 control-plane
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

Proven live at 1 / 3 / 5 bots. **How many pods exist** is manual — you pick N by how
many you deploy (`OABCP_BOTS`) + which names go in the session roster. **How many
actually review a given PR** is preset-driven and automatic: a preset
(`lite`/`quick`/`standard`/`full`, default lite) picks 1/3/5/7 angles and
`assign_angles` round-robins them onto the roster, trimming idle reviewers (quorum =
participants) — set per-PR via a `review:<preset>` label or globally via
`OABCP_COUNCIL_PRESET`. To resize the pod pool: edit `OABCP_BOTS` + add/remove pod services and
redeploy, or `POST /v1/sessions/:id/roster` to add a bot mid-session (removal/leave
is not built yet).

## South (a stock OpenAB bot)

Register a bot to get its token, then point an OpenAB pod's `[gateway]` at the
plane — no proxy patch, stock image. See `config.toml.example`.

## Test

```sh
cargo test                # 51 unit + 10 integration (1/3/5-bot parity, close-path regressions)
```

`tests/spike.rs` drives mock bots over the real gateway wire protocol: thread
creation, reactions, streaming edits, and full 3-/5-bot councils to a closed
verdict with one-thread-per-session convergence.
