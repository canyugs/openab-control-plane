# ADR 007 — OAB Father and external controllers

Status: proposed · 2026-06-29

## Context

OCP started as a PR-review council because code review was the fastest way to prove
the core guarantee: multiple OAB pods can be coordinated through one durable control
plane, with identity, routing, quorum, and a closing verdict. That first app is useful,
but it should not define the product boundary.

The broader opportunity is a "control layer runtime" for OAB: teams should be able to
insert their own policy/controller between triggers and bot sessions without forking
OCP or giving an extension root access to the store, deployment credentials, or every
connector token.

The closest prior art is split across several systems:

- **Telegram BotFather** ([Bot API](https://core.telegram.org/bots/api),
  [tutorial](https://core.telegram.org/bots/tutorial)): a human-facing factory for bot
  identity, tokens, profile metadata, commands, and webhook setup.
- **Kubernetes Operators / CRDs**
  ([operator pattern](https://kubernetes.io/docs/concepts/extend-kubernetes/operator/)):
  an API server stores desired/observed state; controllers reconcile external systems.
- **Slack / GitHub App manifests**
  ([Slack manifests](https://docs.slack.dev/app-manifests/configuring-apps-with-app-manifests),
  [GitHub App manifests](https://docs.github.com/en/apps/sharing-github-apps/registering-a-github-app-from-a-manifest)):
  an installable extension declares events, permissions, commands, and endpoints.
- **Argo Events** ([docs](https://argoproj.github.io/argo-events/)): event sources,
  sensors, and triggers are separated so event ingestion is not hard-coded to one
  workflow.
- **Temporal** ([workflow execution](https://docs.temporal.io/workflow-execution)):
  long-running workflows need durable state, replay, retries, and clear ownership of
  side effects.
- **HashiCorp go-plugin** ([repo](https://github.com/hashicorp/go-plugin)): process
  isolation is a safer default than in-process untrusted plugins.
- **MCP** ([docs](https://modelcontextprotocol.io/docs/getting-started/intro)):
  explicit discovery of tools/resources is a useful model for what a controller can
  offer or consume, even though MCP is an agent tool protocol rather than a control
  plane extension model.

## Decision

Define a future **OAB Father** layer above OCP core, and make PR review the first
bundled controller instead of a special app path.

### 1. OCP core owns runtime guarantees

OCP core remains the small, security-sensitive runtime:

- bot identity and token issuance;
- session state, messages, reactions, outbox, and SSE;
- routing, fanout, quorum, and close semantics;
- admission, authorization checks, quotas, and audit events;
- capability validation before any side effect is executed.

Core should expose primitives. It should not accumulate one-off product workflows for
every trigger source, SaaS integration, or review style.

### 2. OAB Father owns management and installation

OAB Father is the management plane for humans and operators:

- create bot identities and rotate/revoke tokens;
- install controller plugins and bind them to repos, projects, commands, or webhooks;
- store plugin manifests, capabilities, secrets, and ownership;
- provision OAB pods through adapters such as Zeabur;
- show runs, sessions, audit events, and controller health.

This can begin as API/CLI. It does not need to start as a chat interface, even though
BotFather is the naming inspiration.

### 3. Controllers are external by default

The first stable extension boundary should be an **external HTTP controller**:

```text
GitHub webhook / API / cron / human trigger
        ↓
      OCP core
        ↓ signed event
 external controller service
        ↓ declarative action
      OCP core
        ↓
 sessions / messages / OAB pods / side effects
```

Use signed events and declarative actions instead of in-process Rust plugins. This keeps
untrusted or fast-moving policy outside the coordination hot path, while keeping final
validation and audit inside OCP.

Controller installation creates two direction-specific credentials:

- an **event signing secret** used by OCP to sign outbound controller events;
- an **action token** used by the controller when it calls OCP's action API.

Every outbound event includes:

- `X-OAB-Controller-ID`
- `X-OAB-Event-ID`
- `X-OAB-Timestamp` (Unix seconds)
- `X-OAB-Signature: sha256=<hex>`

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

`method` is uppercase ASCII. `request_target` is the exact path plus optional query
string sent by OCP, already percent-encoded, with no percent-decoding, query sorting, or
host/scheme normalization. OCP sends uncompressed JSON; `body` is the exact request body
bytes, and `sha256_hex` is lowercase hex. OCP computes
`HMAC-SHA256(canonical_string, event_signing_secret)`, emits lowercase hex, and
controllers compare in constant time. Controllers reject timestamps outside a 5-minute
skew window and reject replayed event ids within their retention window. The secret is
per controller installation, never global.

Every inbound action request presents the install-scoped action token as
`Authorization: Bearer <token>` and includes `X-OAB-Action-ID`. The token is 256 bits
of random entropy, base64url-encoded. OCP stores only
`HMAC-SHA256(token, server_pepper_vN)`, checks with constant-time comparison, and stores
the pepper key version next to the token hash. HMAC is chosen because these are
machine-generated high-entropy tokens that need indexed lookup; the pepper is a
deployment secret managed through the platform secret store. Pepper rotation is
key-versioned: old token hashes remain verifiable until their tokens are rotated or
revoked. OAB Father rotates action tokens with an overlap window (v1 default: 15
minutes) where both old and new tokens are accepted so in-flight actions do not fail.
OCP checks the token against the installed manifest and records the controller id on
every action and audit event.

OCP also persists `(controller_id, action_id)` for at least 24 hours and treats
duplicate action ids as idempotent replays. For `open_session`, `trigger_ref` remains
the domain-level dedupe key, but it does not replace the transport-level
`X-OAB-Action-ID`. If a request has a new `action_id` but a duplicate `trigger_ref`,
`trigger_ref` wins and OCP returns the existing session id.

Controller delivery is durable and bounded. OCP stores the normalized trigger event
before dispatch, then calls the controller with a short timeout (v1 target: 10 seconds).
The v1 retry budget is initial attempt + 3 retries, with 10s / 30s / 90s backoff and a
5-minute total delivery window. Exhaustion moves the event to a dead-letter state and
emits an operator-visible audit event; it does not create a half-open session. Once a
session exists, OCP's existing liveness and close guarantees still apply even if a later
controller callback fails: bots may still post messages through the session API, the
session watchdog can force-close with the transcript so far, and OAB Father should offer
a manual close/retry control for operators.

### 4. Controllers return desired actions, not direct store writes

A controller can propose actions such as:

```json
{
  "action": "open_session",
  "trigger_ref": "github:pr/canyugs/openab-control-plane/123",
  "roster": ["bot-chair-01", "bot-reviewer-01"],
  "chair_bot": "bot-chair-01",
  "mode": "council",
  "quorum_n": 1,
  "prompt": "Review this PR from the assigned angles..."
}
```

OCP validates the request against registered identities, quotas, capability grants, and
tenant/repo bindings before writing state. A controller never receives the DB handle or
the root API key.

The names in the action body are identities, not roles: `"chair_bot": "bot-chair-01"`
refers to the registered bot id/name `bot-chair-01`; it does not mean "any bot with
role=chair".

Initial quotas are install-scoped and enforced by OCP: max concurrent sessions, action
rate, event retry budget, and optional per-repo/session caps. Defaults before a quota UI
exists: 5 concurrent sessions, 60 accepted actions/minute, and the delivery retry budget
defined above. Exceeding a quota is a hard reject (`429` with `Retry-After` when a
retry can help, otherwise `409`) and a structured body containing quota name, limit,
current usage, and reset time. OCP does not queue actions beyond quota. OAB Father owns
the operator UX and usage view for these quotas; OCP owns enforcement.

`trigger_ref` is a stable idempotency URI, not a transport URL. The v1 GitHub PR form is
`github:pr/<owner>/<repo>/<number>`. Provider-specific forms must be documented before a
controller can depend on them.

Controller registrations start in typed SQLite tables (`controllers`,
`controller_bindings`, `controller_events`, and `controller_action_idempotency`). Do not
start with an untyped CRD blob table; schema changes can be migrated while the surface is
small.

### 5. Capability manifests are mandatory

A controller installation declares what it wants before it can run:

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

The manifest is both operator UX and enforcement input. It should be versioned and
reviewable, the same way app manifests make Slack/GitHub installs inspectable before
granting access.

The committed v1 action surface is exactly `open_session`, `post_message`,
`add_roster`, `close_session`, and `emit_status`. New core actions require a manifest
version addition or a backward-compatible v1 extension.

`sideEffects` are mandatory declarations for connector writes performed by the
controller or the pod it steers. They are not OCP-executed connector operations. OCP
enforces them at the boundaries it controls: it refuses to install or bind a controller
whose requested trigger workflow implies an undeclared side effect, rejects actions whose
repo/project scope is outside the manifest, and records the declared side effect in the
audit log. This is policy-level enforcement, not runtime observation of the external
connector call: OCP still does not hold the GitHub credential, cannot prove the
controller posted exactly what it declared, and does not post the comment itself.

Manifest compatibility follows Kubernetes-style group/version semantics without using
an unowned DNS domain in examples. Within `openab-control-plane/v1`, additions must be
backward-compatible. Breaking changes require a new version. OCP should support the
current and immediately previous manifest version during migration, with a minimum
90-day sunset window for the previous version. Any manifest update that expands events,
actions, side effects, scopes, quotas, endpoint, or auth material is staged until an
operator re-approves it through OAB Father. The old manifest remains active until
approval, and sessions opened under the old manifest complete under the old grants.

### 6. Controller events are normalized in v1

The v1 contract sends normalized OCP events, not raw third-party webhooks. For example,
GitHub PR open becomes `github.pull_request.opened` with repo, PR number, sender,
installation/repo binding, labels, and an opaque source event id.

Raw provider payloads may be retained for audit/debug or exposed later behind an
explicit escape hatch, but controller authors should not have to parse GitHub, Slack,
cron, and API payload shapes themselves in v1.

Controller uninstall disables new events and actions immediately, but sessions already
opened by that controller remain governed by their original manifest snapshot. They
continue until normal close, manual operator close, or the session watchdog.

### 7. PR review becomes the first bundled controller

The current `council.rs` path should be treated as the bundled `pr-review` controller:

- GitHub events are normalized into OCP events.
- The PR-review controller chooses preset, roster, quorum, trigger prompt, and
  follow-up shape.
- OCP opens sessions, relays messages, enforces liveness, and records audit events.
- GitHub posting remains a pod/controller side effect governed by explicit capability
  grants, not a hidden core privilege.

This is a refactoring direction, not an immediate requirement to split the binary.
First-party bundled controllers live in this repo until the external controller action
API is real. If OAB Father later moves to a separate management service, the bundled PR
review controller can move behind the same manifest/action contract without changing
OCP core semantics. Bundled controllers must still route through the same action
interpreter as external controllers; they should not bypass validation by writing
directly to the store.

## Consequences

- **OCP remains general.** Code review stays important, but it becomes one controller
  on top of the runtime rather than the runtime's identity.
- **The trust boundary is explicit.** Controllers can be written by other teams without
  receiving database access, Zeabur credentials, GitHub App keys, or unrestricted north
  API access.
- **The API shape becomes Kubernetes-like.** Core stores resources and enforces
  invariants; controllers reconcile from events into desired actions.
- **A controller manifest becomes part of review.** Installing a workflow is not "paste
  a webhook URL"; it is granting named events/actions/scopes.
- **OAB Father can start small.** The first version can be a CLI/API around bot and
  controller registration, capability grants, auth rotation, and audit visibility. A
  UI, chat interface, and provisioning adapters can follow later.
- **Runtime code should move toward interfaces.** Extracting `council.rs` behind a
  controller trait or action interpreter is the next architectural step.

## Non-goals

- Do not invent an in-process plugin ABI first. External process/HTTP isolation is the
  safer and easier MVP.
- Do not move all GitHub I/O into OCP core. ADR 004 still stands: connector credentials
  and PR writes should remain pod/controller concerns unless a specific capability is
  intentionally centralized.
- Do not make OAB Father responsible for agent reasoning. It manages identities,
  controllers, bindings, and provisioning; the OAB pods still run the agents.

## Open questions

- Should controller capabilities be granted per repo/project, per tenant, or both?
- How much of OAB Father belongs in this repo versus a separate management service once
  provisioning adapters arrive?
- What UI should operators use to inspect and replay dead-lettered controller events?
- Which controller metrics must become status checks or deployment health signals?
- Should OCP proactively health-check controllers, and should repeated dead-letters
  suspend a controller until an operator re-enables it?
