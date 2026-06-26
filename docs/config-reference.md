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
| `OABCP_AGENT_COMMAND` | `claude` | Default agent provider when a pod's `/bot-config` fetch has no `?agent=`. Set this to make a uniform single-provider council; leave default + use `?agent=` per pod to mix |
| `OABCP_SESSION_TIMEOUT_SECS` | `900` | Liveness watchdog deadline. A session still active this many seconds after creation is force-closed (verdict notes absentees) so a silent/dead reviewer can't hang it forever. Anchored on `created_at` (no last-activity reset) — raise for legitimately long councils |
| `GH_OUTPUT` | _(off)_ | Set to `1` to enable GitHub PR side-effects (comment, label, review) via `gh` CLI |
| `RUST_LOG` | `info` | Log level filter (standard `tracing` env filter syntax) |

## Bot pods (set on OpenAB containers, not the plane)

BYOK: set **one** credential matching the agent (`OABCP_AGENT_COMMAND`). Both a
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

Device-flow-only agents (Claude CLI proper, OpenCode, Hermes, Cursor, Antigravity,
MiMo) can't BYOK by env var — they need interactive login, not supported on the
gateway path today.

## Per-bot provider (mixed councils)

Each bot is a separate pod, so each can run a different agent CLI on its own
credential. The provider is chosen per pod via the `/bot-config` fetch URL:

```
/bot-config/<id>?agent=gemini     # this pod runs Gemini
/bot-config/<id>?agent=codex      # this pod runs Codex (OpenAI)
/bot-config/<id>                  # falls back to OABCP_AGENT_COMMAND, then claude
```

Known providers (the plane emits the matching `command` + `args`):

| `?agent=` | command + args |
|-----------|----------------|
| `claude` | `claude-agent-acp` |
| `codex` | `codex-acp` |
| `gemini` | `gemini --acp` |
| `grok` | `grok agent stdio` |
| `kiro` | `kiro-cli acp --trust-all-tools` |
| `copilot` | `copilot --acp --stdio` |
| _(anything else)_ | used verbatim as `command`, no args |

The pod must run the matching agent image (e.g. `Dockerfile.gemini`) and carry
that provider's key. **Mixing is the default** when the template wires each pod
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
