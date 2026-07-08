# ADR 008 — External controller protocol

Status: accepted-as-amended · 2026-07-09 (was proposed 2026-06-30) — ratified
at Stage 3 S1; amendments in [ADR 018](018-stage3-extraction.md): the
in-process ControllerAction structs are the v1 action vocabulary
(OpenSession/PostMessage implemented; AddRoster/CloseSession/EmitStatus
reserved with pinned serialized shapes); the external transport stays dormant
until a plugin needs independent deploy cadence or a third-party author appears

## Context

[ADR 007](007-control-plugins-and-oab-father.md) defines control plugins and OAB
Father at the product and management boundary. This ADR narrows the next
question: when a control plugin runs outside the OCP process, how does it
receive events and ask OCP to perform runtime actions?

The useful prior art is split across several systems:

- Telegram BotFather: human-facing bot identity, tokens, metadata, commands, and
  webhook setup.
- Kubernetes Operators and CRDs: an API server stores resources; controllers
  reconcile external systems.
- Slack and GitHub App manifests: installable extensions declare events,
  permissions, commands, and endpoints before they run.
- Argo Events: event sources, sensors, and triggers are separated instead of
  being hard-coded into one workflow.
- Temporal: long-running work needs durable state, retry, replay, and explicit
  side-effect ownership.
- HashiCorp go-plugin: process isolation is safer than loading untrusted code
  in-process.
- MCP: explicit tool/resource discovery is a useful model for capability
  declaration, even though MCP is not itself the OCP plugin protocol.

## Decision

The first stable extension boundary should be an **external HTTP controller**,
not an in-process Rust plugin ABI.

```text
GitHub webhook / API / cron / human trigger
        |
        v
      OCP core
        |
        | signed normalized event
        v
 external controller service
        |
        | declarative action request
        v
      OCP core
        |
        v
 sessions / roster / messages / OpenAB pods
```

OCP owns validation, state changes, liveness, idempotency, and audit. The
controller owns policy: which session to open, which roster to choose, which
prompt to send, and which side effects it expects pods or controller-owned tools
to perform.

## Event Delivery

OCP sends normalized events to a controller endpoint. Raw provider webhooks may
be retained for audit/debug, but the v1 controller contract is not "parse
GitHub/Slack/webhook payloads yourself."

OCP must reject controller installations whose endpoint URI scheme is not
`https`.

Every outbound event includes:

```text
X-OAB-Controller-ID: <controller_id>
X-OAB-Event-ID: <event_id>
X-OAB-Timestamp: <unix_seconds>
X-OAB-Signature: sha256=<hex>
```

The v1 canonical string is:

```text
v1
<controller_id>
<event_id>
<timestamp>
<method>
<request_target>
<sha256_hex(body)>
```

`method` is uppercase ASCII. `request_target` is the exact path plus optional
query string sent by OCP, already percent-encoded, with no decoding, sorting, or
host/scheme normalization. OCP sends uncompressed JSON. `body` is the exact
request body bytes, and `sha256_hex` is lowercase hex.

OCP computes `HMAC-SHA256(canonical_string, event_signing_secret)`, emits
lowercase hex, and controllers compare in constant time. Controllers reject
timestamps outside a 5-minute skew window and retain seen event ids for at least
10 minutes to reject replay through the full delivery window.

Delivery is durable and bounded:

- OCP stores the normalized event before dispatch.
- The v1 request timeout target is 10 seconds.
- The retry budget is initial attempt plus 3 retries, with 10s, 30s, and 90s
  backoff.
- The total delivery window is 5 minutes.
- Exhaustion moves the event to a dead-letter state and emits an
  operator-visible audit event.

Dead-lettering an opening event must not create a half-open session. Once a
session exists, OCP's existing liveness guarantees still apply: bots may continue
posting, the watchdog may force-close the session, and OAB Father should expose
manual retry or close controls for operators.

## Action Requests

Controllers call OCP with declarative action requests. They never receive a DB
handle or root API key.

Every action request presents an install-scoped action token:

```text
Authorization: Bearer <token>
X-OAB-Action-ID: <action_id>
```

The token is 256 bits of random entropy, base64url-encoded. OCP stores only
`HMAC-SHA256(token, server_pepper_vN)`, checks with constant-time comparison,
and stores the pepper key version next to the token hash. The pepper is a
deployment secret managed by the platform secret store. Pepper rotation is
key-versioned: old hashes remain verifiable until their tokens are rotated or
revoked.

OAB Father rotates action tokens with an overlap window. The v1 default overlap
is 15 minutes, during which both old and new tokens are accepted. If an action
token is suspected stolen, immediate revocation is the compensating control;
OCP also rate-limits accepted actions per install so unusual action volume is
visible and bounded.

OCP persists `(controller_id, action_id)` for at least 24 hours and treats
duplicates as idempotent replays. For `open_session`, the domain dedupe key is
`(controller_id, trigger_ref)`. Two controllers may legitimately open separate
sessions for the same provider object, such as a PR review controller and a
release-gate controller both reacting to the same PR.

If a request has a new `action_id` but a duplicate `(controller_id,
trigger_ref)`, the domain dedupe key wins and OCP returns the existing session
id.

## V1 Actions

The v1 action surface is:

| Action | Meaning |
|---|---|
| `open_session` | Create or dedupe a session for a stable trigger ref |
| `post_message` | Append a controller-authored message to an existing session |
| `add_roster` | Add bots to an existing session |
| `close_session` | Request a controlled close when policy allows it |
| `emit_status` | Record an operator-visible controller status update, optionally mapped later to provider status/check surfaces |

`add_roster` is additive only in v1. Added bots must receive session history
through the normal backfill path so late joins do not lose context. `remove_roster`
is deliberately excluded from v1 because it changes liveness and quorum
semantics.

Example `open_session`:

```json
{
  "action": "open_session",
  "trigger_ref": "github:pr/canyugs/openab-control-plane/123",
  "mode": "council",
  "chair_bot": "bot-chair-01",
  "roster": ["bot-chair-01", "bot-reviewer-01"],
  "quorum_n": 1,
  "prompt": "Review this PR from the assigned angles..."
}
```

Names in action bodies are bot identities, not roles. `"chair_bot":
"bot-chair-01"` refers to the registered bot id/name `bot-chair-01`; it does
not mean "any bot with role=chair."

`trigger_ref` is a stable idempotency URI, not a transport URL. The v1 GitHub PR
form is `github:pr/<owner>/<repo>/<number>`. Provider-specific forms must be
documented before a controller can depend on them; create
`docs/trigger-ref-schemes.md` alongside the first external controller
implementation.

## Manifest And Grants

A controller installation declares the events it consumes, actions it may call,
side effects it expects, and scopes where it may operate.

```yaml
apiVersion: openab-control-plane/v1
kind: Controller
metadata:
  name: pr-review
spec:
  endpoint: https://controller.example.com/oab
  events:
    - github.pull_request.opened
    - github.issue_comment.created
  actions:
    - open_session
    - post_message
    - add_roster
    - close_session
    - emit_status
  sideEffects:
    github:
      - pull_request.comment
  auth:
    eventSigning: hmac-sha256
    actionAuth: bearer-token
  scopes:
    repos:
      - canyugs/openab-control-plane
```

The manifest is both operator UX and enforcement input. It should be versioned
and reviewable, like a GitHub or Slack App manifest.

`sideEffects` are declarations for connector writes performed by the controller
or the pods it steers. They are not OCP-executed connector operations. OCP
enforces the declaration at boundaries it controls: it refuses to bind a
controller to trigger types that require undeclared side effects, rejects actions
outside manifest scope, and records declared side effects in audit logs. OCP does
not prove the controller posted exactly what it declared.

Within `openab-control-plane/v1`, manifest additions must be backward
compatible. Breaking changes require a new version. The previous manifest
version is supported for at least 90 days after the new version ships, regardless
of how many later versions have shipped. Any manifest update that expands
events, actions, side effects, scopes, quotas, endpoint, or auth material is
staged until an operator re-approves it through OAB Father. Sessions opened under
the old manifest complete under the old grants.

Wildcard repo scopes are not supported in v1. Org-wide installs should be
modeled as explicit repo bindings until the scope language is designed.

## Quotas And Storage

Initial quotas are install-scoped and enforced by OCP:

- max concurrent sessions;
- accepted action rate;
- event retry budget;
- optional per-repo or per-session caps.

Defaults before a quota UI exists: 5 concurrent sessions, 60 accepted actions per
minute, and the event delivery retry budget defined above.

Exceeding a quota is a hard reject. OCP returns `429` with `Retry-After` when a
retry can help, otherwise `409`.

```json
{
  "error": "quota_exceeded",
  "kind": "concurrent_sessions",
  "limit": 5,
  "current": 5,
  "reset_at": null
}
```

Quota checks and writes must be atomic. SQLite's serialized write transaction is
acceptable for the initial backend, but the store trait must preserve the same
property if another backend is added.

Controller registrations start in typed tables, not an untyped CRD blob table:

- `controllers`
- `controller_bindings`
- `controller_events`
- `controller_action_idempotency`

## Bundled Controllers

The current PR-review path should become the bundled `pr-review` controller over
time:

- GitHub events are normalized into OCP events.
- The controller chooses preset, roster, quorum, prompt, and follow-up shape.
- OCP opens sessions, relays messages, enforces liveness, and records audit.
- GitHub posting remains a pod/controller side effect governed by explicit
  capability grants.

This is a refactoring direction, not an immediate requirement to split the
binary. First-party bundled controllers may live in this repo until the external
action API exists, but they must still route through the same action interpreter
as external controllers. They must not bypass validation by writing directly to
the store.

## Consequences

- Controller authors can build policy without database access, Zeabur
  credentials, GitHub App keys, or unrestricted north API access.
- OCP remains the runtime kernel: it validates actions, enforces quotas and
  liveness, and records audit.
- OAB Father has a concrete management surface: install manifests, credentials,
  endpoint health, dead-letter replay, token rotation, and grant review.
- PR review can migrate incrementally from hardcoded council path to bundled
  controller without changing OCP core semantics.

## Deferred

- Proactive controller health checks and auto-suspension after repeated
  dead-letters.
- Whether action-token theft should get a stronger v2 control such as mTLS,
  short-lived JWTs, or source CIDR binding.
- The exact JSON schema for every action request and response.
- The first `docs/trigger-ref-schemes.md`.
- A controller conformance test harness.

## Rejected Alternatives

- **In-process plugins first.** This increases blast radius and introduces ABI
  stability concerns before the product boundary is proven.
- **Raw webhook passthrough.** It makes every controller reimplement provider
  parsing and permission quirks.
- **Controller direct store writes.** This bypasses OCP's central validation,
  quota, liveness, and audit guarantees.
