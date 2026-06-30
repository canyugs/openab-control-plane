#!/usr/bin/env bash
# Create, update, scale, or delete local OpenAB bot deployments.
set -euo pipefail

KUBE_NAMESPACE="${KUBE_NAMESPACE:-oabcp-local}"
KUBE_CONTEXT="${KUBE_CONTEXT:-docker-desktop}"
BOT_NAMES="${BOT_NAMES:-chair,rev1,rev2}"
AGENT="${AGENT:-claude}"
BOT_AGENTS="${BOT_AGENTS:-}"
IMAGE="${IMAGE:-}"
AGENT_IMAGES="${AGENT_IMAGES:-}"
AGENT_SECRETS="${AGENT_SECRETS:-}"
CONTROL_PLANE_SERVICE="${CONTROL_PLANE_SERVICE:-control-plane}"
CONTROL_PLANE_PORT="${CONTROL_PLANE_PORT:-8090}"
CONFIG_BASE_URL="${CONFIG_BASE_URL:-}"
CREDENTIAL_ENV="${CREDENTIAL_ENV:-}"
SECRET_NAME="${SECRET_NAME:-}"
SECRET_KEY="${SECRET_KEY:-}"
CHAIR_BOT="${CHAIR_BOT:-chair}"
CHAIR_CREDENTIAL_ENV="${CHAIR_CREDENTIAL_ENV:-}"
CHAIR_SECRET_NAME="${CHAIR_SECRET_NAME:-}"
CHAIR_SECRET_KEY="${CHAIR_SECRET_KEY:-}"
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
  --bot-agents <map>         Per-bot agents, e.g. chair=kiro,rev1=claude.
  --image <image>            OpenAB image for every bot. Defaults for claude and kiro profiles.
  --agent-images <map>       Per-agent images, e.g. kiro=ghcr.io/...:kiro,cursor=...
  --bots <a,b,c>             Bot deployment names. Default: chair,rev1,rev2.
  --replicas <n>             Replicas per bot deployment. Default: 1.
  --credential-env <name>    Env var exposed to OpenAB, from a Kubernetes Secret.
  --secret-name <name>       Kubernetes Secret name carrying the credential.
  --secret-key <key>         Secret key name. Default: same as --credential-env.
  --agent-secret <spec>      Per-agent credential. Repeatable.
                             Format: agent=secret-name:ENV_NAME[:secret-key].
  --chair-bot <name>         Bot name that receives chair-only env. Default: chair.
  --chair-credential-env <n> Env var exposed only to the chair bot.
  --chair-secret-name <n>    Kubernetes Secret carrying the chair-only credential.
  --chair-secret-key <key>   Secret key name. Default: same as --chair-credential-env.
  --namespace <name>         Kubernetes namespace. Default: oabcp-local.
  --context <name>           Expected kubectl context. Default: docker-desktop.
  --any-context              Do not enforce the kubectl context.
  --control-plane <service>  Service name. Default: control-plane.
  --port <port>              Service port. Default: 8090.
  --config-base-url <url>    Full OCP base URL, overriding service/port.
  --no-wait                  Do not wait for rollout completion.
  --delete                   Delete all local OpenAB bot deployments.

Environment:
  KUBE_NAMESPACE, KUBE_CONTEXT, BOT_NAMES, AGENT, BOT_AGENTS, IMAGE,
  AGENT_IMAGES, AGENT_SECRETS, REPLICAS, CONFIG_BASE_URL,
  CREDENTIAL_ENV, SECRET_NAME, SECRET_KEY,
  CHAIR_BOT, CHAIR_CREDENTIAL_ENV, CHAIR_SECRET_NAME, CHAIR_SECRET_KEY.

Examples:
  kubectl -n oabcp-local create secret generic kiro-api \
    --from-literal=KIRO_API_KEY=<value>

  scripts/dev-deploy-bots.sh \
    --agent kiro \
    --secret-name kiro-api \
    --credential-env KIRO_API_KEY

  scripts/dev-deploy-bots.sh \
    --agent kiro \
    --secret-name kiro-api \
    --credential-env KIRO_API_KEY \
    --chair-secret-name gh-token \
    --chair-credential-env GH_TOKEN

  scripts/dev-deploy-bots.sh \
    --bot-agents chair=kiro,rev1=claude,rev2=claude \
    --agent-secret kiro=kiro-api:KIRO_API_KEY \
    --agent-secret claude=claude-oauth:CLAUDE_CODE_OAUTH_TOKEN \
    --chair-secret-name gh-token \
    --chair-credential-env GH_TOKEN
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

lookup_csv_map() {
  local map="$1"
  local key="$2"
  local default_value="${3:-}"
  local entry entry_key entry_value
  local -a entries=()
  if [[ -z "$map" ]]; then
    printf '%s' "$default_value"
    return
  fi
  IFS=',' read -r -a entries <<<"$map"
  for entry in "${entries[@]}"; do
    entry=$(printf '%s' "$entry" | xargs)
    [[ -n "$entry" ]] || continue
    [[ "$entry" == *"="* ]] || die "invalid mapping '$entry' (expected key=value)"
    entry_key=$(printf '%s' "${entry%%=*}" | xargs)
    entry_value="${entry#*=}"
    if [[ "$entry_key" == "$key" ]]; then
      printf '%s' "$entry_value"
      return
    fi
  done
  printf '%s' "$default_value"
}

image_for_agent() {
  local agent="$1"
  local image
  if [[ -n "$IMAGE" ]]; then
    printf '%s' "$IMAGE"
    return
  fi
  image=$(lookup_csv_map "$AGENT_IMAGES" "$agent" "")
  if [[ -z "$image" ]]; then
    image=$(default_image_for_agent "$agent")
  fi
  printf '%s' "$image"
}

resolve_agent_secret() {
  local agent="$1"
  local spec rest
  AGENT_SECRET_NAME=""
  AGENT_CREDENTIAL_ENV=""
  AGENT_SECRET_KEY=""

  spec=$(lookup_csv_map "$AGENT_SECRETS" "$agent" "")
  # Common local convenience: use the kiro-api secret when it already exists.
  if [[ -z "$spec" && "$agent" == "kiro" ]] &&
    kubectl -n "$KUBE_NAMESPACE" get secret kiro-api >/dev/null 2>&1; then
    spec="kiro-api:KIRO_API_KEY:KIRO_API_KEY"
  fi
  [[ -n "$spec" ]] || return
  [[ "$spec" == *:* ]] ||
    die "invalid --agent-secret for '$agent' (expected agent=secret-name:ENV_NAME[:secret-key])"

  AGENT_SECRET_NAME="${spec%%:*}"
  rest="${spec#*:}"
  AGENT_CREDENTIAL_ENV="${rest%%:*}"
  if [[ "$rest" == *:* ]]; then
    AGENT_SECRET_KEY="${rest#*:}"
  else
    AGENT_SECRET_KEY="$AGENT_CREDENTIAL_ENV"
  fi

  [[ -n "$AGENT_SECRET_NAME" ]] || die "agent secret for '$agent' has an empty secret name"
  [[ -n "$AGENT_CREDENTIAL_ENV" ]] || die "agent secret for '$agent' has an empty env name"
  [[ -n "$AGENT_SECRET_KEY" ]] || die "agent secret for '$agent' has an empty secret key"
}

append_secret_env_entry() {
  local env_name="$1"
  local secret_name="$2"
  local secret_key="$3"
  env_entries="${env_entries}            - name: $env_name
              valueFrom:
                secretKeyRef:
                  name: $secret_name
                  key: $secret_key
"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --agent) AGENT="${2:?--agent needs a value}"; shift 2 ;;
    --agent=*) AGENT="${1#*=}"; shift ;;
    --bot-agents) BOT_AGENTS="${2:?--bot-agents needs a value}"; shift 2 ;;
    --bot-agents=*) BOT_AGENTS="${1#*=}"; shift ;;
    --image) IMAGE="${2:?--image needs a value}"; shift 2 ;;
    --image=*) IMAGE="${1#*=}"; shift ;;
    --agent-images) AGENT_IMAGES="${2:?--agent-images needs a value}"; shift 2 ;;
    --agent-images=*) AGENT_IMAGES="${1#*=}"; shift ;;
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
    --agent-secret) AGENT_SECRETS="${AGENT_SECRETS:+$AGENT_SECRETS,}${2:?--agent-secret needs a value}"; shift 2 ;;
    --agent-secret=*) AGENT_SECRETS="${AGENT_SECRETS:+$AGENT_SECRETS,}${1#*=}"; shift ;;
    --chair-bot) CHAIR_BOT="${2:?--chair-bot needs a value}"; shift 2 ;;
    --chair-bot=*) CHAIR_BOT="${1#*=}"; shift ;;
    --chair-credential-env) CHAIR_CREDENTIAL_ENV="${2:?--chair-credential-env needs a value}"; shift 2 ;;
    --chair-credential-env=*) CHAIR_CREDENTIAL_ENV="${1#*=}"; shift ;;
    --chair-secret-name) CHAIR_SECRET_NAME="${2:?--chair-secret-name needs a value}"; shift 2 ;;
    --chair-secret-name=*) CHAIR_SECRET_NAME="${1#*=}"; shift ;;
    --chair-secret-key) CHAIR_SECRET_KEY="${2:?--chair-secret-key needs a value}"; shift 2 ;;
    --chair-secret-key=*) CHAIR_SECRET_KEY="${1#*=}"; shift ;;
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

if [[ -z "$SECRET_KEY" && -n "$CREDENTIAL_ENV" ]]; then
  SECRET_KEY="$CREDENTIAL_ENV"
fi
if [[ -z "$CHAIR_SECRET_KEY" && -n "$CHAIR_CREDENTIAL_ENV" ]]; then
  CHAIR_SECRET_KEY="$CHAIR_CREDENTIAL_ENV"
fi

if [[ -n "$SECRET_NAME" || -n "$CREDENTIAL_ENV" || -n "$SECRET_KEY" ]]; then
  [[ -n "$SECRET_NAME" ]] || die "--secret-name is required when credential env is set"
  [[ -n "$CREDENTIAL_ENV" ]] || die "--credential-env is required when secret is set"
  [[ -n "$SECRET_KEY" ]] || die "--secret-key is required when secret is set"
  kubectl -n "$KUBE_NAMESPACE" get secret "$SECRET_NAME" >/dev/null ||
    die "secret '$SECRET_NAME' does not exist in namespace '$KUBE_NAMESPACE'"
fi

if [[ -n "$CHAIR_SECRET_NAME" || -n "$CHAIR_CREDENTIAL_ENV" || -n "$CHAIR_SECRET_KEY" ]]; then
  [[ -n "$CHAIR_SECRET_NAME" ]] || die "--chair-secret-name is required when chair credential env is set"
  [[ -n "$CHAIR_CREDENTIAL_ENV" ]] || die "--chair-credential-env is required when chair secret is set"
  [[ -n "$CHAIR_SECRET_KEY" ]] || die "--chair-secret-key is required when chair secret is set"
  kubectl -n "$KUBE_NAMESPACE" get secret "$CHAIR_SECRET_NAME" >/dev/null ||
    die "secret '$CHAIR_SECRET_NAME' does not exist in namespace '$KUBE_NAMESPACE'"
fi

kubectl create namespace "$KUBE_NAMESPACE" --dry-run=client -o yaml | kubectl apply -f - >/dev/null

IFS=',' read -r -a bots <<<"$BOT_NAMES"
for raw_bot in "${bots[@]}"; do
  bot=$(printf '%s' "$raw_bot" | xargs)
  [[ -n "$bot" ]] || continue
  bot_agent=$(lookup_csv_map "$BOT_AGENTS" "$bot" "$AGENT")
  bot_image=$(image_for_agent "$bot_agent")
  [[ -n "$bot_image" ]] ||
    die "no default image for agent '$bot_agent' (bot '$bot'); pass --image or --agent-images"
  if [[ -n "$CONFIG_BASE_URL" ]]; then
    config_url="${CONFIG_BASE_URL%/}/bot-config/$bot?agent=$bot_agent"
  else
    config_url="http://$CONTROL_PLANE_SERVICE:$CONTROL_PLANE_PORT/bot-config/$bot?agent=$bot_agent"
  fi
  env_entries=""
  if [[ -n "$SECRET_NAME" ]]; then
    append_secret_env_entry "$CREDENTIAL_ENV" "$SECRET_NAME" "$SECRET_KEY"
  else
    resolve_agent_secret "$bot_agent"
    if [[ -n "$AGENT_SECRET_NAME" ]]; then
      kubectl -n "$KUBE_NAMESPACE" get secret "$AGENT_SECRET_NAME" >/dev/null ||
        die "secret '$AGENT_SECRET_NAME' does not exist in namespace '$KUBE_NAMESPACE'"
      append_secret_env_entry "$AGENT_CREDENTIAL_ENV" "$AGENT_SECRET_NAME" "$AGENT_SECRET_KEY"
    fi
  fi
  if [[ "$bot" == "$CHAIR_BOT" && -n "$CHAIR_SECRET_NAME" ]]; then
    append_secret_env_entry "$CHAIR_CREDENTIAL_ENV" "$CHAIR_SECRET_NAME" "$CHAIR_SECRET_KEY"
  fi
  env_yaml=""
  if [[ -n "$env_entries" ]]; then
    env_yaml="          env:
$env_entries"
  fi

  echo "applying OpenAB bot deployment: $bot ($bot_agent, $bot_image)"
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
          image: $bot_image
          imagePullPolicy: IfNotPresent
          command:
            - openab
            - run
            - -c
            - $config_url
$env_yaml
YAML

  # Local OCP DBs are often recreated, which changes /bot-config tokens without
  # changing the Deployment spec. Restart so pods refetch config on every run.
  if [[ "$REPLICAS" != "0" ]]; then
    kubectl -n "$KUBE_NAMESPACE" rollout restart "deployment/$bot" >/dev/null
  fi

  if [[ "$WAIT_ROLLOUT" == "1" && "$REPLICAS" != "0" ]]; then
    kubectl -n "$KUBE_NAMESPACE" rollout status "deployment/$bot" --timeout=180s
  fi
done

kubectl -n "$KUBE_NAMESPACE" get pods -l app=openab-bot \
  -o custom-columns=NAME:.metadata.name,READY:.status.containerStatuses[0].ready,IMAGE:.spec.containers[0].image,STATUS:.status.phase
