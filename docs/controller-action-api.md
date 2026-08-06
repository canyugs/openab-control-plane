# Controller Action API

Status: P5 provider-neutral boundary. This API implements inbound actions and
durable signed runtime-event delivery from
[ADR 008](adr/008-external-controller-protocol.md).

## Configuration

Set all three variables on OCP:

```text
OABCP_API_KEY=<root operator bearer>
OABCP_CONTROLLER_ACTION_PEPPERS={"1":"<base64url-encoded 32+-byte secret>"}
OABCP_CONTROLLER_EVENT_SIGNING_KEYS={"1":"<base64url-encoded 32+-byte secret>"}
```

Pepper versions are positive integers. OCP stores only the HMAC-SHA256 of each
action token (using `pepper_vN` as the HMAC key) and the selected version. Keep
an old pepper configured until its tokens have been rotated or revoked.
Event signing keys are also versioned deployment secrets. OCP derives a unique
HMAC secret for each controller and stores only the selected key version.

## Install a controller

Installation management requires the root operator bearer and is fail-closed
when `OABCP_API_KEY` is unset.

```http
POST /v1/controller-installations
Authorization: Bearer <OABCP_API_KEY>
Content-Type: application/json

{
  "id": "example-controller",
  "actions": ["open_session", "post_message", "add_roster", "close_session", "emit_status"],
  "scopes": ["tenant:example/resource:demo"],
  "max_concurrent_sessions": 5,
  "max_actions_per_minute": 60
}
```

Scopes are exact, provider-neutral strings. Wildcards are rejected in v1. The
response returns a 256-bit base64url action token exactly once; persist it in
the controller's secret store.

Rotate a token with a 15-minute overlap window:

```http
POST /v1/controller-installations/<controller_id>/tokens
Authorization: Bearer <OABCP_API_KEY>
```

Immediately revoke a particular token or disable an installation:

```http
DELETE /v1/controller-installations/<controller_id>/tokens/<token_id>
Authorization: Bearer <OABCP_API_KEY>

PATCH /v1/controller-installations/<controller_id>
Authorization: Bearer <OABCP_API_KEY>
Content-Type: application/json

{"enabled":false}
```

## Configure runtime events

Configure an HTTPS endpoint and explicit provider-neutral event grants after
creating the installation:

```http
POST /v1/controller-installations/<controller_id>/events
Authorization: Bearer <OABCP_API_KEY>
Content-Type: application/json

{
  "endpoint": "https://controller.example.com/openab/events?version=1",
  "events": [
    "session.opened",
    "session.progress",
    "session.terminal",
    "session.timeout",
    "session.superseded",
    "action.failed"
  ]
}
```

The response returns the controller-specific `event_signing_secret`. Store it
in the controller's secret store. Event rows are committed with their action,
message, or terminal session transition before dispatch. OCP sends the exact
stored JSON bytes with these headers:

```text
X-OAB-Controller-ID: <controller_id>
X-OAB-Event-ID: <event_id>
X-OAB-Timestamp: <unix seconds>
X-OAB-Signature: sha256=<lowercase hex HMAC>
```

The canonical HMAC input is:

```text
v1
<controller_id>
<event_id>
<timestamp>
POST
<exact encoded path and query>
<sha256 lowercase hex of exact body bytes>
```

Receivers reject timestamps outside five minutes and retain accepted event ids
for at least ten minutes to reject replays. Delivery has a ten-second request
timeout and retries after 10, 30, and 90 seconds. Four failed attempts, or a
five-minute delivery window, moves the event to dead letter without rolling
back the already-valid runtime transition. Operator-visible dead letters are
available at:

```http
GET /v1/controller-installations/<controller_id>/event-audit
Authorization: Bearer <OABCP_API_KEY>
```

Delivery is at-least-once. A crash after the receiver accepts an event but
before OCP records success can produce a duplicate, and a retried event can
arrive after a later event. Receivers must dedupe by `event_id` and use
`occurred_at` when they need domain ordering. Successfully delivered outbox
rows are retained for seven days; dead-letter audit entries remain available
through the operator endpoint.

A normal-close `session.terminal` payload carries, besides `state`, `reason`,
`decision`, and the `findings_red/yellow/green` counts, the session's
**`final_messages`**: the settled result span's message bodies, in order — the
same text the kernel itself parses for the verdict trailer and findings block.
This is the material an external controller derives its product projection
from (ADR 031 §6); there is no message read-back API. When the span is empty the
array carries the closing message text instead, so a normal close is never
silent. The array is capped at 256 KiB total; when over, the oldest parts are
dropped first and `final_messages_truncated: true` is set. The last part is
never dropped — if it alone exceeds the cap its *end* is kept, clipped on a
character boundary. Both rules protect the same thing: the verdict trailer and
findings block sit at the end of the span.
Timeout, supersede, and controller-requested closes (`close_session` action)
carry no result and no final messages — only a normal close has one.

## Execute an action

```http
POST /v1/controller/actions
Authorization: Bearer <install action token>
X-OAB-Action-ID: act_001
X-OAB-Scope: tenant:example/resource:demo
Content-Type: application/json

{
  "version": 1,
  "action_id": "act_001",
  "action": {
    "type": "open_session",
    "params": {
      "title": "Example council",
      "trigger_ref": "object:example/42",
      "trigger_fingerprint": "revision:abc123",
      "roster": ["chair", "rev1"],
      "quorum_n": 1,
      "chair_bot": "chair",
      "mode": "council",
      "prompt": "Inspect object 42."
    }
  }
}
```

`X-OAB-Action-ID` must equal the envelope `action_id`. OCP stores the completed
response under `(controller_id, action_id)`; an exact replay returns that same
body without running the interpreter again. Reusing an action id with different
body or scope returns `409 conflict`. Every admission, including a completed
replay, rechecks the current token, action grant, scope, and session ownership.

An action left in `processing` by a process crash has a five-minute lease. Once
that lease expires, OCP marks the outcome indeterminate and returns a stable,
non-retryable `409` for that action id. OCP does not automatically re-execute an
action whose side effects may already have happened; the controller must first
reconcile its domain state, then use a new action id if another action is safe.

## Reconcile an action

Use the same installation token and exact scope to recover a lost action
response, a dead-lettered runtime event, or an indeterminate `open_session`:

```http
GET /v1/controller/actions/act_001
Authorization: Bearer <install action token>
X-OAB-Scope: tenant:example/resource:demo
```

```json
{
  "version": 1,
  "action_id": "act_001",
  "action_kind": "open_session",
  "scope": "tenant:example/resource:demo",
  "state": "completed",
  "http_status": 200,
  "response": {
    "version": 1,
    "action_id": "act_001",
    "result": {
      "type": "session_opened",
      "data": { "session_id": "ses_123", "deduped": false }
    }
  },
  "session": {
    "session_id": "ses_123",
    "state": "closed",
    "trigger_ref": "object:example/42",
    "trigger_fingerprint": "revision:abc123",
    "closed_at": 1786000000000,
    "decision": "approve",
    "result": {
      "author_id": "chair",
      "message_ids": ["msg_123"],
      "text": "The settled controller result."
    }
  }
}
```

`state` is `processing`, `completed`, or `outcome_unknown`. Optional fields are
omitted until they exist. The `response` is the exact stored action response;
the `session` projection contains only provider-neutral kernel state and the
canonical settled result. It deliberately excludes roster, transcript,
operator audit, and provider/product data.

For a crash after the kernel session committed but before the action result and
`session.opened` event committed, an admitted `open_session` retains its opaque
trigger intent. The read can therefore return `outcome_unknown` together with
the already-created session. A controller should inspect that session before
choosing a new action id. A new retry with the same trigger/fingerprint will
then dedupe through the normal kernel path.

The read returns `404` when the action does not belong to the authenticated
installation and exact currently-enabled scope. Revoked/expired credentials
return `401`. Disabling an installation does not erase its action outcomes;
an otherwise-active token may still reconcile them, but revoking its scope or
token removes that access.

Every external `open_session` requires an opaque `trigger_ref`. Dedupe and
fingerprint supersede are controller-scoped, so two installations may use the
same external ref without sharing a session. Later actions may address only
sessions owned by the same installation and scope.

Errors use the versioned `ErrorEnvelope` from `controller-protocol`. Rate quota
responses return `429` and `Retry-After`; concurrent-session quota responses
return `409`. Grant, scope, and session-ownership checks fail closed. Action
request bodies are bounded to 1 MiB before full buffering.

## Canary note

The first pull request reviewed end to end by the external controller —
session opened, verdict parsed from `final_messages`, and all three GitHub
writes (round comment, `openab/council` status, formal review) performed by
the controller's own App credentials — was the pull request that added this
paragraph.
Canary note 2: verify v0.1.43 status write + chair message hygiene.
Canary note 3: verify v0.1.44 marker-slice comment.
