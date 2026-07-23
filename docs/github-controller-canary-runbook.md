# GitHub controller external-canary runbook

P8 moves raw GitHub ingress for one development repository from embedded OCP
to the external controller. OCP remains the runtime, compatibility findings
owner, and compatibility capability path. The controller receives one scoped
`open_session` credential and a per-controller runtime-event secret. It has no
GitHub App credential and never falls back to embedded ingress.

This is not the P9 product-projection cutover. Runtime event receipts are used
only for delivery audit and aggregate canary visibility; their payloads are not
persisted by the controller.

## Cutover record

Create one record before changing traffic:

```text
repository=<owner/repo>
state=external_canary
ingress_route_revision_before=<embedded revision>
ingress_route_revision_external=<external revision>
ocp_image=<immutable digest>
controller_image=<immutable digest>
side_effect_owner=<exactly one compatibility actor>
findings_owner=ocp
credential_path=ocp-compatibility
controller_id=github-canary
controller_scope=tenant:dev/resource:github-canary
embedded_repo_counter_baseline=<integer>
promoted_at=<timestamp>
promoted_by=<operator>
rollback_route=<embedded revision>
rollback_images=<compatible OCP/controller digests>
```

Use a repository-specific edge route or a GitHub App/webhook installation that
is dedicated to this repository. A single App webhook URL shared by unrelated
repositories is not a per-repository cutover mechanism. At every revision,
exactly one route accepts this repository's raw webhook: embedded or external,
never both.

## Install the controller boundary

OCP must have its operator key, action peppers, and event signing keys
configured as described in [Controller Action API](controller-action-api.md).
Create the canary installation with only `open_session` and one exact scope:

```sh
curl -sS --fail-with-body \
  -H "Authorization: Bearer $OABCP_KEY" \
  -H 'content-type: application/json' \
  --data '{
    "id":"github-canary",
    "actions":["open_session"],
    "scopes":["tenant:dev/resource:github-canary"],
    "max_concurrent_sessions":3,
    "max_actions_per_minute":60
  }' \
  "$OABCP_URL/v1/controller-installations"
```

Store the one-time action token from that response as
`GITHUB_CONTROLLER_OCP_ACTION_TOKEN`. Configure all six generic runtime events:

```sh
curl -sS --fail-with-body \
  -H "Authorization: Bearer $OABCP_KEY" \
  -H 'content-type: application/json' \
  --data '{
    "endpoint":"https://controller.example.com/api/v1/openab/events?version=1",
    "events":[
      "session.opened",
      "session.progress",
      "session.terminal",
      "session.timeout",
      "session.superseded",
      "action.failed"
    ]
  }' \
  "$OABCP_URL/v1/controller-installations/github-canary/events"
```

Store the returned `event_signing_secret` as
`GITHUB_CONTROLLER_EVENT_SIGNING_SECRET`. Configure the controller with:

```text
GITHUB_CONTROLLER_MODE=external_canary
GITHUB_CONTROLLER_CANARY_REPOSITORY=<owner/repo>
GITHUB_CONTROLLER_ALLOWED_REPOS=<owner/repo>
GITHUB_CONTROLLER_WEBHOOK_SECRET=<repository webhook secret>
GITHUB_CONTROLLER_OCP_URL=https://<ocp-origin>
GITHUB_CONTROLLER_OCP_ACTION_TOKEN=<installation token>
GITHUB_CONTROLLER_OCP_SCOPE=tenant:dev/resource:github-canary
GITHUB_CONTROLLER_ID=github-canary
GITHUB_CONTROLLER_EVENT_SIGNING_SECRET=<issued per-controller secret>
GITHUB_CONTROLLER_OBSERVER_SECRET=<independent random HMAC secret>
```

Do not set `GITHUB_CONTROLLER_GITHUB_APP_*`, `GH_TOKEN`, or
`GITHUB_CONTROLLER_SHADOW_SECRET` unless the separate P7 shadow endpoint is
still intentionally used. Keep the controller database on its own persistent
volume.

## Promotion gate

Record the repository-specific embedded counter from authenticated
`GET /v1/compatibility-usage`, then verify exact ownership and readiness:

```sh
scripts/github-controller-canary.sh preflight \
  --repository <owner/repo> \
  --url https://controller.example.com
```

Change the route with the router or webhook provider's atomic revision/CAS
operation. The new revision must remove the embedded destination and add the
external destination in one change. Do not implement health-based fallback to
embedded OCP; an unavailable controller returns `503` so GitHub retains the
delivery for retry.

Run the following dev-lane drills and attach delivery ids, session ids, image
digests, event audit, aggregate summary, and route revisions to the cutover
record:

1. Open a real development PR and allow the council to reach a normal terminal
   verdict. Confirm `session.opened`, progress, and `session.terminal` receipts.
2. Redeliver the same GitHub delivery. Confirm the response is a duplicate and
   OCP still has one session for that trigger/fingerprint.
3. Push a new head. Confirm the new delivery supersedes the prior active
   session and the controller receives `session.superseded`.
4. Open another canary session with silent reviewers and let the watchdog close
   it. Confirm `session.timeout` in the authenticated canary summary.
5. Stop the OCP action endpoint or controller-to-OCP network path before action
   acceptance. Confirm the controller returns `503`, records a retryable
   delivery, does not invoke embedded ingress, and succeeds with the same action
   id after recovery.
6. Re-read the repository-specific embedded counter. It must equal the recorded
   baseline throughout the external window. Global embedded traffic from other
   repositories is irrelevant to this gate.

Inspect aggregate controller state with:

```sh
export GITHUB_CONTROLLER_OBSERVER_SECRET='...'
scripts/github-controller-canary.sh summary \
  --url https://controller.example.com
```

Inspect OCP delivery retries and dead letters through the authenticated
`/v1/controller-installations/github-canary/event-audit` endpoint. A runtime
event outage never rolls back a valid session transition.

## Atomic rollback drill

Use the same procedure for the required dev-lane rollback test and for an
incident:

1. Stop the external ingress route for the repository. Keep embedded disabled.
   Record the stopped external route revision.
2. Let already accepted controller requests finish. Resolve any retryable
   delivery by retrying its deterministic action id against OCP; do not replay
   it through embedded ingress.
3. Run the read-only rollback gate:

   ```sh
   export GITHUB_CONTROLLER_OBSERVER_SECRET='...'
   export OABCP_KEY='...'
   scripts/github-controller-canary.sh rollback-gate \
     --repository <owner/repo> \
     --expected-embedded-count <baseline> \
     --route-revision <stopped-external-revision> \
     --url https://controller.example.com \
     --plane-url https://ocp.example.com
   ```

   The gate fails if controller work is processing/retryable, readiness no
   longer proves exact ownership, or the embedded repository counter changed.
4. Atomically restore the recorded embedded route revision. Do not leave the
   stopped external route enabled as a fallback.
5. Deliver one new signed synthetic or development event through the restored
   route. Confirm the repository-specific embedded counter increments exactly
   once and the external controller's acted count does not change.
6. Record the restored route revision and both counter snapshots. Stop the
   controller or return it to `plan_only` only after the route proof.

If the rollback gate cannot become clean, keep both raw ingress routes stopped
for the canary repository and reconcile the deterministic action ids. Enabling
both paths to recover availability is not an approved rollback.
