# Scale the Council (local dogfood)

Runbook for growing / shrinking the reviewer fleet on the `oabcp-local`
dogfood, toward a council large enough to **fully stand in for PR review**,
then scaling it down or up on demand.

Scope: local Kubernetes (`docker-desktop` context, `oabcp-local` namespace).
Production scaling is the same knobs applied to Zeabur service env — see
[config-reference.md](config-reference.md) and [bot-operations.md](bot-operations.md).

**Verified 2026-07-05:** a 4-reviewer mixed council (chair + rev1–rev4, 3 Kiro +
2 Claude) convened via `open-council.sh` with `quorum_n=4`, converged in ~140s
(approve r0 y4 g5), zero rate-limit/quota errors, no OOM on the 6-CPU / 8-GB node.

## The knobs (effective reviewers = min of them)

| Knob | Where | Controls |
| --- | --- | --- |
| **A. Reviewer identities + pods** | `OABCP_BOTS` on the plane + one running pod per bot | Which reviewers *exist* |
| **B. Convened roster** | webhook: `OABCP_COUNCIL_ROSTER`; manual driver: `open-council.sh`'s `ROSTER` | Which of them a given council *invites* |
| **C. Preset angles** | webhook: `OABCP_COUNCIL_PRESET`; manual: `open-council.sh --preset` | How many invited reviewers actually deliberate |

`assign_angles` (src/council.rs) round-robins preset angles onto reviewers:
angles ≤ reviewers → the first N take one each, extras are **trimmed** (quorum
doesn't wait on them); angles > reviewers → all participate, some cover several.
Quorum = participating reviewers (chair excluded). `lite`=1, `quick`=3,
`standard`=5, `full`=7 angles. Without a preset the convened roster all
participate and quorum = every reviewer in it.

`OABCP_MAX_ROSTER` (default 16) caps a session: chair + 15 reviewers.

## ⚠️ Three papercuts that make "just scale the plane" not work

Learned the hard way scaling 2→4 reviewers. Each dial has a *separate* source of
truth; raising one alone silently does nothing:

1. **`open-council.sh` hardcodes `ROSTER=["chair","rev1","rev2"]`** — it does NOT
   read the plane's live roster. Growing `OABCP_BOTS`/`OABCP_COUNCIL_ROSTER` on
   the plane does not change who the manual driver invites. You **must** pass the
   full roster explicitly: `ROSTER='["chair","rev1","rev2","rev3","rev4"]'
   open-council.sh …`. Miss this and you silently get a 2-reviewer, quorum-2
   council no matter how many pods are up.
2. **`dev-deploy-bots.sh` takes the bot list from `--bots`, not `--bot-agents`**
   (default `chair,rev1,rev2`). Passing only `--bot-agents rev3=…,rev4=…` deploys
   nothing new — you must also pass `--bots rev3,rev4` (or the full list).
3. **`dev-deploy-k8s.sh` does NOT forward `OABCP_COUNCIL_PRESET`** (only
   `OABCP_BOTS` / `OABCP_COUNCIL_ROSTER`). To set a webhook-path preset, apply it
   separately: `kubectl -n oabcp-local set env deploy/control-plane
   OABCP_COUNCIL_PRESET=standard`. For the manual dogfood path, use
   `open-council.sh --preset` instead — the plane env is irrelevant there.

## Baseline (as shipped locally)

- Plane `dev-deploy-k8s.sh`: `OABCP_BOTS=chair:chair,rev1:reviewer,rev2:reviewer`,
  `OABCP_COUNCIL_ROSTER=chair,rev1,rev2`, no preset (webhook path → `lite`).
- Bots `dev-deploy-bots.sh`: 3 pods, Kiro (`?agent=kiro`, `KIRO_API_KEY`).
- Gateway tokens: legacy auto-mint (plane generates per bot, served inline).

## ⚠️ Redeploy hazard

Changing plane env (`OABCP_BOTS` / `OABCP_COUNCIL_ROSTER` / preset) redeploys the
plane, which **wipes the in-container SQLite DB and re-mints every bot's gateway
token** → you **must restart all bot pods** so they re-fetch `/bot-config`.
Since PR #94 (C7) a rolling bot restart no longer strands a live bot, so the
restart is safe; the plane restart itself still costs the disposable DB. The
plane pod swap also breaks the `port-forward svc/control-plane` — restart it.

`kubectl set env deploy/control-plane …` keeps the current image and rolls only
the plane, but it is NOT non-destructive: the control-plane deployment has no
volume (`/data` is the container's ephemeral filesystem), so the pod swap still
wipes the DB and re-mints tokens. C7 makes the follow-up bot restart *safe* (no
stranding); it does not remove the *need* for it — a bot holding a stale token
fails re-auth against the fresh DB. A durable `/data` PVC would make plane
restarts non-destructive and is the real fix (Track C observations); until then,
every plane env change = restart all bots.

## Multi-provider — what's auto vs manual

To dodge one provider key's rate limit at scale, split reviewers across providers.

**Automatic:**
- The `[agent]` command/args for 6 built-in profiles (plane serves on `?agent=<name>`):
  `claude`, `codex`, `gemini`, `grok`, `kiro`, `copilot` (src/api.rs `builtin_agent_profile`).
- The pod image for **claude and kiro only** (`dev-deploy-bots.sh default_image_for_agent`).
- The per-bot gateway token (plane-minted, provider-independent).

**Manual (per provider):**
- **The credential — always.** Built-in profiles declare no `inherit_env`, so
  nothing auto-wires the key. Create a k8s Secret and map it:
  `--agent-secret <agent>=<secret-name>:<ENV_NAME>` (e.g.
  `kiro=kiro-api:KIRO_API_KEY`, `claude=claude-oauth:CLAUDE_CODE_OAUTH_TOKEN`).
- **The image** for any non-claude/kiro provider → `--agent-images <agent>=<image>`.
- **A custom profile / trust args** for an unlisted CLI → `--agent-profiles-json`
  (host) or `OABCP_AGENT_PROFILES` (plane env).

## Scale UP — add reviewers (mixed providers)

Example: 2 → 4 reviewers, `rev1/rev3=kiro`, `rev2/rev4=claude`.

```sh
# 0. (Claude only) create the credential Secret once, if absent.
#    claude setup-token → CLAUDE_CODE_OAUTH_TOKEN, then:
#    kubectl -n oabcp-local create secret generic claude-oauth \
#      --from-file=CLAUDE_CODE_OAUTH_TOKEN=<file-with-token>

# 1. Grow the plane roster (set env → rolls only the plane; keeps the image).
kubectl --context docker-desktop -n oabcp-local set env deploy/control-plane \
  OABCP_BOTS="chair:chair,rev1:reviewer,rev2:reviewer,rev3:reviewer,rev4:reviewer" \
  OABCP_COUNCIL_ROSTER="chair,rev1,rev2,rev3,rev4"

# 2. Restart the port-forward (plane pod was replaced).
pkill -f "port-forward svc/control-plane 8090"; \
  kubectl --context docker-desktop -n oabcp-local port-forward svc/control-plane 8090:8090 &

# 3. Deploy the new bot pods. --bots is the name list; --bot-agents the map.
scripts/dev-deploy-bots.sh \
  --bots rev3,rev4 \
  --bot-agents rev3=kiro,rev4=claude \
  --agent-secret kiro=kiro-api:KIRO_API_KEY \
  --agent-secret claude=claude-oauth:CLAUDE_CODE_OAUTH_TOKEN \
  --extra-secret gh-token:GH_TOKEN \
  --steering-file docs/steering/pr-review.md

# 4. Restart the EXISTING bots too — step 1 wiped the DB + re-minted tokens, so
#    chair/rev1/rev2 hold stale tokens and would drop on their next reconnect.
kubectl --context docker-desktop -n oabcp-local rollout restart deploy/chair deploy/rev1 deploy/rev2
```

Step 4 is mandatory whenever step 1 ran (any plane env change): the token re-mint
forces every pre-existing bot to re-fetch `/bot-config`. C7 makes these restarts
safe (a rolling reconnect no longer strands a live bot); it does not let you skip
them. If a *fresh* pod sticks at `connected=false` on first boot, restart just
that deploy (the double-connect race — see the gotcha below).

## Scale DOWN

```sh
scripts/dev-deploy-bots.sh --replicas 0          # park all bots, plane untouched
kubectl --context docker-desktop -n oabcp-local delete deploy rev3 rev4  # drop pods
scripts/dev-deploy-bots.sh --delete              # remove all bot deployments
```

To stop convening a reviewer, drop it from the roster you pass (`ROSTER=` for the
manual driver, `OABCP_COUNCIL_ROSTER` for the webhook path) — deleting its pod is
only needed to reclaim resources.

## Verify + run a scale test

```sh
# All bots connected before opening (a straggler shrinks the first council).
curl -s -H "Authorization: Bearer $KEY" "$PLANE/v1/bots" \
  | python3 -c "import sys,json; [print(b['id'],b['connected']) for b in json.load(sys.stdin)['bots']]"

# Drive a full-roster council — MUST pass ROSTER (papercut #1) or you get quorum-2.
ROSTER='["chair","rev1","rev2","rev3","rev4"]' PLANE=… KEY=… \
  scripts/open-council.sh --watch --self-fetch <owner/repo#pr>

# Confirm it actually ran at scale: quorum_n should equal your reviewer count.
curl -s -H "Authorization: Bearer $KEY" "$PLANE/v1/sessions?limit=1" \
  | python3 -c "import sys,json; s=json.load(sys.stdin)['sessions'][0]; print('quorum_n=',s['quorum_n'],'r/y/g=',s['findings_red'],s['findings_yellow'],s['findings_green'])"
```

## Scale-test gotchas

- **Shared model quota.** N reviewers on one provider key hit rate limits. The
  2026-07-05 4-way run split 3 Kiro / 2 Claude across two keys → zero throttling.
  Mix providers (`--bot-agents rev=claude,…` + that provider's Secret) before a
  single key saturates.
- **Double-connect on fresh-pod startup.** A newly created bot pod occasionally
  opens two WS connections; the surviving one drops ~0.5s later and openab does
  not auto-retry, leaving it `connected=false`. Restart that one deploy to get a
  clean single connection. Distinct from C7 (which fixed a *superseded old* conn
  clobbering the flag) — see PLAN C8.
- **Liveness grace.** `OABCP_LIVENESS_GRACE_SECS=60`: a pod not connected within
  the window is flipped `unreachable` and trimmed. Big fleets cold-start slower —
  wait for all `connected=true` before opening.
