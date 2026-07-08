# Bot Operations Runbook

This runbook covers how to reduce, add, or replace OCP council bots after an
install is already running.

Use it when:

- Claude subscription quota is exhausted and you need to switch a bot to another
  CLI/provider.
- A repository needs fewer or more reviewers.
- The chair needs to move to a different pod or provider.
- You need to understand which settings affect the standing council.

## Mental Model

OCP separates three concerns:

| Layer | Controlled by | What it decides |
|---|---|---|
| Bot identity | `POST /v1/bots`, `POST /v1/bots/discover`, first-boot `OABCP_BOTS` | Which bot ids exist in the plane |
| Standing council | `PUT /v1/council/roster` runtime override, env fallback | Which bot ids are convened for webhook reviews |
| Running pod | Zeabur service image, command, env, volume | Which CLI/provider the bot actually runs |

These three names must stay aligned. If the roster contains `rev3`, there must be
a registered bot id `rev3`, and a running service that connects with
`/bot-config/rev3`.

`OABCP_BOTS` is first-boot seeding only. Once the bots table is non-empty, the
plane ignores `OABCP_BOTS`; add identities with the APIs and retire them with
`DELETE /v1/bots/:id`. The important operational switch is the standing roster:
if a bot is not in that roster, webhook reviews will not call it.

The first bot in the standing roster is the chair. The chair synthesizes the
verdict and is the only bot expected to write PR comments.

This runbook assumes one OCP deployment manages one operational group. That group
can cover multiple repositories, but it shares one bot inventory, one standing
council policy, and one trust boundary. Use separate OCP deployments for separate
teams, products, or credential domains.

## Bot Discovery

Bot discovery is possible, but it should be split into two separate decisions:

| Step | Meaning | Should it be automatic? |
|---|---|---|
| Discover | OCP learns that a bot pod exists, is connected, and has capabilities | yes |
| Admit | OCP includes that bot in a session or standing council | policy-gated |

Do not let any pod add itself to the review council only because it connected.
That would let a misconfigured or compromised pod influence reviews. Discovery
should create an inventory; the standing roster, a controller, or an operator
still decides admission.

Current state:

- The static path is first-boot `OABCP_BOTS` + `/bot-config/<name>`.
- `POST /v1/bots` can register a bot identity and returns its gateway token.
- `GET /v1/bots` lists inventory metadata and standing-roster membership.
- `POST /v1/bots/discover` can bootstrap or refresh a bot when
  `OABCP_BOT_DISCOVERY_TOKEN` is set.
- `PATCH /v1/bots/:id` lets an operator mark health/enabled/provider metadata.
  It requires the north API key, not the discovery token. Send JSON `null` for
  nullable metadata fields such as `provider`, `note`, `version`, or `runtime`
  to clear them.
- `DELETE /v1/bots/:id` retires an idle, non-rostered identity and its gateway
  token. Remove it from the roster and stop the pod first.
- Connected bots already update connection state internally.
- Recruiting an unknown bot emits `provision_requested`; see
  [provisioner.md](provisioner.md).

Discovery flow:

1. A bot pod starts with a narrow bootstrap credential, not the root north API key.
2. The pod calls a discovery endpoint with:

   ```json
   {
     "id": "rev3",
     "role": "reviewer",
     "provider": "codex",
     "capabilities": ["review", "gh-read"],
     "version": "openab:0.9.0-beta.7"
   }
   ```

3. OCP registers or refreshes the bot identity, records metadata, and returns the
   pod's `/bot-config/<id>?agent=<provider>` URL.
4. The pod fetches that config and connects to `/ws` with its bot token.
5. Operators or controllers choose whether to add the bot to the standing roster
   or a specific session roster.

Refreshing an existing bot through discovery updates inventory metadata only. It
does not let a pod change an existing bot's name or role. Omitted metadata fields
are left unchanged; send an empty `capabilities` array to clear capabilities.

Security requirements for discovery:

- Registration must be allowlisted by install, project, or signed bootstrap
  token.
- Self-reported capabilities are inventory hints, not authorization grants.
- Self-reported role is only used when creating a newly discovered identity; it
  is not allowed to overwrite an existing identity's role.
- Reviewers must not receive GitHub write credentials through discovery.
- Chair selection remains explicit: first in the standing roster or `chair_bot`
  in an open-session action.
- Offline or stale bots should remain discoverable but not auto-selected.

This gives the useful operator experience ("show me available bots and
providers") without making the runtime trust every connected pod.

The design rationale is captured in
[ADR 009](adr/009-quota-failover-bot-discovery.md): discovery exists primarily
to support quota failover and safe replacement inside a single OCP group, not to
auto-admit pods or create a multi-tenant bot registry.

## Observability (`GET /v1/stats`)

A read-only JSON snapshot for eyeballing review throughput and infra state (north
API key required). No new dependency or collection — it aggregates the existing
`sessions` / `bots` / `outbox` tables on demand.

```sh
curl -s -H "Authorization: Bearer $KEY" "$PLANE/v1/stats" | jq
```

```json
{
  "sessions": {
    "by_state": { "open": 1, "closed": 12, "aborted": 0 },
    "closed_24h": 9,
    "time_to_verdict_ms": { "p50": 138000, "p95": 210000, "count": 12 },
    "by_mode": { "council": 12, "solo": 1 },
    "by_decision": { "approve": 10, "reject": 2 },
    "findings": { "red": 3, "yellow": 41, "green": 78, "avg_per_session": 10.2 }
  },
  "bots": {
    "total": 5, "connected": 5,
    "by_health": { "ok": 5 },
    "detail": [ { "id": "chair", "connected": true, "health": "ok",
                  "last_seen_ms": 1783183627000, "version": "openab:0.9.0-beta.7" } ]
  },
  "outbox": { "pending": 0 }
}
```

Honest ceilings: findings are a **distribution, not a quality signal** — whether
the verdicts were *right* is the eval harness's job, not these counts. It's a
live snapshot over whatever the current DB holds; with the durable `/data` PVC
(see [scale-council.md](scale-council.md)) that now spans plane restarts, but a
`DELETE /v1/bots/:id` or PVC wipe still changes it. No Prometheus/OTel — JSON is enough
for humans and scripts.

## Provider Map

The provider is selected by either:

- a per-pod config URL: `/bot-config/<id>?agent=<provider>`
- the plane-wide default: `OABCP_AGENT_COMMAND=<provider>`

Known provider values:

| Provider | OCP emits | Required credential |
|---|---|---|
| `claude` | `claude-agent-acp` | `CLAUDE_CODE_OAUTH_TOKEN` or `ANTHROPIC_API_KEY` |
| `codex` | `codex-acp` | `OPENAI_API_KEY` |
| `gemini` | `gemini --acp` | `GEMINI_API_KEY` |
| `grok` | `grok agent stdio` | `GROK_CODE_XAI_API_KEY` |
| `kiro` | `kiro-cli acp --trust-all-tools` | `KIRO_API_KEY` |
| `copilot` | `copilot --acp --stdio` | `COPILOT_GITHUB_TOKEN` |

Changing `?agent=` only changes the config OCP serves. The bot service image must
already contain the selected CLI. The current Zeabur templates deploy Claude
OpenAB pods by default, so switching to Codex/Gemini/etc. may require changing the
bot service image as well as env vars.

## Safety Rules

- Do not share bot volumes as a shortcut. Credentials, CLI cache, session state,
  and lock files should remain pod-local.
- Keep GitHub write credentials only on the chair. Reviewers should not carry
  `GH_TOKEN` or App write credentials.
- When changing the chair, verify GitHub write-back separately from model auth.
- Do not install both the copied Action and webhook trigger on the same target
  repo.
- Prefer replacing one bot at a time so a failed provider switch has a small
  blast radius.

## Reduce Review Work

Use this when the council is too slow or too expensive, but you do not need to
delete services.

For a single PR, add one of these labels:

| Label | Angles |
|---|---:|
| `review:lite` | 1 |
| `review:quick` | 3 |
| `review:standard` | 5 |
| `review:full` | 7 |

For all webhook-triggered reviews, set the control-plane env:

```text
OABCP_COUNCIL_PRESET=lite
```

Restart the control-plane after changing the env. This does not change which bots
exist; it only changes how much review work is assigned.

## Remove A Reviewer From The Standing Council

Use this when a reviewer should stop participating in webhook reviews.

Example: remove `rev2` from a `chair,rev1,rev2` council.

1. Drop `rev2` from the runtime standing roster:

   ```sh
   curl -X PUT "$PLANE/v1/council/roster" \
     -H "Authorization: Bearer $KEY" \
     -H "Content-Type: application/json" \
     -d '{"roster":["chair","rev1"]}'
   ```

2. Stop or delete the `rev2` Zeabur service if it is no longer needed.
3. Optional full identity retirement:

   ```sh
   curl -X DELETE "$PLANE/v1/bots/rev2" \
     -H "Authorization: Bearer $KEY"
   ```

   `DELETE` returns `409` while the bot is still in the standing roster, still
   connected, or belongs to an active session. Remove it from the roster and stop
   the pod first; wait for active sessions to close.

Validation:

- Open or manually trigger a review.
- Confirm the session roster no longer includes `rev2`:

  ```sh
  curl -H "Authorization: Bearer $KEY" \
    "$PLANE/v1/sessions?trigger_ref=github%3Apr%2Fowner%2Frepo%23123"
  ```

Rollback:

1. If you deleted the identity, register it again with `POST /v1/bots` or
   `POST /v1/bots/discover`, then update the bot service with the new token or
   config URL.
2. Put `rev2` back in the runtime standing roster with
   `PUT /v1/council/roster`.
3. Start the `rev2` service.

## Add A Reviewer

Use this when you want a larger standing council.

Example: add `rev3`.

1. Register the identity.

   Use `POST /v1/bots` when the service owns its runtime config and can use the
   returned gateway token:

   ```sh
   curl -X POST "$PLANE/v1/bots" \
     -H "Authorization: Bearer $KEY" \
     -H "Content-Type: application/json" \
     -d '{"name":"rev3","role":"reviewer"}'
   ```

   Or use discovery when you need a stable chosen id such as `rev3` for
   `/bot-config/rev3`:

   ```sh
   curl -X POST "$PLANE/v1/bots/discover" \
     -H "Authorization: Bearer $OABCP_BOT_DISCOVERY_TOKEN" \
     -H "Content-Type: application/json" \
     -d '{"id":"rev3","role":"reviewer","provider":"codex","capabilities":["review"]}'
   ```

2. Add the new id to the runtime standing roster:

   ```sh
   curl -X PUT "$PLANE/v1/council/roster" \
     -H "Authorization: Bearer $KEY" \
     -H "Content-Type: application/json" \
     -d '{"roster":["chair","rev1","rev2","rev3"]}'
   ```

3. Create a new Zeabur service for `rev3`.
4. Configure the service command to fetch the matching bot config:

   ```sh
   openab run -c http://control-plane.zeabur.internal:8090/bot-config/rev3
   ```

5. If `rev3` should use another provider, append the provider query:

   ```sh
   openab run -c http://control-plane.zeabur.internal:8090/bot-config/rev3?agent=codex
   ```

6. Set the provider credential on the `rev3` service, for example
   `OPENAI_API_KEY` for `codex` or `GEMINI_API_KEY` for `gemini`.
7. Use a service image that contains the selected CLI.
8. Start `rev3`.

Validation:

- `GET /v1/bots` lists `rev3` with `rostered: true`.
- Check the `rev3` logs for `connected to gateway`.
- Trigger a review and confirm `rev3` appears in the session roster.

Rollback:

1. Remove `rev3` with `PUT /v1/council/roster`.
2. Stop or delete the `rev3` service.
3. Optionally retire the identity with `DELETE /v1/bots/rev3`.

## Attach An Existing OpenAB Instance

Use this when you already run an OpenAB and want OCP to control it, instead of
letting OCP stand up a fresh pod. OCP never reaches into a pod — a bot **dials
out** to the plane's `/ws`, so "attach" means pointing your OpenAB at the plane
and registering its identity. The instance must be a build that speaks the
gateway protocol ([ADR 008](adr/008-external-controller-protocol.md)) and must be
able to reach the plane's `OABCP_WS_URL`.

**Connecting is not joining the council.** An attached instance becomes a
controllable, idle bot; it only reviews once you add its id to
the standing roster (see *Add A Reviewer*). This is the deliberate trust
boundary — see *Safety Rules*.

First, register the identity (either way):

- API: `POST /v1/bots` (returns the bot id + a gateway token), or
- Discovery: `POST /v1/bots/discover` when you need a chosen id and
  `OABCP_BOT_DISCOVERY_TOKEN` is enabled.

`OABCP_BOTS` is only first-boot bootstrap for a new empty database; do not use it
for day-2 membership changes.

Then pick a path by **who owns the runtime config**:

### Path A — bootstrap config (`/bot-config`), quick trials

Point the instance at the plane's config URL:

```sh
openab run -c http://control-plane.zeabur.internal:8090/bot-config/<id>
```

The instance now runs the plane's **minimal** config
([ADR 010](adr/010-openab-configurl-boundary.md) keeps `/bot-config` as the
bootstrap path; the served token is externalized per
[ADR 016](adr/016-gateway-token-externalization.md)). ⚠️ Your
instance's own `[agent]` / tools / steering do **not** apply on this run unless
you replicate them into `OABCP_AGENT_PROFILES` on the plane. Good for a fast
"does it connect and review" check; not for an instance with custom settings.

### Path B — keep your config, add the gateway (production, preserves settings)

Per [ADR 010](adr/010-openab-configurl-boundary.md), OpenAB keeps owning its
runtime config; OCP owns only identity + gateway. Leave your `config.toml`
(agent, tools, steering, `pre_seed`, secrets) untouched and add a `[gateway]`
block pointing at the plane:

```toml
# `platform = "feishu"` is just the gateway adapter name — not a Feishu account.
# `token` is the OCP gateway token from registration, NOT a platform/Feishu token.
[gateway]
url = "wss://<plane-domain>/ws"   # the plane's OABCP_WS_URL
platform = "feishu"
token = "${OABCP_BOT_TOKEN}"      # OCP gateway token (env-expanded at boot, ADR 016)
allow_all_users = true
allow_bot_messages = true
bot_username = "<id>"
streaming = true
```

**A11 MUST:** keep `allow_bot_messages = true` on attached Path B bots. If it is
omitted, OAB silently drops council-peer speech after the plane's outbox has
already marked the message delivered; the plane cannot see the loss, and the
thread goes quiet until the watchdog. `allow_all_users = true` is belt-only here
because OAB already allows all users when the list is empty, but keep it explicit
so the WS token remains the trust boundary. This requirement also belongs on
upstream OAB's gateway compliance list; that upstream note is outside this PR.

Keep starting the instance with **your own** config
(`openab run -c <your-configUrl>`). All original settings are preserved; OCP only
coordinates sessions and rosters on top.

Ready-made reference: [`docs/pod-config/`](pod-config/) holds the exact
credential-free `config.toml` files the Zeabur template pods mount at
`/etc/openab/config.toml` (ADR 010 B2 demotion). They demonstrate the full
Path B shape — `${OABCP_BOT_TOKEN}`/`${OABCP_BOT_NAME}` env expansion, the
pinned trust fields, and an `[agent]` section that deliberately names no
command so the pod image's own `OPENAB_AGENT_COMMAND` decides.

Either path is **reversible and non-destructive**: `-c` is a per-run argument and
never rewrites the on-disk config. To detach: Path A — stop the run and point the
instance back at its own config; Path B — remove the `[gateway]` block and restart
it.

**Register-only (no session/roster rights):** to let an instance report in for
inventory without granting it anything, use `POST /v1/bots/discover` with
`OABCP_BOT_DISCOVERY_TOKEN` (see *Bot Discovery*). It records metadata; it cannot
open sessions or change rosters.

Validation:

- `POST /v1/bots` returned the id and gateway token, or
  `POST /v1/bots/discover` returned the chosen id's config URL.
- Instance logs show `connected to gateway`.
- `GET /v1/stats` lists the id as `connected: true`.

Rollback:

1. If added to the council, remove the id with `PUT /v1/council/roster`.
2. Detach the instance:
   - Path A: stop the `-c <plane>/bot-config/<id>` run; point it back at its own config.
   - Path B: remove the `[gateway]` block from your config and restart it.

## Replace A Reviewer Provider

Use this when a model quota is exhausted or when you want to test another CLI
without changing the council identity.

Example: replace `rev1` from Claude to Codex.

1. Stop or pause the `rev1` service.
2. Change the service image to one that contains `codex-acp`.
3. Change the service command:

   ```sh
   openab run -c http://control-plane.zeabur.internal:8090/bot-config/rev1?agent=codex
   ```

4. Remove unused Claude credentials from the `rev1` service if they are no longer
   needed.
5. Set the Codex credential:

   ```text
   OPENAI_API_KEY=<key>
   ```

6. Restart `rev1`.

The bot identity and standing roster do not need to change because the bot id is
still `rev1`.

Validation:

- Check the `rev1` logs for the selected command (`codex-acp` in this example).
- Trigger a small review with `review:lite`.
- Confirm `rev1` responds and signals done.

Rollback:

1. Stop `rev1`.
2. Restore the Claude-capable image.
3. Change the config URL back to `/bot-config/rev1` or `/bot-config/rev1?agent=claude`.
4. Restore `CLAUDE_CODE_OAUTH_TOKEN` or `ANTHROPIC_API_KEY`.
5. Restart `rev1`.

## Replace The Chair Provider

Use this when the chair's model provider is unavailable. Replacing reviewers does
not help if the chair cannot synthesize and post the final verdict.

Example: replace `chair` from Claude to Codex while keeping the same chair id.

1. Stop or pause the `chair` service.
2. Change the service image to one that contains `codex-acp`.
3. Change the service command:

   ```sh
   openab run -c http://control-plane.zeabur.internal:8090/bot-config/chair?agent=codex
   ```

4. Set `OPENAI_API_KEY` on the chair service.
5. Keep the GitHub write-back setup:
   - PAT path: keep `GH_TOKEN` only on the chair.
   - GitHub App path: keep `/home/node/.github-app.pem` and
     `/home/node/bin/get-gh-app-token.sh` on the chair volume.
6. Restart the chair.
7. Verify GitHub write-back:

   ```sh
   npx zeabur@latest service exec --id <CHAIR_SERVICE_ID> -- \
     sh -lc 'HOME=/home/node gh auth status'
   ```

8. Trigger a small review with `review:lite`.

Rollback is the same as a reviewer provider rollback, but also re-check
`gh auth status` afterward.

## Replace The Chair Identity

Use this when you want a different service to become chair, not just a different
provider on the existing `chair` service.

Example: move chair duties from `chair` to `chair2`.

1. Add the new chair identity. Use discovery if you need the stable id
   `chair2` for `/bot-config/chair2`:

   ```sh
   curl -X POST "$PLANE/v1/bots/discover" \
     -H "Authorization: Bearer $OABCP_BOT_DISCOVERY_TOKEN" \
     -H "Content-Type: application/json" \
     -d '{"id":"chair2","role":"chair","provider":"codex","capabilities":["review"]}'
   ```

2. Put `chair2` first in the runtime standing roster:

   ```sh
   curl -X PUT "$PLANE/v1/council/roster" \
     -H "Authorization: Bearer $KEY" \
     -H "Content-Type: application/json" \
     -d '{"roster":["chair2","rev1","rev2"]}'
   ```

3. Create a `chair2` service.
4. Configure its command:

   ```sh
   openab run -c http://control-plane.zeabur.internal:8090/bot-config/chair2
   ```

5. Set model credentials on `chair2`.
6. Move GitHub write-back credentials to `chair2`:
   - PAT path: set `GH_TOKEN` on `chair2`, remove it from the old chair.
   - GitHub App path: upload the App private key and token-minter to `chair2`.
7. Start `chair2`.
8. Verify `chair2` is connected and can write to GitHub.
9. Stop the old `chair` service after a successful test.

Rollback:

1. Restore the old runtime standing roster with `PUT /v1/council/roster`.
2. Restore GitHub write-back credentials on `chair`.
3. Restart `chair`.
4. Stop `chair2`.

## Emergency Quota Playbook

When a review does not produce a PR comment and bot logs show a provider quota
error, choose the smallest change that restores service.

1. If the chair is quota-limited, replace or re-credential the chair first.
2. If only reviewers are quota-limited, replace one reviewer first and run
   `review:lite`.
3. If you have another Claude subscription token, replacing
   `CLAUDE_CODE_OAUTH_TOKEN` is the fastest path.
4. If subscription quota is exhausted globally, switch the affected bot to a
   pay-per-token or alternate provider such as `codex` or `gemini`.
5. After the emergency is over, decide whether to keep the new provider or roll
   back to the normal profile.

## Runtime Add During A Session

The chair can ask OCP to add a registered bot mid-session by writing:

```text
[[recruit:rev3]]
```

This only works when `rev3` is already registered and has a running pod. It is an
escape hatch for a live session, not a substitute for changing the standing
roster. Runtime removal is not supported.

## Final Verification Checklist

After any roster or provider change:

- `control-plane` is running.
- Every rostered bot service is running.
- Bot logs show `connected to gateway`.
- The chair has exactly one GitHub write-back identity.
- Reviewers do not carry PR write credentials.
- A `review:lite` test PR opens a session and reaches a verdict.
- `GET /v1/sessions/:id/log` or `GET /v1/session-log?...` shows the expected bot
  ids and state transitions.
