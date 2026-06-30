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
local dev often rebuilds the same `:local` tag. It fails if the running pod
`imageID` does not match the local Docker image, because that means Docker
Desktop Kubernetes is still running an older binary.

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

## Run Local OpenAB Bot Pods

OCP alone is not enough for an end-to-end review. The bot pods are the southbound
execution layer: they fetch `/bot-config/<name>`, connect to `/ws`, run the
selected CLI, and post replies back to OCP.

For the current Kiro local test, create a Kubernetes Secret first:

```sh
kubectl -n oabcp-local create secret generic kiro-api \
  --from-literal=KIRO_API_KEY=<KIRO_API_KEY>
```

Then deploy the three bot pods:

```sh
scripts/dev-deploy-bots.sh --agent kiro
```

The script auto-detects the `kiro-api` secret when it exists and uses
`ghcr.io/openabdev/openab:0.9.0-beta.3-kiro`. The Kiro built-in agent profile
serves:

```toml
[agent]
command = "kiro-cli"
args = ["acp", "--trust-all-tools"]
working_dir = "/home/agent"
```

That `working_dir` matters: the Kiro OpenAB image uses `/home/agent`, while the
Claude image uses `/home/node`. A missing working directory can surface as a
spawn `No such file or directory` error even when the CLI exists in `PATH`.

Mixed CLI councils use two independent settings:

- OCP agent profiles decide the served `[agent]` command, args, working directory,
  and inherited env names.
- Bot deployments decide which OpenAB image and Kubernetes Secret each pod gets.

For example, this runs a Kiro chair with Claude reviewers:

```sh
scripts/dev-run-host-ocp.sh \
  --agent-profiles-json '{"kiro":{"args":["acp","--trust-all-tools"]}}'

scripts/dev-deploy-bots.sh \
  --bot-agents chair=kiro,rev1=claude,rev2=claude \
  --config-base-url http://host.docker.internal:18090 \
  --agent-secret kiro=kiro-api:KIRO_API_KEY \
  --agent-secret claude=claude-oauth:CLAUDE_CODE_OAUTH_TOKEN
```

Use `--agent-images agent=image,...` when a profile does not have a built-in
local image. Any CLI permission/trust switch belongs in the agent profile `args`;
the deploy script does not infer a bypass flag for unknown CLIs.

If the test should also let the chair write PR comments, sync your local
`gh auth token` into a Kubernetes Secret and inject it only into the chair:

```sh
scripts/dev-sync-gh-token-secret.sh

scripts/dev-deploy-bots.sh \
  --agent kiro \
  --chair-secret-name gh-token \
  --chair-credential-env GH_TOKEN
```

This is a local development shortcut. The production GitHub App path should keep
using the chair pod's App identity setup from
[install-github-app.md](install-github-app.md), not a shared user PAT.

To scale the bots down without deleting their deployments:

```sh
scripts/dev-deploy-bots.sh --replicas 0
```

To remove them:

```sh
scripts/dev-deploy-bots.sh --delete
```

If Docker Desktop Kubernetes cannot see a freshly built local OCP image, you can
still test the bot execution path by running OCP on the host and pointing bot
pods at it:

```sh
scripts/dev-run-host-ocp.sh
```

In another terminal:

```sh
scripts/dev-sync-gh-token-secret.sh

scripts/dev-deploy-bots.sh \
  --agent kiro \
  --config-base-url http://host.docker.internal:18090 \
  --chair-secret-name gh-token \
  --chair-credential-env GH_TOKEN
```

For webhook testing with host OCP, run the tunnel pod against the host service:

```sh
scripts/dev-tunnel-k8s.sh --origin-url http://host.docker.internal:18090
```

Custom or experimental CLIs should be configured as OCP agent profiles, not by
editing Rust code. Put command args, permission/trust flags, working directory,
and extra inherited env vars in `OABCP_AGENT_PROFILES`; see
[config-reference.md](config-reference.md).

## Expose Local OCP To GitHub

There are two local tunnel options.

The Kubernetes-native path is preferred for end-to-end local review testing. It
runs cloudflared inside the same namespace as OCP, so cloudflared reaches OCP
through `http://control-plane:8090` and no host-side port-forward is needed:

```sh
scripts/dev-tunnel-k8s.sh
```

The script prints:

```text
tunnel URL: https://<host>.trycloudflare.com
webhook URL: https://<host>.trycloudflare.com/api/v1/github_webhooks
```

Cloudflare quick tunnel DNS can take a few seconds to reach your local resolver.
If a first `curl` says it cannot resolve the host, retry after a short wait.

Patch the GitHub App webhook to that URL:

```sh
scripts/dev-webhook.sh \
  --url https://<host>.trycloudflare.com/api/v1/github_webhooks \
  --key-path /Users/can/Downloads/zeabur-council.2026-06-27.private-key.pem
```

Then probe the same public route GitHub will use:

```sh
scripts/dev-webhook-ready.sh \
  --url https://<host>.trycloudflare.com/api/v1/github_webhooks \
  --repo canyugs/openab-control-plane \
  --pr 53
```

The older host-side scripted path is still useful when you do not want a tunnel
pod in Kubernetes. It starts a local port-forward if needed, starts a Cloudflare
quick tunnel on the host, points the App webhook at the tunnel URL, and restores
the original App webhook URL when you stop it.

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

Manual host-side equivalent: keep the `kubectl port-forward` running, then open a second terminal:

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
