# ADR 009 — Quota failover through bot discovery and explicit admission

Status: accepted · 2026-07-01

## Context

The immediate operational problem is quota exhaustion. In dogfood, a PR review
can be correctly triggered by GitHub webhook and still fail to produce a verdict
because the model CLI returns a provider quota error, such as a Claude weekly
limit. When that happens, the operator wants to answer a concrete question:

> Which bot can replace the exhausted one right now?

Today's static model makes that answer manual:

- `OABCP_BOTS` seeds identities at boot.
- `OABCP_COUNCIL_ROSTER` decides the standing webhook council.
- Each Zeabur bot service independently chooses its image, `/bot-config` URL,
  provider CLI, credentials, and volume.

That is safe, but weak for failover. It does not expose an inventory of live
alternative bots, their providers, their capabilities, or whether they are stale,
disabled, quota-limited, or ready to replace a failed bot.

ADR 001 identifies this as the membership plane. ADR 005 already chose
roster-swap as the primary cost/model governance mechanism: model choice is
mostly a property of which pod is in the roster, not a model field inside the
session wire. This ADR narrows that direction for quota failover.

This ADR also intentionally scopes v1 to a **single operational group per OCP
deployment**. A group is one trust boundary: one bot inventory, one standing
council policy, one set of install credentials, and one operator/controller
surface. A single group may cover multiple repositories through
`OABCP_ALLOWED_REPOS`, but it is still one trust domain. If two teams or products
need isolated bot inventories, credentials, or admission policy, they should run
separate OCP deployments.

## Decision

OCP should support **bot discovery as inventory for replacement**, but
**admission remains explicit and policy-gated**.

For v1, discovery is **single-group scoped**. The inventory API should not carry
`group_id`, tenant routing, or cross-group scheduling semantics. The OCP
deployment itself is the group boundary.

Discovery answers:

- which bot pods exist;
- which provider/CLI they run;
- which role and capabilities they claim;
- whether they are connected, stale, disabled, or degraded;
- which replacement candidates are available when a bot hits quota.

Discovery does **not** mean a pod may add itself to the standing council or to an
active session. Admission is still controlled by one of:

- `OABCP_COUNCIL_ROSTER` for standing webhook reviews;
- an authenticated north/controller action for a specific session;
- an operator/OAB Father action;
- the existing recruit/admission gate, including the external provisioner seam.

The core safety rule is:

> Discovered is not rostered. Connected is not trusted. Self-reported capability
> is a hint, not an authorization grant.

## Current Implementation Check

As of 2026-07-01, OCP exposes these bot inventory primitives:

- `POST /v1/bots` registers an operator-created bot identity and returns its
  token.
- `GET /v1/bots` returns bot inventory, metadata, connection state, and current
  standing-roster membership.
- `POST /v1/bots/discover` registers or refreshes a discovered bot when
  `OABCP_BOT_DISCOVERY_TOKEN` is configured.
- `PATCH /v1/bots/:id` updates operator metadata such as `enabled`, `health`,
  `note`, `provider`, `capabilities`, `version`, and `runtime`.

Automatic quota detection and automatic replacement are still outside OCP v1.
Replacement remains an explicit roster action.

## Replacement Model

When a bot's provider quota is exhausted, the replacement flow should be:

1. Mark or observe the current bot as degraded, for example
   `health=quota_exhausted` or via logs/operator input.
2. Query bot inventory for candidates with compatible role and capability.
3. Prefer a candidate that is already connected and not degraded.
4. Explicitly update the standing roster or session roster.
5. Keep GitHub write-back constraints intact:
   - replacing a reviewer does not grant write credentials;
   - replacing the chair requires a chair-capable candidate with PR write-back
     configured and verified.

This keeps the operational goal fast: when Claude quota is gone, the operator can
find `codex` or `gemini` candidates and swap roster membership without editing a
database or guessing from service names.

## V1 Inventory Shape

The north API exposes bot inventory:

```text
GET /v1/bots
Authorization: Bearer <north-api-key>
```

Optional query filters:

| Query | Meaning |
|---|---|
| `role=reviewer` | Return only bots with this role |
| `provider=codex` | Return only bots that report this provider |
| `capability=review` | Return only bots that report this capability hint |
| `connected=true` | Return only bots whose connected state matches the boolean |
| `enabled=true` | Return only bots whose operator-enabled state matches the boolean |
| `health=ok` | Return only bots with this health value |

The response should include both the standing roster and the inventory, because
failover decisions need to compare "what is currently used" with "what could
replace it":

```json
{
  "standing_roster": ["chair", "rev1", "rev2"],
  "bots": [
    {
      "id": "rev1",
      "role": "reviewer",
      "provider": "claude",
      "capabilities": ["review", "gh-read"],
      "connected": true,
      "enabled": true,
      "health": "ok",
      "rostered": true,
      "chair": false,
      "last_seen_ms": 1782810000000,
      "source": "seeded",
      "runtime": {
        "kind": "kubernetes",
        "namespace": "openab",
        "workload": "deployment/rev1",
        "pod": "rev1-6f4d9b8c7d-x2abc"
      }
    },
    {
      "id": "rev-codex-1",
      "role": "reviewer",
      "provider": "codex",
      "capabilities": ["review", "gh-read"],
      "connected": true,
      "enabled": true,
      "health": "ok",
      "rostered": false,
      "chair": false,
      "last_seen_ms": 1782810000000,
      "source": "discovered",
      "runtime": {
        "kind": "kubernetes",
        "namespace": "openab",
        "workload": "deployment/rev-codex-1"
      }
    }
  ]
}
```

Fields:

| Field | Meaning |
|---|---|
| `id` | Bot identity used in rosters and messages |
| `role` | `chair`, `reviewer`, or future role |
| `provider` | `claude`, `codex`, `gemini`, etc. |
| `capabilities` | Inventory hints such as `review`, `gh-read`, `gh-write` |
| `connected` | WebSocket presence |
| `enabled` | Operator/controller admission eligibility |
| `health` | `ok`, `quota_exhausted`, `auth_failed`, `stale`, `disabled`, etc. |
| `rostered` | Whether this bot is in the standing roster right now |
| `chair` | Whether this bot is currently the standing chair |
| `last_seen_ms` | Last connection or heartbeat timestamp |
| `source` | `seeded`, `registered`, `discovered`, or `provisioned` |
| `runtime` | Optional platform hint for debugging/provisioners |

`capabilities` are not permissions. They help choose candidates, but OCP must
still enforce admission and role constraints.

There is intentionally no `group_id` field in the v1 inventory response. All
listed bots belong to the same OCP deployment/group. Multi-group inventory is a
future OAB Father or platform concern, not an OCP v1 discovery feature.

The `runtime` object is also not an authority boundary. It lets a k3s/OpenAB
operator or Zeabur provisioner correlate a bot with a Kubernetes workload, but
OCP should not require Kubernetes access to serve inventory.

## Discovery / Registration

The current `POST /v1/bots` is an operator-facing registration primitive. A
discovery path should be narrower than the root north API key and scoped to this
OCP deployment's single group.

The bootstrap endpoint is:

```text
POST /v1/bots/discover
Authorization: Bearer <OABCP_BOT_DISCOVERY_TOKEN>
```

If `OABCP_BOT_DISCOVERY_TOKEN` is unset, discovery registration is disabled. The
token is install/group scoped. It can register or refresh bot metadata, but it
cannot open sessions, edit rosters, or grant GitHub write credentials.

V1 flow:

1. A bot service starts with an install-scoped bootstrap credential.
2. The service calls a discovery endpoint with identity and metadata:

   ```json
   {
     "id": "rev-codex-1",
     "role": "reviewer",
     "provider": "codex",
     "capabilities": ["review", "gh-read"],
     "version": "openab:0.9.0-beta.3",
     "runtime": {
       "kind": "kubernetes",
       "namespace": "openab",
       "workload": "deployment/rev-codex-1",
       "pod": "rev-codex-1-7c9d88b6f7-h2k9m"
     }
   }
   ```

3. OCP validates the bootstrap credential and allowlist.
4. OCP registers or refreshes metadata and returns the bot config URL.
5. The bot connects to `/ws` with its bot token.

Refreshing an existing bot does not overwrite its stored `name` or `role`; a pod
cannot rename itself or promote itself from reviewer to chair by changing its
discovery payload. Omitted discovery metadata fields are preserved on refresh;
nullable metadata is cleared through the north `PATCH /v1/bots/:id` endpoint.
The returned config URL is generated from the saved provider metadata after the
refresh, so partial refreshes keep returning the provider-specific config URL.

Example response:

```json
{
  "bot_id": "rev-codex-1",
  "config_url": "http://control-plane.zeabur.internal:8090/bot-config/rev-codex-1?agent=codex"
}
```

The bootstrap credential is only for registration/refresh. It is not the north
root API key and cannot open sessions, change rosters, or grant GitHub write
credentials.

The registration request does not choose a group. The group is implied by the OCP
deployment and bootstrap credential used.

## Operator Metadata Updates

Provider quota state may be observed by an operator, a log watcher, or a human.
OCP should support a narrow operator metadata update:

```text
PATCH /v1/bots/:id
Authorization: Bearer <north-api-key>
```

Allowed fields:

```json
{
  "enabled": false,
  "health": "quota_exhausted",
  "note": "Claude weekly quota reset at 2026-07-01T15:00:00Z"
}
```

This endpoint only changes inventory metadata. It does not remove the bot from
the standing roster, add a replacement, or close active sessions. Replacement
still goes through explicit admission. Nullable metadata fields such as
`provider`, `note`, `version`, and `runtime` can be cleared by sending JSON
`null`.

## Admission Policy

Admission is separate from discovery:

- Standing council admission is still `OABCP_COUNCIL_ROSTER` in the current
  product.
- Session admission is still the authenticated `open_session`/`add_roster`
  action path.
- Chair status is explicit: the chair is first in `OABCP_COUNCIL_ROSTER` or the
  `chair_bot` in a session action.
- A discovered chair candidate must not be selected automatically only because
  it advertises `role=chair` or `gh-write`.
- Offline/stale/degraded bots remain visible in inventory but should not be
  auto-selected.
- Cross-group admission is not supported in v1 because one OCP deployment owns
  exactly one group.

Later, OAB Father or an external controller may implement policy such as:

```text
replace rev1 with any connected reviewer where provider != claude and health == ok
```

That controller still calls OCP through authenticated action APIs. It does not
receive a database handle or bypass admission gates.

## Interaction With Provisioning

Discovery and provisioning are complementary:

- Discovery says what exists now.
- Provisioning creates a missing pod.
- Admission decides whether that pod joins a roster.

If no suitable replacement exists, the existing `provision_requested` seam can
ask an external provisioner to create one. The provisioner may then register or
discover the new bot, after which an operator/controller can admit it.

OCP still does not hold Zeabur API credentials. Pod creation remains outside the
coordination hot path.

## Security

- Discovery requires a scoped bootstrap credential.
- Bootstrap credentials are install/group scoped and revocable.
- Self-reported provider/capability metadata is inventory, not authorization.
- Self-reported role only applies when a new discovered identity is created; it
  does not override existing identities.
- Reviewers never receive GitHub write-back credentials through discovery.
- Chair write-back credentials remain pod-local and must be verified separately.
- A compromised pod cannot add itself to `OABCP_COUNCIL_ROSTER`.
- Provider quota/failure metadata should not leak API keys, subscription ids, or
  provider account details.

## Non-Goals

- Multi-tenant bot registry inside one OCP deployment.
- Cross-group bot scheduling or shared bot inventory.
- Automatically adding every connected bot to reviews.
- Sharing bot disks or CLI caches across services.
- Moving Zeabur provisioning credentials into OCP.
- Solving provider-specific model selection inside a live CLI session.
- Replacing OAB Father or external controllers as the operator UX layer.

## Consequences

- Operators get a fast answer when quota is exhausted: list candidates, pick a
  replacement, and update roster.
- Operators that need separate trust domains deploy separate OCP instances rather
  than multiplexing groups inside one OCP.
- OCP now stores bot metadata beyond the original static identity fields.
- The north API exposes inventory before admission, so controllers can inspect
  candidates without mutating rosters.
- Template UX can later expose provider profiles and standby replacements
  without changing the OCP session wire.
- The failover path remains auditable because replacement is an explicit roster
  action, not an implicit side effect of a pod connecting.
