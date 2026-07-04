# Scale the Council (local dogfood)

Runbook for growing / shrinking the reviewer fleet on the `oabcp-local`
dogfood, with the goal of running a council large enough to **fully stand in
for PR review**, then scaling it down or up on demand.

Scope: local Kubernetes (`docker-desktop` context, `oabcp-local` namespace).
Production scaling is the same two knobs applied to Zeabur service env — see
[config-reference.md](config-reference.md) and [bot-operations.md](bot-operations.md).

## The two knobs (effective size = min of them)

Council size is governed by **two independent dials**, and the number of
reviewers that actually deliberate is the *smaller* of the two:

| Knob | Where | What it controls |
| --- | --- | --- |
| **A. Reviewer identities + pods** | `OABCP_BOTS` / `OABCP_COUNCIL_ROSTER` on the plane + one running pod per bot | How many reviewers *exist* and get convened |
| **B. Preset angles** | `OABCP_COUNCIL_PRESET` (`lite`=1, `quick`=3, `standard`=5, `full`=7) | How many reviewers a webhook-convened council *occupies* |

`assign_angles` (src/council.rs) round-robins angles onto reviewers:

- **angles ≤ reviewers** → the first N reviewers take one angle each; the rest
  are **trimmed** (sit out, quorum does not wait on them).
- **angles > reviewers** → every reviewer participates, some cover multiple angles.

So to run 5 reviewers on the **webhook / public-council path** you need *both*
5 reviewer pods **and** `OABCP_COUNCIL_PRESET=standard`. Setting only one wastes
the other. Quorum = participating reviewers (chair excluded).

> `open-council.sh` (the manual dogfood driver) is generic — with no `--preset`
> it convenes **all** roster reviewers, no trim. Knob B only bites the webhook
> path (`OABCP_COUNCIL_PRESET`) and `open-council.sh --preset`.

`OABCP_MAX_ROSTER` (default 16) caps the roster: chair + 15 reviewers. Raise it
before going past that.

## Baseline (as shipped locally)

- Plane: `dev-deploy-k8s.sh`, `OABCP_BOTS=chair:chair,rev1:reviewer,rev2:reviewer`,
  `OABCP_COUNCIL_ROSTER=chair,rev1,rev2`, no preset set → **defaults to `lite`**.
- Bots: `dev-deploy-bots.sh`, 3 pods (`chair`, `rev1`, `rev2`), Kiro agent
  (`?agent=kiro`, `KIRO_API_KEY` secret).
- Gateway tokens: **legacy auto-mint** (plane generates per bot, serves inline via
  `/bot-config/<name>`; no `OABCP_EXTERNALIZE_TOKENS` locally).

## ⚠️ Redeploy hazard — read before touching the plane

Changing any plane env (`OABCP_BOTS`, `OABCP_COUNCIL_ROSTER`,
`OABCP_COUNCIL_PRESET`) means **redeploying the plane**, which **wipes the
in-container SQLite DB and re-mints every bot's gateway token**. After a plane
redeploy you **must restart all bot pods** so they re-fetch `/bot-config` and
reconnect with the new token — otherwise they hang on a stale token.

Bot-pod-only changes (scale replicas, add/remove a pod without touching the
roster) do **not** wipe the plane. Only plane env edits do.

## Scale UP — add reviewers (target: full PR-review replacement)

Example: go from 2 → 5 reviewers (`rev3`, `rev4`, `rev5`), preset `standard` so
all five are occupied.

```sh
# 1. Redeploy the plane with the bigger roster + a matching preset.
#    (This wipes the DB and re-mints tokens — bots restart in step 3.)
OABCP_BOTS="chair:chair,rev1:reviewer,rev2:reviewer,rev3:reviewer,rev4:reviewer,rev5:reviewer" \
OABCP_COUNCIL_ROSTER="chair,rev1,rev2,rev3,rev4,rev5" \
OABCP_COUNCIL_PRESET="standard" \
  scripts/dev-deploy-k8s.sh

# 2. Deploy all bot pods (full map — the script reconciles to it).
scripts/dev-deploy-bots.sh \
  --bot-agents chair=kiro,rev1=kiro,rev2=kiro,rev3=kiro,rev4=kiro,rev5=kiro \
  --agent-secret kiro=kiro-api:KIRO_API_KEY \
  --extra-secret gh-token:GH_TOKEN \
  --steering-file docs/steering/pr-review.md

# 3. Restart bots so they re-fetch config after the plane's DB reset.
kubectl --context docker-desktop -n oabcp-local rollout restart deploy/chair deploy/rev1 deploy/rev2 deploy/rev3 deploy/rev4 deploy/rev5
```

The three names must stay aligned per bot: the `OABCP_BOTS` entry, the pod's
`/bot-config/<name>` fetch URL (set by `--bot-agents`), and the
`OABCP_COUNCIL_ROSTER` entry. Beyond 15 reviewers, also pass
`OABCP_MAX_ROSTER=<n>` to the plane in step 1.

## Scale DOWN — shrink or park the fleet

Pick by how permanent you want it:

```sh
# Park all bots (keep deployments, 0 replicas) — instant, reversible, plane untouched.
scripts/dev-deploy-bots.sh --replicas 0

# Drop specific reviewers from the standing council (no more convening):
#   redeploy the plane with the smaller roster, then restart remaining bots.
OABCP_BOTS="chair:chair,rev1:reviewer,rev2:reviewer" \
OABCP_COUNCIL_ROSTER="chair,rev1,rev2" \
OABCP_COUNCIL_PRESET="quick" \
  scripts/dev-deploy-k8s.sh
kubectl --context docker-desktop -n oabcp-local rollout restart deploy/chair deploy/rev1 deploy/rev2
# then remove the orphaned pods:
kubectl --context docker-desktop -n oabcp-local delete deploy rev3 rev4 rev5

# Nuke all bot deployments entirely.
scripts/dev-deploy-bots.sh --delete
```

Trimming the roster does **not** require deleting pods — a reviewer out of
`OABCP_COUNCIL_ROSTER` is simply never convened. Delete its pod only to reclaim
resources.

## Verify before a scale test

```sh
# All bots connected (health=ok, connected=1) before opening a council.
curl -s -H "Authorization: Bearer $KEY" "$PLANE/v1/bots" | jq '.[] | {id, role, connected, health}'
```

Then drive one review and confirm the participant count matches expectations:

```sh
PLANE=http://localhost:8090 KEY=local-test-key \
  scripts/open-council.sh --watch --self-fetch <owner/repo#pr>
```

The verdict trailer `[[verdict:approve r=N y=M g=K]]` and the plane's
`findings_*` columns confirm how many reviewers actually voted.

## Scale-test gotchas

- **Shared model quota.** Every pod here shares one provider key
  (`KIRO_API_KEY`). N reviewers self-fetching + generating at once will hit
  rate limits. For large councils, split keys or mix providers per bot
  (`--bot-agents rev3=claude,...` with that provider's secret).
- **Liveness grace.** `OABCP_LIVENESS_GRACE_SECS=60`: a pod that hasn't
  connected within the grace window is flipped `unreachable` and its quorum
  slot trimmed. Big fleets cold-start slower — wait for all `connected=1`
  before opening, or the first council shrinks itself.
- **Preset must match reviewer count on the webhook path.** More pods without a
  bigger preset just trims the extras. Effective size = min(preset angles,
  reviewers).
