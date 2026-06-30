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
pulling the release tag:

```sh
docker buildx build --platform linux/arm64 --load \
  -t openab-control-plane:local .
```

On an amd64 machine, use `--platform linux/amd64`.

## Run OCP On Docker Desktop Kubernetes

```sh
WEBHOOK_SECRET=$(openssl rand -hex 32)

kubectl create namespace oabcp-local

kubectl -n oabcp-local create deployment control-plane \
  --image=openab-control-plane:local \
  --port=8090

kubectl -n oabcp-local set env deployment/control-plane \
  OABCP_ADDR=0.0.0.0:8090 \
  OABCP_API_KEY=local-test-key \
  GITHUB_WEBHOOK_SECRET="$WEBHOOK_SECRET" \
  OABCP_BOTS=chair:chair,rev1:reviewer,rev2:reviewer \
  OABCP_COUNCIL_ROSTER=chair,rev1,rev2

kubectl -n oabcp-local patch deployment control-plane \
  -p '{"spec":{"template":{"spec":{"containers":[{"name":"openab-control-plane","imagePullPolicy":"Never"}]}}}}'

kubectl -n oabcp-local rollout status deployment/control-plane --timeout=120s
kubectl -n oabcp-local expose deployment control-plane \
  --type=ClusterIP \
  --port=8090 \
  --target-port=8090
```

Forward the north API locally:

```sh
kubectl -n oabcp-local port-forward service/control-plane 8090:8090
```

Smoke check:

```sh
curl -H "Authorization: Bearer local-test-key" \
  http://127.0.0.1:8090/v1/council/roster
```

Expected:

```json
{"roster":["chair","rev1","rev2"],"source":"env"}
```

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
