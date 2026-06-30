# Configuration Reference

All configuration is via environment variables. No config file needed.

## Control plane

| Variable | Default | Description |
|----------|---------|-------------|
| `OABCP_ADDR` | `0.0.0.0:8090` | Listen address (host:port) |
| `OABCP_DB` | `plane.db` | SQLite database path. Use `/data/plane.db` with a persistent volume for durability |
| `OABCP_API_KEY` | _(open)_ | Bearer token for north API authentication. Unset = no auth |
| `OABCP_BOTS` | _(none)_ | Initial bot roster registered at boot. Format: `name:role,name:role,...` e.g. `chair:chair,rev1:reviewer,rev2:reviewer`. Idempotent — existing bots are skipped |
| `OABCP_WS_URL` | auto-detected | WebSocket URL bots connect to. Override when the internal hostname differs from default |
| `OABCP_AGENT_COMMAND` | `claude` | Default agent profile when a pod's `/bot-config` fetch has no `?agent=`. Set this to make a uniform single-provider council; leave default + use `?agent=` per pod to mix |
| `OABCP_AGENT_PROFILES` | _(built-ins)_ | JSON object for overriding or adding OpenAB agent profiles. Each profile can set `command`, `args`, `working_dir`, and `inherit_env`. Put CLI trust/permission flags in `args` |
| `OABCP_AGENT_WORKING_DIR` | profile-specific | Force `[agent].working_dir` for every served bot config. Useful when all bot images use the same non-default home |
| `OABCP_AGENT_INHERIT_ENV` | _(none)_ | Extra comma-separated env var names appended to `[agent].inherit_env` for custom CLIs |
| `OABCP_SESSION_TIMEOUT_SECS` | `900` | Liveness watchdog deadline. A session still active this many seconds after creation is force-closed (verdict notes absentees) so a silent/dead reviewer can't hang it forever. Anchored on `created_at` (no last-activity reset) — raise for legitimately long councils |
| `OABCP_MAX_ROSTER` | `16` | Admission quota — max bots in a session roster. Mid-session adds (`POST /v1/sessions/:id/roster`) beyond this are rejected (`409`). Bounds roster growth; applies to dynamic adds, not the initial roster at open |
| `OABCP_COUNCIL_ROSTER` | `chair,rev1,rev2` | Webhook-convened council roster (comma-separated; `[0]` is the chair, the rest review). Should match the bots seeded via `OABCP_BOTS` |
| `OABCP_COUNCIL_PRESET` | `lite` | Default webhook-convened review preset: `lite` (1 angle), `quick` (3), `standard` (5), `full` (7). Angles are round-robined onto the reviewers (extras trimmed, quorum = participants). A per-PR `review:<preset>` label overrides this. Mirrors `open-council.sh --preset`; `open-council.sh` itself stays generic unless `--preset` is passed |
| `OABCP_BOT_HANDLE` | _(none)_ | The App bot's GitHub handle (e.g. `zeabur-council`) for conversational follow-ups (ADR 006). When set, a PR comment that `@mention`s it is answered by a solo session. Unset → only the explicit `/ask` command works, not `@mention` |
| `OABCP_ALLOWED_REPOS` | _(allow all)_ | Comma-separated `owner/repo` allowlist for webhook triggers. Unset/empty = allow all; when set, a webhook from any other repo is acked and ignored. Comment commands (`/review`, `/ask`, `@mention`) are additionally gated to write-ish commenters by `author_association`. |
| `GITHUB_WEBHOOK_SECRET` | _(none)_ | HMAC secret for `POST /api/v1/github_webhooks`. **Fail-closed**: unset = every webhook is rejected. Opens a session on a PR `opened`/`reopened`/`ready_for_review`, or a write-ish user's `/review` comment on a PR |
| `GH_OUTPUT` | _(off)_ | Set to `1` to enable GitHub PR side-effects (comment, label, review) via `gh` CLI |
| `RUST_LOG` | `info` | Log level filter (standard `tracing` env filter syntax) |

## Plane-minted GitHub App tokens (optional)

Optional operator capability — lets the **plane** mint per-role scoped installation
tokens through `POST /v1/sessions/:id/github-token` (chair `pull_requests:write`,
reviewers read-only). This is not required for the dogfood pod-local App posting path
in [install-github-app.md](install-github-app.md); that path stores the App key on the chair pod's
`/home/node` volume and authenticates `gh` in the chair's `pre_boot` hook.

| Variable | Default | Description |
|----------|---------|-------------|
| `GITHUB_APP_ID` | _(none)_ | GitHub App id. The plane mints a short-lived App JWT → per-role installation token; a pod fetches its scoped token via `/v1/sessions/:id/github-token` |
| `GITHUB_APP_INSTALLATION_ID` | _(none)_ | Installation the tokens are minted against (single-install today) |
| `GITHUB_APP_PRIVATE_KEY` | _(none)_ | The App's PEM private key — held only by the plane; pods never see it |
| `GITHUB_API_BASE` | `https://api.github.com` | Override the GitHub API base URL (e.g. GitHub Enterprise Server, or a test endpoint) |

## Bot pods (set on OpenAB containers, not the plane)

BYOK: set **one** credential matching the agent profile (`OABCP_AGENT_COMMAND`).
Both a
subscription token and an API key work for Claude. The served config inherits
every var below — the pod only carries whatever you actually set; unset vars are
skipped. Switching provider = change `OABCP_AGENT_COMMAND` + set that provider's key.

| Variable | Provider / agent | Notes |
|----------|------------------|-------|
| `CLAUDE_CODE_OAUTH_TOKEN` | Claude (subscription) | Claude Pro/Max quota — the BYOK default |
| `ANTHROPIC_API_KEY` | Claude / openab-agent (key) | Pay-per-token alternative |
| `OPENAI_API_KEY` | Codex / Pi | |
| `GEMINI_API_KEY` | Gemini | |
| `GROK_CODE_XAI_API_KEY` | Grok (xAI) | |
| `KIRO_API_KEY` | Kiro | |
| `COPILOT_GITHUB_TOKEN` | GitHub Copilot | Optional PAT |
| `GH_TOKEN` | — | GitHub PAT for PR operations. **Set only on the chair pod** to prevent duplicate comments |

**"Use your own login" = token form only.** Your own subscription login *is*
supported — *as a token*: `claude setup-token` mints `CLAUDE_CODE_OAUTH_TOKEN` from
your Claude Pro/Max login (above), and each pod can carry its own. What is **not**
supported on the gateway path today is **interactive / device-flow login** — agents
whose only auth is an interactive login that writes credentials into the pod (Claude
CLI proper, OpenCode, Hermes, Cursor, Antigravity, MiMo) can't be authed by env var
and have no validated on-pod login path. (The chair's persistent volume +
`gh auth login` in its `pre_boot` hook authenticates **`gh`/git for PR write-back**,
not the agent model — don't conflate the two.)

## Per-bot Agent Profile (mixed councils)

Each bot is a separate pod, so each can run a different agent CLI on its own
credential. The provider is chosen per pod via the `/bot-config` fetch URL:

```
/bot-config/<id>?agent=gemini     # this pod runs the gemini profile
/bot-config/<id>?agent=codex      # this pod runs the codex profile
/bot-config/<id>                  # falls back to OABCP_AGENT_COMMAND, then claude
```

Built-in profiles keep the common cases working without extra config:

| `?agent=` | command + args | working dir |
|-----------|----------------|-------------|
| `claude` | `claude-agent-acp` | `/home/node` |
| `codex` | `codex-acp` | `/home/node` |
| `gemini` | `gemini --acp` | `/home/node` |
| `grok` | `grok agent stdio` | `/home/node` |
| `kiro` | `kiro-cli acp --trust-all-tools` | `/home/agent` |
| `copilot` | `copilot --acp --stdio` | `/home/node` |
| _(anything else)_ | used verbatim as `command`, no args | `/home/node` |

Custom profiles go in `OABCP_AGENT_PROFILES`. This is the escape hatch for a new
CLI image, a different home directory, extra credential env vars, or required
trust/permission flags:

```json
{
  "cursor": {
    "command": "cursor-agent",
    "args": ["--acp", "--allow-all-tools"],
    "working_dir": "/home/agent",
    "inherit_env": ["CURSOR_API_KEY"]
  },
  "kiro": {
    "args": ["acp", "--trust-all-tools", "--verbose"]
  }
}
```

For an existing built-in profile, omitted fields keep their built-in value. For a
new custom profile, `command` is required. Permission or sandbox-bypass flags are
not inferred by OCP; put them in `args` so the deploy config makes that trust
decision explicit.

The pod must run the matching agent image and carry that provider's key. OCP only
serves the OpenAB config; it does not install a CLI into the container and does
not create credentials. Keep the axes separate:

| Axis | Where it is configured | Example |
|------|------------------------|---------|
| OAB `[agent]` command/args | `OABCP_AGENT_PROFILES` or built-in profile | `kiro-cli acp --trust-all-tools` |
| Bot image | deployment/template/service image | `ghcr.io/openabdev/openab:0.9.0-beta.3-kiro` |
| Model credential | bot pod env/Secret | `KIRO_API_KEY`, `CLAUDE_CODE_OAUTH_TOKEN` |
| PR write credential | chair pod only | `GH_TOKEN` or pod-local GitHub App setup |

For local Kubernetes testing, `scripts/dev-deploy-bots.sh` can wire these per bot:

```sh
scripts/dev-deploy-bots.sh \
  --bot-agents chair=kiro,rev1=claude,rev2=claude \
  --agent-secret kiro=kiro-api:KIRO_API_KEY \
  --agent-secret claude=claude-oauth:CLAUDE_CODE_OAUTH_TOKEN \
  --chair-secret-name gh-token \
  --chair-credential-env GH_TOKEN
```

Use `--agent-images agent=image,...` for custom profiles without a built-in local
image. **Mixing is the default** when the template or deployment wires each pod
with a different `?agent=`; for a uniform council, set `OABCP_AGENT_COMMAND` and
drop the per-pod param.

## Roster format

`OABCP_BOTS` registers bots at startup so pods can fetch config from
`/bot-config/<name>` without manual `POST /v1/bots` calls.

```
OABCP_BOTS="chair:chair,rev1:reviewer,rev2:reviewer"
```

Each entry is `name:role` (role defaults to `reviewer` if omitted). The bot's
`id` is set equal to `name`, so the pod's fetch URL is known ahead of time. A
random token is generated once per bot, stored, and served inline by
`/bot-config/<name>` — no human ever copies a token. Re-seeding is idempotent
(`INSERT OR IGNORE`): restarts and already-present bots are skipped, so tokens
stay stable across reboots as long as the DB volume persists.

## Add / remove / replace a bot (change the standing council)

Three things carry a bot's name and **must stay aligned**: `OABCP_BOTS` (seeds the
identity), the pod's `/bot-config/<name>` fetch URL (the running container), and
`OABCP_COUNCIL_ROSTER` (who the webhook actually convenes). `OABCP_BOTS` ≠
`OABCP_COUNCIL_ROSTER`: the first decides *which identities exist*, the second
*which of them form a council*.

`OABCP_COUNCIL_ROSTER` is the boot/default roster. Runtime changes are stored in
the control-plane DB and override the env value for future webhook and `/ask`
sessions:

```sh
curl -H "Authorization: Bearer $KEY" "$PLANE/v1/council/roster"

curl -X POST -H "Authorization: Bearer $KEY" -H "content-type: application/json" \
  "$PLANE/v1/council/roster/replace" \
  -d '{"old_bot_id":"rev1","new_bot_id":"rev3"}'
```

Use `PUT /v1/council/roster {"roster":["chair","rev1","rev3"]}` to set the full
standing roster. The first bot must be registered with `role=chair`; every bot
must already exist via `OABCP_BOTS` or `POST /v1/bots`.

**Add a reviewer (e.g. `rev3`)** — all three, names matching:
1. control-plane env: `OABCP_BOTS` += `rev3:reviewer`, `OABCP_COUNCIL_ROSTER` += `rev3`.
2. Add a pod service running `openab run -c <plane>/bot-config/rev3` (append
   `?agent=<provider>` for a mixed council) with that provider's credential env.
3. Restart the control-plane (seeds `rev3`) and deploy the new pod.

**Remove a reviewer**:
1. Drop it from `OABCP_COUNCIL_ROSTER` (so it's no longer convened) and restart the plane.
2. Delete/stop its pod service.
- Dropping it from `OABCP_BOTS` does **not** un-seed the identity (seed is
  `INSERT OR IGNORE` / additive) — but the leftover row is harmless once the bot is
  out of the roster and has no pod. To actually purge it, delete the row from the DB.

**Change the chair** — reorder `OABCP_COUNCIL_ROSTER` so the desired bot is `[0]`.
Only the chair gets the `pre_boot` App hook + PR write, so the new chair pod needs
the write setup: a `GH_TOKEN` (PAT track) or the App key on its volume (App track) —
see [install-pat.md](install-pat.md) or
[install-github-app.md](install-github-app.md).

**Just want fewer bots on a small PR** — don't change composition; use a smaller
preset (`review:lite` label or `OABCP_COUNCIL_PRESET`). Idle reviewers are trimmed
automatically (quorum = participants).

**Mid-session (runtime) add** — `POST /v1/sessions/:id/roster {bot_id}` or chair
`[[recruit:<id>]]` (below). Admission-gated, capped by `OABCP_MAX_ROSTER`.

**Mid-session (runtime) replace** — replace a failed/quota-exhausted bot without
waiting for restart:

```sh
curl -X POST -H "Authorization: Bearer $KEY" -H "content-type: application/json" \
  "$PLANE/v1/sessions/$SESSION/roster/replace" \
  -d '{"old_bot_id":"rev1","new_bot_id":"rev3"}'
```

The replacement must already be registered and must not already be in that
session. OCP preserves roster position, purges pending outbox frames for the old
bot in that session, backfills the new bot with prior messages, and ignores later
replies from the removed bot. Replacing the current chair requires a replacement
registered with `role=chair`. Pure removal mid-session is still not supported.

## Self-recruitment (`[[recruit:<id>]]`)

The **chair** can pull another registered bot onto the panel mid-session by
embedding `[[recruit:<bot_id>]]` anywhere in a message (a text convention, like
`[[reply_to:]]` — no special gateway command). For a seeded roster `id == name`,
so `[[recruit:rev3]]` adds the bot seeded as `rev3`.

The request passes the same admission gate as the north `POST .../roster`:

- **authz** — only the session chair may recruit; a reviewer's directive is denied.
- **valid** — the target must already be registered (seeded or `POST /v1/bots`).
- **bounded** — rejected if the roster is at `OABCP_MAX_ROSTER`.

A recruited bot is backfilled with the conversation so far (durable outbox), so
it can join late and still have full context. North sees `recruit` /
`recruit_denied` / `recruit_rejected` SSE events. `GET /v1/sessions/:id` returns
the current `roster`.

Recruiting a bot that **isn't registered yet** emits `provision_requested`
instead of a plain rejection — the cue for an external fleet provisioner to spin
up that pod. OCP never calls the infra API itself; see
[provisioner.md](provisioner.md).

## Done-signal (how a bot says "I'm finished")

A bot signals completion two interchangeable ways:

- **Text** — end its final message with the token `[done]` (or send a message
  that is only 🆗). This matches the convention the real Discord council uses and
  is what stock agents reliably produce.
- **Reaction** — the OAB-default 🆗 `add_reaction` (`emoji_done`).

Either is counted toward quorum. The text form exists because real agents tend to
write `[done]` rather than emit the gateway reaction (a 🆗 *in passing* mid-message
is **not** a done-signal — only a trailing `[done]` or a bare 🆗). Steering should
tell reviewers and the chair to end their final message with `[done]`; the
`open-council.sh` trigger already does.
