#!/usr/bin/env bash
# Create, update, scale, or delete local OpenAB bot deployments.
set -euo pipefail

KUBE_NAMESPACE="${KUBE_NAMESPACE:-oabcp-local}"
KUBE_CONTEXT="${KUBE_CONTEXT:-docker-desktop}"
BOT_NAMES="${BOT_NAMES:-chair,rev1,rev2}"
AGENT="${AGENT:-claude}"
IMAGE="${IMAGE:-}"
CONTROL_PLANE_SERVICE="${CONTROL_PLANE_SERVICE:-control-plane}"
CONTROL_PLANE_PORT="${CONTROL_PLANE_PORT:-8090}"
CONFIG_BASE_URL="${CONFIG_BASE_URL:-}"
CREDENTIAL_ENV="${CREDENTIAL_ENV:-}"
SECRET_NAME="${SECRET_NAME:-}"
SECRET_KEY="${SECRET_KEY:-}"
REPLICAS="${REPLICAS:-1}"
WAIT_ROLLOUT=1
CHECK_CONTEXT=1
DELETE=0

usage() {
  cat <<'USAGE'
Usage:
  scripts/dev-deploy-bots.sh [--agent <profile>] [--image <image>]
  scripts/dev-deploy-bots.sh --replicas 0
  scripts/dev-deploy-bots.sh --delete

Options:
  --agent <profile>          Agent profile passed to /bot-config?agent=. Default: claude.
  --image <image>            OpenAB image. Defaults for claude and kiro profiles.
  --bots <a,b,c>             Bot deployment names. Default: chair,rev1,rev2.
  --replicas <n>             Replicas per bot deployment. Default: 1.
  --credential-env <name>    Env var exposed to OpenAB, from a Kubernetes Secret.
  --secret-name <name>       Kubernetes Secret name carrying the credential.
  --secret-key <key>         Secret key name. Default: same as --credential-env.
  --namespace <name>         Kubernetes namespace. Default: oabcp-local.
  --context <name>           Expected kubectl context. Default: docker-desktop.
  --any-context              Do not enforce the kubectl context.
  --control-plane <service>  Service name. Default: control-plane.
  --port <port>              Service port. Default: 8090.
  --config-base-url <url>    Full OCP base URL, overriding service/port.
  --no-wait                  Do not wait for rollout completion.
  --delete                   Delete all local OpenAB bot deployments.

Environment:
  KUBE_NAMESPACE, KUBE_CONTEXT, BOT_NAMES, AGENT, IMAGE, REPLICAS,
  CONFIG_BASE_URL, CREDENTIAL_ENV, SECRET_NAME, SECRET_KEY.

Examples:
  kubectl -n oabcp-local create secret generic kiro-api \
    --from-literal=KIRO_API_KEY=<value>

  scripts/dev-deploy-bots.sh \
    --agent kiro \
    --secret-name kiro-api \
    --credential-env KIRO_API_KEY
USAGE
}

die() {
  echo "error: $*" >&2
  exit 1
}

need() {
  command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"
}

default_image_for_agent() {
  case "$1" in
    claude|claude-agent-acp) echo "ghcr.io/openabdev/openab:0.9.0-beta.3-claude" ;;
    kiro) echo "ghcr.io/openabdev/openab:0.9.0-beta.3-kiro" ;;
    *) echo "" ;;
  esac
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --agent) AGENT="${2:?--agent needs a value}"; shift 2 ;;
    --agent=*) AGENT="${1#*=}"; shift ;;
    --image) IMAGE="${2:?--image needs a value}"; shift 2 ;;
    --image=*) IMAGE="${1#*=}"; shift ;;
    --bots) BOT_NAMES="${2:?--bots needs a value}"; shift 2 ;;
    --bots=*) BOT_NAMES="${1#*=}"; shift ;;
    --replicas) REPLICAS="${2:?--replicas needs a value}"; shift 2 ;;
    --replicas=*) REPLICAS="${1#*=}"; shift ;;
    --credential-env) CREDENTIAL_ENV="${2:?--credential-env needs a value}"; shift 2 ;;
    --credential-env=*) CREDENTIAL_ENV="${1#*=}"; shift ;;
    --secret-name) SECRET_NAME="${2:?--secret-name needs a value}"; shift 2 ;;
    --secret-name=*) SECRET_NAME="${1#*=}"; shift ;;
    --secret-key) SECRET_KEY="${2:?--secret-key needs a value}"; shift 2 ;;
    --secret-key=*) SECRET_KEY="${1#*=}"; shift ;;
    --namespace) KUBE_NAMESPACE="${2:?--namespace needs a value}"; shift 2 ;;
    --namespace=*) KUBE_NAMESPACE="${1#*=}"; shift ;;
    --context) KUBE_CONTEXT="${2:?--context needs a value}"; shift 2 ;;
    --context=*) KUBE_CONTEXT="${1#*=}"; shift ;;
    --any-context) CHECK_CONTEXT=0; shift ;;
    --control-plane) CONTROL_PLANE_SERVICE="${2:?--control-plane needs a value}"; shift 2 ;;
    --control-plane=*) CONTROL_PLANE_SERVICE="${1#*=}"; shift ;;
    --port) CONTROL_PLANE_PORT="${2:?--port needs a value}"; shift 2 ;;
    --port=*) CONTROL_PLANE_PORT="${1#*=}"; shift ;;
    --config-base-url) CONFIG_BASE_URL="${2:?--config-base-url needs a value}"; shift 2 ;;
    --config-base-url=*) CONFIG_BASE_URL="${1#*=}"; shift ;;
    --no-wait) WAIT_ROLLOUT=0; shift ;;
    --delete) DELETE=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
done

need kubectl

if [[ "$CHECK_CONTEXT" == "1" ]]; then
  current_context=$(kubectl config current-context)
  [[ "$current_context" == "$KUBE_CONTEXT" ]] ||
    die "kubectl context is '$current_context', expected '$KUBE_CONTEXT' (or pass --any-context)"
fi

if [[ "$DELETE" == "1" ]]; then
  kubectl -n "$KUBE_NAMESPACE" delete deployment -l app=openab-bot --ignore-not-found
  exit 0
fi

if [[ -z "$IMAGE" ]]; then
  IMAGE=$(default_image_for_agent "$AGENT")
fi
[[ -n "$IMAGE" ]] || die "no default image for agent '$AGENT'; pass --image"

if [[ -z "$SECRET_KEY" && -n "$CREDENTIAL_ENV" ]]; then
  SECRET_KEY="$CREDENTIAL_ENV"
fi

# Common local convenience: use the kiro-api secret when it already exists.
if [[ "$AGENT" == "kiro" && -z "$SECRET_NAME" ]] &&
  kubectl -n "$KUBE_NAMESPACE" get secret kiro-api >/dev/null 2>&1; then
  SECRET_NAME="kiro-api"
  CREDENTIAL_ENV="${CREDENTIAL_ENV:-KIRO_API_KEY}"
  SECRET_KEY="${SECRET_KEY:-KIRO_API_KEY}"
fi

if [[ -n "$SECRET_NAME" || -n "$CREDENTIAL_ENV" || -n "$SECRET_KEY" ]]; then
  [[ -n "$SECRET_NAME" ]] || die "--secret-name is required when credential env is set"
  [[ -n "$CREDENTIAL_ENV" ]] || die "--credential-env is required when secret is set"
  [[ -n "$SECRET_KEY" ]] || die "--secret-key is required when secret is set"
  kubectl -n "$KUBE_NAMESPACE" get secret "$SECRET_NAME" >/dev/null ||
    die "secret '$SECRET_NAME' does not exist in namespace '$KUBE_NAMESPACE'"
fi

credential_env_yaml=""
if [[ -n "$SECRET_NAME" ]]; then
  credential_env_yaml="          env:
            - name: $CREDENTIAL_ENV
              valueFrom:
                secretKeyRef:
                  name: $SECRET_NAME
                  key: $SECRET_KEY"
fi

kubectl create namespace "$KUBE_NAMESPACE" --dry-run=client -o yaml | kubectl apply -f - >/dev/null

IFS=',' read -r -a bots <<<"$BOT_NAMES"
for raw_bot in "${bots[@]}"; do
  bot=$(printf '%s' "$raw_bot" | xargs)
  [[ -n "$bot" ]] || continue
  if [[ -n "$CONFIG_BASE_URL" ]]; then
    config_url="${CONFIG_BASE_URL%/}/bot-config/$bot?agent=$AGENT"
  else
    config_url="http://$CONTROL_PLANE_SERVICE:$CONTROL_PLANE_PORT/bot-config/$bot?agent=$AGENT"
  fi

  echo "applying OpenAB bot deployment: $bot ($AGENT, $IMAGE)"
  kubectl -n "$KUBE_NAMESPACE" apply -f - >/dev/null <<YAML
apiVersion: apps/v1
kind: Deployment
metadata:
  name: $bot
  labels:
    app: openab-bot
    bot: $bot
spec:
  replicas: $REPLICAS
  selector:
    matchLabels:
      app: openab-bot
      bot: $bot
  template:
    metadata:
      labels:
        app: openab-bot
        bot: $bot
    spec:
      containers:
        - name: openab
          image: $IMAGE
          imagePullPolicy: IfNotPresent
          command:
            - openab
            - run
            - -c
            - $config_url
$credential_env_yaml
YAML

  if [[ "$WAIT_ROLLOUT" == "1" && "$REPLICAS" != "0" ]]; then
    kubectl -n "$KUBE_NAMESPACE" rollout status "deployment/$bot" --timeout=180s
  fi
done

kubectl -n "$KUBE_NAMESPACE" get pods -l app=openab-bot \
  -o custom-columns=NAME:.metadata.name,READY:.status.containerStatuses[0].ready,IMAGE:.spec.containers[0].image,STATUS:.status.phase
