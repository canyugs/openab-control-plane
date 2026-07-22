# Controller Action API

Status: P4 provider-neutral boundary. This API implements the inbound action
half of [ADR 008](adr/008-external-controller-protocol.md). Runtime-event
delivery is a separate P5 deliverable.

## Configuration

Set both variables on OCP:

```text
OABCP_API_KEY=<root operator bearer>
OABCP_CONTROLLER_ACTION_PEPPERS={"1":"<base64url-encoded 32+-byte secret>"}
```

Pepper versions are positive integers. OCP stores only the HMAC-SHA256 of each
action token (using `pepper_vN` as the HMAC key) and the selected version. Keep
an old pepper configured until its tokens have been rotated or revoked.

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
body or scope returns `409 conflict`.

Every external `open_session` requires an opaque `trigger_ref`. Dedupe and
fingerprint supersede are controller-scoped, so two installations may use the
same external ref without sharing a session. Later actions may address only
sessions owned by the same installation and scope.

Errors use the versioned `ErrorEnvelope` from `controller-protocol`. Rate quota
responses return `429` and `Retry-After`; concurrent-session quota responses
return `409`. Grant, scope, and session-ownership checks fail closed.
