# GitHub controller shadow runbook

P7 compares plans only. OCP remains the only raw-ingress owner, continues using
the in-process interpreter, and remains the findings owner. The controller must
receive a selected replay/copy, never the authoritative GitHub route.

## Safety gate

Before starting a shadow copy, record:

```text
repository
state=shadow
ingress_route_revision
ocp_image
controller_image
side_effect_owner=embedded
findings_owner=ocp
credential_path=none-for-controller
promoted_at / promoted_by
rollback_route / rollback_images
```

The controller environment must contain the webhook and shadow HMAC secrets,
its own database, roster/preset configuration, and nothing else. In particular,
do not set an OCP action token, `GH_TOKEN`, or any
`GITHUB_CONTROLLER_GITHUB_APP_*` variable. `/readyz` fails if App credentials
are present.

## Replay and live-copy procedure

1. Start from one allowlisted synthetic P0 fixture. Produce the normalized
   embedded outcome. A trigger uses
   `{"outcome":"planned","snapshot":{...}}`, populated from the embedded
   session detail (`GET /v1/sessions/:id`) plus the pinned dedupe,
   terminal-projection, and proposed-write fields. An ignored event uses
   `{"outcome":"ignored","reason":"not_a_trigger"}` (or the exact embedded
   reason).
2. Build and submit a signed wrapper with the repository helper:

   ```sh
   export GITHUB_CONTROLLER_SHADOW_SECRET='...'
   scripts/github-controller-shadow.sh compare \
     --url http://127.0.0.1:8091 \
     --comparison-id fixture-opened-1 \
     --delivery-id fixture-delivery-1 \
     --event pull_request \
     --payload tests/fixtures/github/pull_request_opened.json \
     --embedded /path/to/embedded-parity-snapshot.json
   ```

   The helper signs the exact generated wrapper bytes and does not put the
   secret in a process argument. JSON `null` means the embedded reference was
   unavailable and intentionally produces a blocking mismatch.
3. Confirm `identity_or_ownership_mismatches=0`. An exact fixture must also have
   `presentation_mismatches=0`.
4. Enable a copy for one explicitly named repository. The original GitHub
   request still goes only to embedded OCP; the copy goes to the shadow
   comparator. Never configure fallback from controller to embedded ingress.
5. Review every presentation mismatch and record why it is acceptable or fix
   it. Any identity/ownership mismatch freezes promotion immediately.
6. Check the authenticated summary. Required P7 budget is zero identity or
   ownership mismatch reports.

   ```sh
   scripts/github-controller-shadow.sh summary --url http://127.0.0.1:8091
   ```

The comparison store keeps only aggregate counts. Preserve reviewed fixture
reports in CI or an operator artifact if a durable explanation is required.

## Stop and rollback

Stop the copy route first. Confirm that OCP remained the sole ingress and
side-effect owner, then stop the controller. Because P7 has no action or write
credentials, there is no controller mutation to drain or reverse. Retain the
OCP/controller image pair and the comparison summary with the cutover record.
