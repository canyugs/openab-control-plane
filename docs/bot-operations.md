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
| Bot identity | `OABCP_BOTS` | Which bot ids exist in the plane |
| Standing council | `OABCP_COUNCIL_ROSTER` | Which bot ids are convened for webhook reviews |
| Running pod | Zeabur service image, command, env, volume | Which CLI/provider the bot actually runs |

These three names must stay aligned. If the roster contains `rev3`, there must be
a registered bot id `rev3`, and a running service that connects with
`/bot-config/rev3`.

`OABCP_BOTS` is additive and idempotent. Removing a name from `OABCP_BOTS` does
not purge an existing row from the database. The important operational switch is
`OABCP_COUNCIL_ROSTER`: if a bot is not in that roster, webhook reviews will not
call it.

The first bot in `OABCP_COUNCIL_ROSTER` is the chair. The chair synthesizes the
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
should create an inventory; `OABCP_COUNCIL_ROSTER`, a controller, or an operator
still decides admission.

Current state:

- The static path is `OABCP_BOTS` + `/bot-config/<name>`.
- `POST /v1/bots` can register a bot identity.
- `GET /v1/bots` lists inventory metadata and standing-roster membership.
- `POST /v1/bots/discover` can bootstrap or refresh a bot when
  `OABCP_BOT_DISCOVERY_TOKEN` is set.
- `PATCH /v1/bots/:id` lets an operator mark health/enabled/provider metadata.
  It requires the north API key, not the discovery token. Send JSON `null` for
  nullable metadata fields such as `provider`, `note`, `version`, or `runtime`
  to clear them.
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
5. Operators or controllers choose whether to add the bot to
   `OABCP_COUNCIL_ROSTER` or a specific session roster.

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
- Chair selection remains explicit: first in `OABCP_COUNCIL_ROSTER` or
  `chair_bot` in an open-session action.
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
`DELETE FROM bots`/PVC wipe still resets it. No Prometheus/OTel — JSON is enough
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

1. Edit the control-plane env:

   ```text
   OABCP_COUNCIL_ROSTER=chair,rev1
   ```

2. Restart the control-plane.
3. Stop or delete the `rev2` Zeabur service if it is no longer needed.
4. Leave `OABCP_BOTS` alone unless you are deliberately cleaning up identities.
   A leftover `rev2` identity is harmless while it is not in
   `OABCP_COUNCIL_ROSTER`.

Validation:

- Open or manually trigger a review.
- Confirm the session roster no longer includes `rev2`:

  ```sh
  curl -H "Authorization: Bearer $KEY" \
    "$PLANE/v1/sessions?trigger_ref=github%3Apr%2Fowner%2Frepo%23123"
  ```

Rollback:

1. Set `OABCP_COUNCIL_ROSTER=chair,rev1,rev2`.
2. Restart the control-plane.
3. Restart the `rev2` service.

## Add A Reviewer

Use this when you want a larger standing council.

Example: add `rev3`.

1. Edit the control-plane env:

   ```text
   OABCP_BOTS=chair:chair,rev1:reviewer,rev2:reviewer,rev3:reviewer
   OABCP_COUNCIL_ROSTER=chair,rev1,rev2,rev3
   ```

2. Create a new Zeabur service for `rev3`.
3. Configure the service command to fetch the matching bot config:

   ```sh
   openab run -c http://control-plane.zeabur.internal:8090/bot-config/rev3
   ```

4. If `rev3` should use another provider, append the provider query:

   ```sh
   openab run -c http://control-plane.zeabur.internal:8090/bot-config/rev3?agent=codex
   ```

5. Set the provider credential on the `rev3` service, for example
   `OPENAI_API_KEY` for `codex` or `GEMINI_API_KEY` for `gemini`.
6. Use a service image that contains the selected CLI.
7. Restart the control-plane so it seeds `rev3`.
8. Start or restart `rev3`.

Validation:

- Check the control-plane logs for `seeded from OABCP_BOTS bot="rev3"`.
- Check the `rev3` logs for `connected to gateway`.
- Trigger a review and confirm `rev3` appears in the session roster.

Rollback:

1. Remove `rev3` from `OABCP_COUNCIL_ROSTER`.
2. Restart the control-plane.
3. Stop or delete the `rev3` service.

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

`OABCP_BOTS` and `OABCP_COUNCIL_ROSTER` do not need to change because the bot id
is still `rev1`.

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

1. Add the new identity:

   ```text
   OABCP_BOTS=chair:chair,chair2:chair,rev1:reviewer,rev2:reviewer
   ```

2. Put `chair2` first in the standing roster:

   ```text
   OABCP_COUNCIL_ROSTER=chair2,rev1,rev2
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
7. Restart the control-plane.
8. Start or restart `chair2`.
9. Verify `chair2` is connected and can write to GitHub.
10. Stop the old `chair` service after a successful test.

Rollback:

1. Set `OABCP_COUNCIL_ROSTER=chair,rev1,rev2`.
2. Restore GitHub write-back credentials on `chair`.
3. Restart the control-plane and `chair`.
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
