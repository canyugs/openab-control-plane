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

**Roster shrink needs an explicit delete.** Because seeding is `INSERT OR IGNORE`
against the now-durable DB, dropping a name from `OABCP_BOTS` does NOT remove its
row — the bot stays seeded and connectable. To actually retire a reviewer, delete
its row (and its pod), don't just shorten the env:

```sh
kubectl --context docker-desktop -n oabcp-local exec deploy/control-plane -- \
  sqlite3 /data/plane.db "DELETE FROM bots WHERE id='rev4';"
kubectl --context docker-desktop -n oabcp-local delete deploy rev4
```

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
```

Since C9 the DB is durable, so step 1 does **not** re-mint tokens — the existing
chair/rev1/rev2 keep their `/bot-config` and stay connected; no restart needed
(pre-C9 this was a mandatory 4th step). If a *fresh* pod sticks at
`connected=false` on first boot, restart just that deploy (the double-connect
race — see the gotcha below).

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
- **Double-connect on fresh-pod startup (C8 — could not reproduce post-C9).** A
  fresh pod was once seen opening two WS with the survivor dropping ~0.5s later
  and sticking `connected=false`. Re-tested 2026-07-05 across 16 fresh-pod events
  (8 rolling + 8 scale-0→1): every one a clean single connection. openab in fact
  auto-reconnects with backoff (the original "no retry" was wrong), and the
  practical trigger was the ephemeral-DB token churn that C9 removed. If it
  recurs, the plane now logs the fingerprint: a `re-registered gen N->M (displaced
  a live connection)` warn and/or a `superseded connection closed (gen N)` line —
  a bot stuck offline after those, with no later `connected`, is the C8 race;
  `invalid bot token` warns instead point at the stale-token path. Workaround
  either way: `rollout restart` that one deploy.
- **Liveness grace.** `OABCP_LIVENESS_GRACE_SECS=60`: a pod not connected within
  the window is flipped `unreachable` and trimmed. Big fleets cold-start slower —
  wait for all `connected=true` before opening.
