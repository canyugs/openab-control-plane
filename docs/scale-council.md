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
| **A. Reviewer identities + pods** | `POST /v1/bots` or `POST /v1/bots/discover` + one running pod per bot; `OABCP_BOTS` only seeds an empty first-boot DB | Which reviewers *exist* |
| **B. Convened roster** | webhook: runtime `PUT /v1/council/roster`, env fallback; manual driver: `open-council.sh`'s `ROSTER` | Which of them a given council *invites* |
| **C. Preset angles** | webhook: `OABCP_COUNCIL_PRESET`; manual: `open-council.sh --preset` | How many invited reviewers actually deliberate |

`assign_angles` (src/council.rs) round-robins preset angles onto reviewers:
angles ≤ reviewers → the first N take one each, extras are **trimmed** (quorum
doesn't wait on them); angles > reviewers → all participate, some cover several.
Quorum = participating reviewers (chair excluded). `lite`=1, `quick`=3,
`standard`=5, `full`=7 angles. Without a preset the convened roster all
participate and quorum = every reviewer in it.

`OABCP_MAX_ROSTER` (default 16) caps a session: chair + 15 reviewers.

**C5 provider ceiling:** the measured quota knee is much lower than the plane's
mechanical roster cap. In `docs/eval/scale-knee.md`, 429s appeared at N=5
reviewers even split across two provider keys, and N=6 degraded into a 630s close
with `decision=None` plus a missed 🔴. Rule of thumb: plan around ~4 reviewers
per provider key; provider quota binds an order of magnitude before OCP's
fanout/state machinery. K-of-N quorum remains a Stage 4 app-layer policy, not a
Stage 0 mechanism change.

## ⚠️ Three papercuts that make "just scale the plane" not work

Learned the hard way scaling 2→4 reviewers. Each dial has a *separate* source of
truth; raising one alone silently does nothing:

1. **`open-council.sh` hardcodes `ROSTER=["chair","rev1","rev2"]`** — it does NOT
   read the plane's live roster. Updating the plane's runtime standing roster
   does not change who the manual driver invites. You **must** pass the
   full roster explicitly: `ROSTER='["chair","rev1","rev2","rev3","rev4"]'
   open-council.sh …`. Miss this and you silently get a 2-reviewer, quorum-2
   council no matter how many pods are up.
2. **`dev-deploy-bots.sh` takes the bot list from `--bots`, not `--bot-agents`**
   (default `chair,rev1,rev2`). Passing only `--bot-agents rev3=…,rev4=…` deploys
   nothing new — you must also pass `--bots rev3,rev4` (or the full list).
3. **`dev-deploy-k8s.sh` does NOT forward `OABCP_COUNCIL_PRESET`**. To set a
   webhook-path preset, apply it separately:
   `kubectl -n oabcp-local set env deploy/control-plane
   OABCP_COUNCIL_PRESET=standard`. For the manual dogfood path, use
   `open-council.sh --preset` instead — the plane env is irrelevant there.

## Baseline (as shipped locally)

- Plane `dev-deploy-k8s.sh`: `OABCP_BOTS=chair:chair,rev1:reviewer,rev2:reviewer`,
  `OABCP_COUNCIL_ROSTER=chair,rev1,rev2`, no preset (webhook path → `lite`).
  `OABCP_BOTS` only seeds the first empty DB; day-2 add/remove uses the APIs.
- Bots `dev-deploy-bots.sh`: 3 pods, Kiro (`?agent=kiro`, `KIRO_API_KEY`).
- Gateway tokens: legacy auto-mint (plane generates per bot, served inline).

## Redeploy behaviour (durable DB since C9)

`dev-deploy-k8s.sh` mounts a `control-plane-data` PVC at `/data` and points
`OABCP_DB=/data/plane.db` there, so the SQLite DB **survives plane pod swaps**.
A plane redeploy or `kubectl set env deploy/control-plane …` no longer wipes the
DB or re-mints tokens → **existing bots keep their tokens and do NOT need
restarting**. The deployment uses `strategy: Recreate` (the RWO PVC can't attach
to two pods at once, so a rolling swap would deadlock on the mount).

Verified 2026-07-05: bounced the plane with all 5 bots untouched — they
auto-reconnected (openab retries with backoff on a `Connection reset`) on their
persisted tokens, back to 5/5, zero `401`.

Still true after C9:
- The plane pod swap breaks `port-forward svc/control-plane` — restart it.
- A **fresh** bot added via step 3 still needs its own pod; only *existing* bots
  are spared the restart.
- **One-time cutover:** the first apply that *introduces* the PVC starts from a
  fresh empty `/data/plane.db`, so it re-mints tokens once — restart the existing
  bots that single time so they re-fetch `/bot-config`. Every plane restart after
  that is non-destructive.

**Roster shrink and identity retirement are API operations.** Remove the bot from
the standing roster first, stop its pod, then delete its identity and gateway
token:

```sh
curl -X PUT "$PLANE/v1/council/roster" \
  -H "Authorization: Bearer $KEY" \
  -H "Content-Type: application/json" \
  -d '{"roster":["chair","rev1","rev2","rev3"]}'
kubectl --context docker-desktop -n oabcp-local delete deploy rev4
curl -X DELETE "$PLANE/v1/bots/rev4" \
  -H "Authorization: Bearer $KEY"
```

`DELETE /v1/bots/:id` returns `409` if the bot is still rostered, connected, or
in an active session. That is intentional; stop inviting it, stop the pod, and
wait for active sessions to close.

To wipe everything (old destructive behaviour on demand): delete the PVC
(`kubectl -n oabcp-local delete pvc control-plane-data`) then redeploy.

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

# 1. Register stable bot ids for /bot-config/rev3 and /bot-config/rev4.
curl -X POST "$PLANE/v1/bots/discover" \
  -H "Authorization: Bearer $OABCP_BOT_DISCOVERY_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"id":"rev3","role":"reviewer","provider":"kiro","capabilities":["review"]}'
curl -X POST "$PLANE/v1/bots/discover" \
  -H "Authorization: Bearer $OABCP_BOT_DISCOVERY_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"id":"rev4","role":"reviewer","provider":"claude","capabilities":["review"]}'

# 2. Grow the webhook standing roster without restarting the plane.
curl -X PUT "$PLANE/v1/council/roster" \
  -H "Authorization: Bearer $KEY" \
  -H "Content-Type: application/json" \
  -d '{"roster":["chair","rev1","rev2","rev3","rev4"]}'

# 3. Deploy the new bot pods. --bots is the name list; --bot-agents the map.
scripts/dev-deploy-bots.sh \
  --bots rev3,rev4 \
  --bot-agents rev3=kiro,rev4=claude \
  --agent-secret kiro=kiro-api:KIRO_API_KEY \
  --agent-secret claude=claude-oauth:CLAUDE_CODE_OAUTH_TOKEN \
  --extra-secret gh-token:GH_TOKEN \
  --steering-file docs/steering/pr-review.md
```

The plane stays up throughout this flow. The existing chair/rev1/rev2 keep their
connections and tokens; only the new bot pods start. Since C8 (#100) a fresh pod
that briefly overlaps an old one self-heals too — no manual restart for the
double-connect case either (see the gotcha below).

## Scale DOWN

```sh
scripts/dev-deploy-bots.sh --replicas 0          # park all bots, plane untouched
kubectl --context docker-desktop -n oabcp-local delete deploy rev3 rev4  # drop pods
scripts/dev-deploy-bots.sh --delete              # remove all bot deployments
```

To stop convening a reviewer, drop it from the roster you pass (`ROSTER=` for the
manual driver, `PUT /v1/council/roster` for the webhook path) — deleting its pod
is only needed to reclaim resources. Use `DELETE /v1/bots/:id` only when you want
to retire the identity and old gateway token.

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
  single key saturates. For C5 capacity planning, treat ~4 reviewers per provider
  key as the ceiling until the provider quota story changes; adding more reviewers
  has already failed before the plane mechanisms did.
- **Double-connect on fresh-pod startup (C8 — FIXED #100).** A fresh pod that
  briefly overlaps the old one (two replicasets during a roll, or a scale-0→1
  dial) used to be able to win the hub slot then die and orphan the survivor →
  bot stuck `connected=false` with no self-heal. Since #100 the plane keeps a
  *stack* of live conns per bot; when the current one dies a surviving conn is
  promoted back to current, so the bot stays connected — no manual restart. openab
  also auto-reconnects with backoff (the original "no retry" was wrong). The plane
  still logs the fingerprint if you want to watch it happen: a `bot <id> second
  live connection gen N->M (overlap)` warn on the double-dial, then `connection
  closed (gen N); still live on another conn` when the loser drops. `invalid bot
  token` warns instead point at the stale-token path (not this race). If a bot
  *does* stick offline after those overlap lines, that's a regression worth a bug,
  not a `rollout restart`.
- **Liveness grace.** `OABCP_LIVENESS_GRACE_SECS=60`: a pod not connected within
  the window is flipped `unreachable` and trimmed. Big fleets cold-start slower —
  wait for all `connected=true` before opening.
