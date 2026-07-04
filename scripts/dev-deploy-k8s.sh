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
OABCP_WS_URL="${OABCP_WS_URL:-ws://control-plane:8090/ws}"
OABCP_AGENT_COMMAND="${OABCP_AGENT_COMMAND:-}"
OABCP_AGENT_PROFILES="${OABCP_AGENT_PROFILES:-}"
OABCP_AGENT_WORKING_DIR="${OABCP_AGENT_WORKING_DIR:-}"
OABCP_AGENT_INHERIT_ENV="${OABCP_AGENT_INHERIT_ENV:-}"
OABCP_SESSION_CLOSE_WEBHOOK="${OABCP_SESSION_CLOSE_WEBHOOK:-}"
REMOTE_PORT="${REMOTE_PORT:-8090}"
WAIT_ROLLOUT=1
CHECK_CONTEXT=1
CHECK_IMAGE_ID=1

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
  --agent-command <p>     Default /bot-config agent profile when ?agent= is absent.
  --agent-profiles-json <j>
                          JSON object overriding or adding OAB agent profiles.
  --agent-profiles-file <p>
                          Read OABCP_AGENT_PROFILES JSON from a file.
  --agent-working-dir <d> Force [agent].working_dir for served bot configs.
  --agent-inherit-env <c> Extra env names appended to [agent].inherit_env.
  --no-wait              Do not wait for rollout completion.
  --skip-image-id-check  Do not compare the pod imageID with the local Docker image.

Environment:
  IMAGE, KUBE_NAMESPACE, KUBE_CONTEXT, OABCP_API_KEY,
  GITHUB_WEBHOOK_SECRET, OABCP_BOTS, OABCP_COUNCIL_ROSTER, OABCP_WS_URL,
  OABCP_AGENT_COMMAND, OABCP_AGENT_PROFILES, OABCP_AGENT_WORKING_DIR,
  OABCP_AGENT_INHERIT_ENV, OABCP_SESSION_CLOSE_WEBHOOK.
USAGE
}

die() {
  echo "error: $*" >&2
  exit 1
}

need() {
  command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"
}

yaml_quote() {
  local value="$1"
  [[ "$value" != *$'\n'* && "$value" != *$'\r'* ]] ||
    die "YAML scalar cannot contain newlines"
  value="${value//\\/\\\\}"
  value="${value//\"/\\\"}"
  printf '"%s"' "$value"
}

append_optional_env_yaml() {
  local name="$1"
  local value="$2"
  optional_env_yaml="${optional_env_yaml}            - name: $name
              value: $(yaml_quote "$value")
"
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
    --agent-command) OABCP_AGENT_COMMAND="${2:?--agent-command needs a value}"; shift 2 ;;
    --agent-command=*) OABCP_AGENT_COMMAND="${1#*=}"; shift ;;
    --agent-profiles-json) OABCP_AGENT_PROFILES="${2:?--agent-profiles-json needs a value}"; shift 2 ;;
    --agent-profiles-json=*) OABCP_AGENT_PROFILES="${1#*=}"; shift ;;
    --agent-profiles-file)
      profile_file="${2:?--agent-profiles-file needs a path}"
      [[ -r "$profile_file" ]] || die "agent profiles file is not readable: $profile_file"
      OABCP_AGENT_PROFILES="$(tr -d '\r\n' <"$profile_file")"
      shift 2
      ;;
    --agent-profiles-file=*)
      profile_file="${1#*=}"
      [[ -r "$profile_file" ]] || die "agent profiles file is not readable: $profile_file"
      OABCP_AGENT_PROFILES="$(tr -d '\r\n' <"$profile_file")"
      shift
      ;;
    --agent-working-dir) OABCP_AGENT_WORKING_DIR="${2:?--agent-working-dir needs a value}"; shift 2 ;;
    --agent-working-dir=*) OABCP_AGENT_WORKING_DIR="${1#*=}"; shift ;;
    --agent-inherit-env) OABCP_AGENT_INHERIT_ENV="${2:?--agent-inherit-env needs a value}"; shift 2 ;;
    --agent-inherit-env=*) OABCP_AGENT_INHERIT_ENV="${1#*=}"; shift ;;
    --no-wait) WAIT_ROLLOUT=0; shift ;;
    --skip-image-id-check) CHECK_IMAGE_ID=0; shift ;;
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

optional_env_yaml=""
[[ -n "$OABCP_AGENT_COMMAND" ]] && append_optional_env_yaml OABCP_AGENT_COMMAND "$OABCP_AGENT_COMMAND"
[[ -n "$OABCP_AGENT_PROFILES" ]] && append_optional_env_yaml OABCP_AGENT_PROFILES "$OABCP_AGENT_PROFILES"
[[ -n "$OABCP_AGENT_WORKING_DIR" ]] && append_optional_env_yaml OABCP_AGENT_WORKING_DIR "$OABCP_AGENT_WORKING_DIR"
[[ -n "$OABCP_AGENT_INHERIT_ENV" ]] && append_optional_env_yaml OABCP_AGENT_INHERIT_ENV "$OABCP_AGENT_INHERIT_ENV"
[[ -n "$OABCP_SESSION_CLOSE_WEBHOOK" ]] && append_optional_env_yaml OABCP_SESSION_CLOSE_WEBHOOK "$OABCP_SESSION_CLOSE_WEBHOOK"
restart_nonce=$(date +%s)

if ! kubectl -n "$KUBE_NAMESPACE" get pvc control-plane-data >/dev/null 2>&1; then
  echo "note: first run with the durable /data PVC — the plane starts from an empty" >&2
  echo "      DB and re-mints bot tokens once. Restart existing bots after this deploy" >&2
  echo "      so they re-fetch /bot-config; later plane restarts are non-destructive." >&2
fi

echo "applying local control-plane deployment with image: $IMAGE"
kubectl -n "$KUBE_NAMESPACE" apply -f - >/dev/null <<YAML
apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: control-plane-data
  labels:
    app: control-plane
spec:
  accessModes:
    - ReadWriteOnce
  resources:
    requests:
      storage: 1Gi
---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: control-plane
  labels:
    app: control-plane
spec:
  replicas: 1
  # Recreate, not RollingUpdate: the RWO PVC can't attach to the new pod while
  # the old one still holds it, so a rolling swap would deadlock on the mount.
  strategy:
    type: Recreate
  selector:
    matchLabels:
      app: control-plane
  template:
    metadata:
      annotations:
        oabcp.dev/restarted-at: "$restart_nonce"
      labels:
        app: control-plane
    spec:
      containers:
        - name: openab-control-plane
          image: "$IMAGE"
          imagePullPolicy: IfNotPresent
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
            - name: OABCP_WS_URL
              value: "$OABCP_WS_URL"
            - name: OABCP_DB
              value: "/data/plane.db"
$optional_env_yaml
          volumeMounts:
            - name: data
              mountPath: /data
      volumes:
        - name: data
          persistentVolumeClaim:
            claimName: control-plane-data
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

if [[ "$WAIT_ROLLOUT" == "1" ]]; then
  if ! kubectl -n "$KUBE_NAMESPACE" rollout status deployment/control-plane --timeout=120s; then
    kubectl -n "$KUBE_NAMESPACE" get pods -l app=control-plane
    exit 1
  fi
fi

kubectl -n "$KUBE_NAMESPACE" get pods -l app=control-plane \
  -o custom-columns=NAME:.metadata.name,READY:.status.containerStatuses[0].ready,IMAGE:.spec.containers[0].image,IMAGE_ID:.status.containerStatuses[0].imageID,STATUS:.status.phase

if [[ "$CHECK_IMAGE_ID" == "1" ]] && [[ "$IMAGE" != localhost:* ]] && [[ "$IMAGE" != 127.0.0.1:* ]] && command -v docker >/dev/null 2>&1; then
  local_image_id=$(docker image inspect "$IMAGE" --format '{{.Id}}' 2>/dev/null || true)
  if [[ -n "$local_image_id" ]]; then
    pod_image_ids=$(kubectl -n "$KUBE_NAMESPACE" get pods -l app=control-plane \
      -o jsonpath='{range .items[?(@.status.phase=="Running")]}{.status.containerStatuses[0].imageID}{"\n"}{end}')
    if [[ "$pod_image_ids" != *"$local_image_id"* ]]; then
      echo >&2
      echo "error: Kubernetes is not running the local Docker image for $IMAGE" >&2
      echo "local Docker image: $local_image_id" >&2
      echo "pod image IDs:" >&2
      printf '%s\n' "$pod_image_ids" >&2
      echo "The webhook readiness probe would likely test an old binary." >&2
      echo >&2
      echo "If a fresh scripts/dev-build-image.sh + retry still shows this (same stale" >&2
      echo "pod image ID every time, or ErrImageNeverPull on a brand-new unique tag)," >&2
      echo "Docker Desktop's dockerd->Kubernetes image bridge is stuck, not the build." >&2
      echo "Fix: 'docker desktop restart', then redeploy." >&2
      exit 1
    fi
  fi
fi

echo
echo "next: scripts/dev-webhook-ready.sh"
