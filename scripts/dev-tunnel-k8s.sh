#!/usr/bin/env bash
# Run cloudflared inside the local Kubernetes namespace and point it at OCP.
set -euo pipefail

KUBE_NAMESPACE="${KUBE_NAMESPACE:-oabcp-local}"
KUBE_CONTEXT="${KUBE_CONTEXT:-docker-desktop}"
NAME="${NAME:-cloudflared}"
IMAGE="${IMAGE:-cloudflare/cloudflared:latest}"
ORIGIN_URL="${ORIGIN_URL:-http://control-plane:8090}"
WEBHOOK_PATH="${WEBHOOK_PATH:-/api/v1/github_webhooks}"
WAIT_ROLLOUT=1
CHECK_CONTEXT=1
DELETE=0

usage() {
  cat <<'USAGE'
Usage:
  scripts/dev-tunnel-k8s.sh
  scripts/dev-tunnel-k8s.sh --delete

Options:
  --origin-url <url>      In-cluster origin URL. Default: http://control-plane:8090.
  --webhook-path <path>   Webhook path printed with the tunnel URL. Default: /api/v1/github_webhooks.
  --name <name>           Deployment name. Default: cloudflared.
  --image <image>         cloudflared image. Default: cloudflare/cloudflared:latest.
  --namespace <name>      Kubernetes namespace. Default: oabcp-local.
  --context <name>        Expected kubectl context. Default: docker-desktop.
  --any-context           Do not enforce the kubectl context.
  --no-wait               Do not wait for rollout or URL discovery.
  --delete                Delete the tunnel deployment.

Environment:
  KUBE_NAMESPACE, KUBE_CONTEXT, NAME, IMAGE, ORIGIN_URL, WEBHOOK_PATH.

The script creates a Cloudflare quick tunnel inside Kubernetes. GitHub reaches
cloudflared, and cloudflared reaches OCP through the Kubernetes service network,
so no host-side port-forward is needed.
USAGE
}

die() {
  echo "error: $*" >&2
  exit 1
}

need() {
  command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --origin-url) ORIGIN_URL="${2:?--origin-url needs a value}"; shift 2 ;;
    --origin-url=*) ORIGIN_URL="${1#*=}"; shift ;;
    --webhook-path) WEBHOOK_PATH="${2:?--webhook-path needs a value}"; shift 2 ;;
    --webhook-path=*) WEBHOOK_PATH="${1#*=}"; shift ;;
    --name) NAME="${2:?--name needs a value}"; shift 2 ;;
    --name=*) NAME="${1#*=}"; shift ;;
    --image) IMAGE="${2:?--image needs a value}"; shift 2 ;;
    --image=*) IMAGE="${1#*=}"; shift ;;
    --namespace) KUBE_NAMESPACE="${2:?--namespace needs a value}"; shift 2 ;;
    --namespace=*) KUBE_NAMESPACE="${1#*=}"; shift ;;
    --context) KUBE_CONTEXT="${2:?--context needs a value}"; shift 2 ;;
    --context=*) KUBE_CONTEXT="${1#*=}"; shift ;;
    --any-context) CHECK_CONTEXT=0; shift ;;
    --no-wait) WAIT_ROLLOUT=0; shift ;;
    --delete) DELETE=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
done

need kubectl
[[ "$WEBHOOK_PATH" == /* ]] || die "--webhook-path must start with /"

if [[ "$CHECK_CONTEXT" == "1" ]]; then
  current_context=$(kubectl config current-context)
  [[ "$current_context" == "$KUBE_CONTEXT" ]] ||
    die "kubectl context is '$current_context', expected '$KUBE_CONTEXT' (or pass --any-context)"
fi

if [[ "$DELETE" == "1" ]]; then
  kubectl -n "$KUBE_NAMESPACE" delete deployment "$NAME" --ignore-not-found
  exit 0
fi

kubectl create namespace "$KUBE_NAMESPACE" --dry-run=client -o yaml | kubectl apply -f - >/dev/null

echo "applying in-cluster cloudflared tunnel: $ORIGIN_URL"
kubectl -n "$KUBE_NAMESPACE" apply -f - >/dev/null <<YAML
apiVersion: apps/v1
kind: Deployment
metadata:
  name: $NAME
  labels:
    app: $NAME
spec:
  replicas: 1
  selector:
    matchLabels:
      app: $NAME
  template:
    metadata:
      labels:
        app: $NAME
    spec:
      containers:
        - name: cloudflared
          image: $IMAGE
          imagePullPolicy: IfNotPresent
          args:
            - tunnel
            - --no-autoupdate
            - --url
            - $ORIGIN_URL
YAML

# A quick tunnel URL is allocated at process start; restart on every run so the
# printed URL always belongs to the currently running pod.
kubectl -n "$KUBE_NAMESPACE" rollout restart "deployment/$NAME" >/dev/null

if [[ "$WAIT_ROLLOUT" == "0" ]]; then
  exit 0
fi

kubectl -n "$KUBE_NAMESPACE" rollout status "deployment/$NAME" --timeout=180s

for _ in $(seq 1 60); do
  logs=$(kubectl -n "$KUBE_NAMESPACE" logs "deployment/$NAME" --tail=120 2>/dev/null || true)
  tunnel_url=$(printf '%s\n' "$logs" | sed -nE 's/.*(https:\/\/[A-Za-z0-9.-]+\.trycloudflare\.com).*/\1/p' | tail -n 1)
  if [[ -n "$tunnel_url" ]]; then
    echo "tunnel URL: $tunnel_url"
    echo "webhook URL: ${tunnel_url}${WEBHOOK_PATH}"
    echo "note: trycloudflare DNS can take a few seconds to reach the local resolver."
    echo
    echo "next: scripts/dev-webhook.sh --url ${tunnel_url}${WEBHOOK_PATH} --key-path <app-private-key.pem>"
    exit 0
  fi
  sleep 1
done

kubectl -n "$KUBE_NAMESPACE" logs "deployment/$NAME" --tail=120 >&2 || true
die "timed out waiting for cloudflared quick tunnel URL"
