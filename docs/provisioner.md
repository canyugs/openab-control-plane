# Fleet provisioner (external)

Membership inc3. When the chair recruits a bot **type that has no running pod
yet**, OCP can't add it (the admission gate requires a registered bot). Instead
of failing silently, OCP emits a `provision_requested` signal for an **external
fleet provisioner** to act on.

## Why this lives outside OCP

[ADR 001](adr/001-three-planes.md) puts the fleet/provisioner in its own trust
domain, off the coordination hot path:

- **Trust domain** — spinning up a pod needs the **Zeabur API token**. That
  credential must not live in the coordination plane.
- **Hot path** — a deploy takes tens of seconds to minutes; the council's
  request/reply loop can't block on it.
- **"Pipe, not container"** — OCP coordinates; it does not own pod lifecycle (that
  is OpenAB / the platform). OCP only *signals*; the provisioner *acts*.

So OCP's entire inc3 surface is one event. The provisioner is a separate
deployable (not in this repo's core) that holds the Zeabur credential.

## The signal

Emitted on the north SSE stream (`GET /v1/sessions/:id/stream`) when an
**authorized** recruit (chair-only) targets an **unregistered** bot:

```json
{ "type": "provision_requested", "session_id": "ses_…",
  "payload": { "by": "<chair bot id>", "target": "<requested bot id/type>" } }
```

Sibling events: `recruit` (added), `recruit_rejected` (e.g. roster full —
provisioning wouldn't help), `recruit_denied` (not authorized to recruit).

## The provisioner loop

```
subscribe SSE  →  on `provision_requested { target }`:
  1. VALIDATE  target against your catalog of known bot types.
               Unknown/garbage (a chair typo) → log and ignore. OCP stays dumb;
               the policy of "what may be provisioned" is the provisioner's.
  2. DEPLOY    a pod for `target` (Zeabur API), wired to this plane:
               OABCP_WS_URL + the bot's /bot-config, exactly like a seeded pod.
  3. REGISTER  the bot with OCP — seed it (`OABCP_BOTS`-style) or `POST /v1/bots`
               — so it passes the admission gate.
  4. ADMIT     `POST /v1/sessions/:id/roster {"bot_id": target}` (the same gate),
               OR signal the chair to re-issue `[[recruit:target]]` now that it
               is registered. Either way the existing gate + backfill apply.
```

Idempotency: if two `provision_requested` events arrive for the same target,
step 1's catalog check plus step 3's idempotent registration make a double-deploy
a no-op — dedupe on `target` if your deploy API isn't idempotent.

## Status

OCP side (the signal + the contract) is done. A reference provisioner — the
Zeabur-API service that consumes it — is a separate component, built with real
deploy credentials and live-tested against Zeabur; intentionally not part of the
coordination plane. Today, recruit targets an already-registered bot; the
provisioner closes the loop for not-yet-running types.
