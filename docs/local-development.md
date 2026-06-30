# Local Development

This workflow runs the control plane on Docker Desktop Kubernetes and exposes it
through a temporary HTTPS tunnel when GitHub needs to call your local webhook.

Use it for fast control-plane iteration. The minimal profile below runs only OCP:
it validates the north API, webhook ingress, sessions, and roster control. A full
PR review still needs OpenAB chair/reviewer pods connected to `/ws`, or the full
Zeabur template deployment.

## Prerequisites

- Docker Desktop with Kubernetes enabled.
- `kubectl` context set to `docker-desktop`.
- `cloudflared` installed for public webhook testing.

Check the local cluster:

```sh
kubectl config current-context
kubectl get nodes
```

Expected context:

```text
docker-desktop
```

## Build The Local Image

The release workflow currently publishes an amd64 image for Zeabur. On Apple
Silicon, Docker Desktop Kubernetes is arm64, so build a local image instead of
pulling the release tag.

```sh
scripts/dev-build-image.sh
```

The script builds a unique `openab-control-plane:dev-...` tag and also updates
`openab-control-plane:local`. On an amd64 machine, use:

```sh
scripts/dev-build-image.sh --platform linux/amd64
```

## Run OCP On Docker Desktop Kubernetes

```sh
scripts/dev-deploy-k8s.sh --image openab-control-plane:local
```

If you want to run a unique tag printed by `dev-build-image.sh`, pass it directly:

```sh
scripts/dev-deploy-k8s.sh --image openab-control-plane:dev-<sha>-<timestamp>
```

The deploy script creates or updates the namespace, deployment, service, local
API key, bot roster, and webhook secret. It also forces a rollout restart, because
local dev often rebuilds the same `:local` tag.

Smoke check the webhook path. This starts a temporary port-forward if one is
needed:

```sh
scripts/dev-webhook-ready.sh --action synchronize --repo canyugs/openab-control-plane --pr 53
```

Expected output:

```text
webhook ready: session <session_id>
```

This check is intentionally stronger than `kubectl rollout status`: it signs a
synthetic `pull_request.synchronize` webhook with the local secret and verifies
that OCP returns `triggered:true`. Run it before pushing a test commit or asking
GitHub to redeliver a webhook.

## Expose Local OCP To GitHub

The scripted path is preferred when testing the `zeabur-council` GitHub App. It
starts the local port-forward if needed, starts a Cloudflare quick tunnel, points
the App webhook at the tunnel URL, and restores the original App webhook URL when
you stop it.

```sh
scripts/dev-webhook.sh --quick \
  --key-path /Users/can/Downloads/zeabur-council.2026-06-27.private-key.pem
```

This updates the App-level webhook, so it affects every installation of the
`zeabur-council` GitHub App while the script is running.

To use a fixed tunnel hostname instead of a quick tunnel:

```sh
scripts/dev-webhook.sh \
  --url https://<fixed-host> \
  --key-path /Users/can/Downloads/zeabur-council.2026-06-27.private-key.pem
```

To inspect the current App webhook config without changing it:

```sh
scripts/dev-webhook.sh --check \
  --key-path /Users/can/Downloads/zeabur-council.2026-06-27.private-key.pem
```

To inspect recent App webhook deliveries:

```sh
scripts/dev-app-deliveries.sh \
  --key-path /Users/can/Downloads/zeabur-council.2026-06-27.private-key.pem
```

Manual equivalent: keep the `kubectl port-forward` running, then open a second terminal:

```sh
cloudflared tunnel --url http://localhost:8090
```

`cloudflared` prints a public `https://...trycloudflare.com` URL. Configure the
GitHub App or repository webhook to:

```text
https://<cloudflared-host>/api/v1/github_webhooks
```

Use the same `WEBHOOK_SECRET` value in your GitHub webhook settings. Subscribe
to Pull requests and Issue comments.

With the App webhook pointed at local OCP, these GitHub actions should open a
local session:

- comment `/review` on a PR;
- push a commit to an open PR branch (`pull_request.synchronize`);
- open, reopen, or mark a draft PR ready for review.

Before triggering those actions, run the webhook readiness probe through the same
route GitHub will use. For a Cloudflare tunnel URL:

```sh
scripts/dev-webhook-ready.sh \
  --url https://<cloudflared-host>/api/v1/github_webhooks \
  --repo canyugs/openab-control-plane \
  --pr 53
```

For direct north API calls through the tunnel:

```sh
PLANE=https://<cloudflared-host>
KEY=local-test-key

curl -H "Authorization: Bearer $KEY" "$PLANE/v1/council/roster"
```

## Dynamic Replacement Smoke Test

Register a temporary reviewer:

```sh
curl -sS -X POST \
  -H "Authorization: Bearer local-test-key" \
  -H "content-type: application/json" \
  http://127.0.0.1:8090/v1/bots \
  -d '{"name":"rev3","role":"reviewer"}'
```

Use the returned `bot_id` for these checks.

Future webhook sessions:

```sh
curl -sS -X POST \
  -H "Authorization: Bearer local-test-key" \
  -H "content-type: application/json" \
  http://127.0.0.1:8090/v1/council/roster/replace \
  -d '{"old_bot_id":"rev2","new_bot_id":"<bot_id>"}'
```

Restore the standing roster after the test so future reviews do not use a bot
without a running pod:

```sh
curl -sS -X PUT \
  -H "Authorization: Bearer local-test-key" \
  -H "content-type: application/json" \
  http://127.0.0.1:8090/v1/council/roster \
  -d '{"roster":["chair","rev1","rev2"]}'
```

Active session:

```sh
curl -sS -X POST \
  -H "Authorization: Bearer local-test-key" \
  -H "content-type: application/json" \
  http://127.0.0.1:8090/v1/sessions \
  -d '{"title":"local replacement smoke","roster":["chair","rev1","rev2"],"quorum_n":1,"chair_bot":"chair","mode":"council"}'

curl -sS -X POST \
  -H "Authorization: Bearer local-test-key" \
  -H "content-type: application/json" \
  http://127.0.0.1:8090/v1/sessions/<session_id>/roster/replace \
  -d '{"old_bot_id":"rev2","new_bot_id":"<bot_id>"}'
```

Confirm the roster:

```sh
curl -H "Authorization: Bearer local-test-key" \
  http://127.0.0.1:8090/v1/sessions/<session_id>
```

## Cleanup

Stop `cloudflared` and `kubectl port-forward`, then remove the local namespace:

```sh
kubectl delete namespace oabcp-local
```
