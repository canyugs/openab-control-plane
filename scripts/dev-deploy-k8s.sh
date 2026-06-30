#!/usr/bin/env bash
# Create or update the local Docker Desktop Kubernetes OCP deployment.
set -euo pipefail

KUBE_NAMESPACE="${KUBE_NAMESPACE:-oabcp-local}"
KUBE_CONTEXT="${KUBE_CONTEXT:-docker-desktop}"
IMAGE="${IMAGE:-openab-control-plane:local}"
OABCP_API_KEY="${OABCP_API_KEY:-local-test-key}"
OABCP_BOTS="${OABCP_BOTS:-chair:chair,rev1:reviewer,rev2:reviewer}"
OABCP_COUNCIL_ROSTER="${OABCP_COUNCIL_ROSTER:-chair,rev1,rev2}"
OABCP_ADDR="${OABCP_ADDR:-0.0.0.0:8090}"
REMOTE_PORT="${REMOTE_PORT:-8090}"
WAIT_ROLLOUT=1
CHECK_CONTEXT=1

usage() {
  cat <<'USAGE'
Usage:
  scripts/dev-deploy-k8s.sh [--image <image>] [--namespace <name>]

Options:
  --image <image>         Image to run. Default: openab-control-plane:local.
  --namespace <name>      Kubernetes namespace. Default: oabcp-local.
  --context <name>        Expected kubectl context. Default: docker-desktop.
  --any-context           Do not enforce the kubectl context.
  --api-key <value>       Local north API key. Default: local-test-key.
  --webhook-secret <val>  GitHub webhook secret. Default: preserve existing or generate.
  --no-wait              Do not wait for rollout completion.

Environment:
  IMAGE, KUBE_NAMESPACE, KUBE_CONTEXT, OABCP_API_KEY,
  GITHUB_WEBHOOK_SECRET, OABCP_BOTS, OABCP_COUNCIL_ROSTER.
USAGE
}

die() {
  echo "error: $*" >&2
  exit 1
}

need() {
  command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"
}

WEBHOOK_SECRET="${GITHUB_WEBHOOK_SECRET:-}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --image) IMAGE="${2:?--image needs a value}"; shift 2 ;;
    --image=*) IMAGE="${1#*=}"; shift ;;
    --namespace) KUBE_NAMESPACE="${2:?--namespace needs a value}"; shift 2 ;;
    --namespace=*) KUBE_NAMESPACE="${1#*=}"; shift ;;
    --context) KUBE_CONTEXT="${2:?--context needs a value}"; shift 2 ;;
    --context=*) KUBE_CONTEXT="${1#*=}"; shift ;;
    --any-context) CHECK_CONTEXT=0; shift ;;
    --api-key) OABCP_API_KEY="${2:?--api-key needs a value}"; shift 2 ;;
    --api-key=*) OABCP_API_KEY="${1#*=}"; shift ;;
    --webhook-secret) WEBHOOK_SECRET="${2:?--webhook-secret needs a value}"; shift 2 ;;
    --webhook-secret=*) WEBHOOK_SECRET="${1#*=}"; shift ;;
    --no-wait) WAIT_ROLLOUT=0; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
done

need kubectl
need openssl

if [[ "$CHECK_CONTEXT" == "1" ]]; then
  current_context=$(kubectl config current-context)
  [[ "$current_context" == "$KUBE_CONTEXT" ]] ||
    die "kubectl context is '$current_context', expected '$KUBE_CONTEXT' (or pass --any-context)"
fi

kubectl create namespace "$KUBE_NAMESPACE" --dry-run=client -o yaml | kubectl apply -f - >/dev/null

if [[ -z "$WEBHOOK_SECRET" ]]; then
  WEBHOOK_SECRET=$(kubectl -n "$KUBE_NAMESPACE" get deployment control-plane \
    -o jsonpath='{range .spec.template.spec.containers[0].env[?(@.name=="GITHUB_WEBHOOK_SECRET")]}{.value}{end}' \
    2>/dev/null || true)
fi
if [[ -z "$WEBHOOK_SECRET" ]]; then
  WEBHOOK_SECRET=$(openssl rand -hex 32)
  echo "generated a new GITHUB_WEBHOOK_SECRET for the local deployment"
fi

echo "applying local control-plane deployment with image: $IMAGE"
kubectl -n "$KUBE_NAMESPACE" apply -f - >/dev/null <<YAML
apiVersion: apps/v1
kind: Deployment
metadata:
  name: control-plane
  labels:
    app: control-plane
spec:
  replicas: 1
  selector:
    matchLabels:
      app: control-plane
  template:
    metadata:
      labels:
        app: control-plane
    spec:
      containers:
        - name: openab-control-plane
          image: "$IMAGE"
          imagePullPolicy: Never
          ports:
            - containerPort: $REMOTE_PORT
          env:
            - name: OABCP_ADDR
              value: "$OABCP_ADDR"
            - name: OABCP_API_KEY
              value: "$OABCP_API_KEY"
            - name: GITHUB_WEBHOOK_SECRET
              value: "$WEBHOOK_SECRET"
            - name: OABCP_BOTS
              value: "$OABCP_BOTS"
            - name: OABCP_COUNCIL_ROSTER
              value: "$OABCP_COUNCIL_ROSTER"
---
apiVersion: v1
kind: Service
metadata:
  name: control-plane
  labels:
    app: control-plane
spec:
  selector:
    app: control-plane
  ports:
    - name: http
      port: $REMOTE_PORT
      targetPort: $REMOTE_PORT
YAML

# Recreate the pod even when the image tag did not change; this catches the common
# local dev case where openab-control-plane:local was rebuilt in place.
kubectl -n "$KUBE_NAMESPACE" rollout restart deployment/control-plane >/dev/null

if [[ "$WAIT_ROLLOUT" == "1" ]]; then
  if ! kubectl -n "$KUBE_NAMESPACE" rollout status deployment/control-plane --timeout=120s; then
    kubectl -n "$KUBE_NAMESPACE" get pods -l app=control-plane
    exit 1
  fi
fi

kubectl -n "$KUBE_NAMESPACE" get pods -l app=control-plane \
  -o custom-columns=NAME:.metadata.name,READY:.status.containerStatuses[0].ready,IMAGE:.spec.containers[0].image,IMAGE_ID:.status.containerStatuses[0].imageID,STATUS:.status.phase

echo
echo "next: scripts/dev-webhook-ready.sh"
