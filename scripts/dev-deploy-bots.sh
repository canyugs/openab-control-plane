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
EXTRA_SECRETS="${EXTRA_SECRETS:-}"
BOT_SECRETS="${BOT_SECRETS:-}"
STEERING_FILE="${STEERING_FILE:-}"
STEERING_CONFIGMAP="${STEERING_CONFIGMAP:-openab-pr-review-steering}"
STEERING_KEY="${STEERING_KEY:-openab-pr-review.md}"
STEERING_MOUNT_PATH="${STEERING_MOUNT_PATH:-}"
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
CHAIR_GITHUB_APP_SECRET="${CHAIR_GITHUB_APP_SECRET:-}"
CHAIR_GITHUB_APP_HOME="${CHAIR_GITHUB_APP_HOME:-}"
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
  --extra-secret <spec>      Secret exposed to every bot. Repeatable.
                             Format: secret-name:ENV_NAME[:secret-key].
  --bot-secret <spec>        Secret exposed to one bot. Repeatable.
                             Format: bot=secret-name:ENV_NAME[:secret-key].
  --chair-bot <name>         Bot name that receives chair-only env. Default: chair.
  --chair-credential-env <n> Env var exposed only to the chair bot.
  --chair-secret-name <n>    Kubernetes Secret carrying the chair-only credential.
  --chair-secret-key <key>   Secret key name. Default: same as --chair-credential-env.
  --chair-github-app-secret <n>
                             Chair-only Secret created by dev-sync-gh-app-secret.sh.
                             Mounts .github-app.pem and bin/get-gh-app-token.sh.
  --chair-github-app-home <p>
                             Home/working dir for App files. Defaults by agent
                             (/home/agent for kiro, /home/node otherwise).
  --steering-file <path>     Create/update a ConfigMap from this steering file and
                             mount it into each bot. Kiro defaults to
                             /home/agent/.kiro/steering; other agents default to
                             /home/node/AGENTS.md.
  --steering-configmap <n>   ConfigMap name for --steering-file.
  --steering-key <name>      ConfigMap key / mounted file name. Default: openab-pr-review.md.
  --steering-mount-path <p>  Override mount path. A *.md path is mounted as one file;
                             any other path is mounted as a directory.
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
  EXTRA_SECRETS, BOT_SECRETS, STEERING_FILE, STEERING_CONFIGMAP,
  STEERING_KEY, STEERING_MOUNT_PATH,
  CREDENTIAL_ENV, SECRET_NAME, SECRET_KEY,
  CHAIR_BOT, CHAIR_CREDENTIAL_ENV, CHAIR_SECRET_NAME, CHAIR_SECRET_KEY,
  CHAIR_GITHUB_APP_SECRET, CHAIR_GITHUB_APP_HOME.

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

  scripts/dev-deploy-bots.sh \
    --agent kiro \
    --agent-secret kiro=kiro-api:KIRO_API_KEY \
    --bot-secret rev1=gh-token:GH_TOKEN \
    --bot-secret rev2=gh-token:GH_TOKEN \
    --chair-github-app-secret github-app-chair \
    --steering-file docs/steering/pr-review.md
USAGE
}

die() {
  echo "error: $*" >&2
  exit 1
}

need() {
  command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"
}

# A "Running" pod only means the openab process started — not that it reached
# the plane. A stale/mismatched config-base-url (e.g. pointing at a host OCP
# that isn't running) leaves the pod stuck retrying forever with no non-zero
# exit anywhere. Poll its own logs for the gateway handshake outcome instead.
check_gateway_connected() {
  local bot="$1" deadline=$((SECONDS + 15)) logs
  while ((SECONDS < deadline)); do
    logs=$(kubectl -n "$KUBE_NAMESPACE" logs "deployment/$bot" --tail=20 2>/dev/null || true)
    if grep -q "connected to gateway" <<<"$logs"; then
      return 0
    fi
    if grep -qE "Connection refused|gateway connection failed" <<<"$logs"; then
      echo "warning: $bot cannot reach the gateway at its configured URL — check --config-base-url and that '$CONTROL_PLANE_SERVICE' is actually deployed (dev-deploy-k8s.sh) or reachable" >&2
      return 1
    fi
    sleep 2
  done
  echo "warning: $bot did not confirm a gateway connection within 15s — check: kubectl -n $KUBE_NAMESPACE logs deployment/$bot" >&2
  return 1
}

yaml_quote() {
  local value="$1"
  [[ "$value" != *$'\n'* && "$value" != *$'\r'* ]] ||
    die "YAML scalar cannot contain newlines"
  value="${value//\\/\\\\}"
  value="${value//\"/\\\"}"
  printf '"%s"' "$value"
}

default_image_for_agent() {
  case "$1" in
    claude|claude-agent-acp) echo "ghcr.io/openabdev/openab:0.9.0-beta.6-claude" ;;
    kiro) echo "ghcr.io/openabdev/openab:0.9.0-beta.6-kiro" ;;
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
  [[ -n "$spec" ]] || return 0
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
  if [[ ",${env_names:-}," == *",$env_name,"* ]]; then
    die "duplicate env var '$env_name' in bot deployment"
  fi
  env_names="${env_names:+$env_names,}$env_name"
  env_entries="${env_entries}            - name: $env_name
              valueFrom:
                secretKeyRef:
                  name: $secret_name
                  key: $secret_key
"
}

append_secret_spec_env_entries() {
  local raw_spec spec secret_name env_name secret_key rest
  local -a specs=()
  [[ -n "$EXTRA_SECRETS" ]] || return 0
  IFS=',' read -r -a specs <<<"$EXTRA_SECRETS"
  for raw_spec in "${specs[@]}"; do
    spec=$(printf '%s' "$raw_spec" | xargs)
    [[ -n "$spec" ]] || continue
    [[ "$spec" == *:* ]] ||
      die "invalid --extra-secret '$spec' (expected secret-name:ENV_NAME[:secret-key])"
    secret_name="${spec%%:*}"
    rest="${spec#*:}"
    env_name="${rest%%:*}"
    if [[ "$rest" == *:* ]]; then
      secret_key="${rest#*:}"
    else
      secret_key="$env_name"
    fi
    [[ -n "$secret_name" ]] || die "--extra-secret has an empty secret name"
    [[ -n "$env_name" ]] || die "--extra-secret has an empty env name"
    [[ -n "$secret_key" ]] || die "--extra-secret has an empty secret key"
    kubectl -n "$KUBE_NAMESPACE" get secret "$secret_name" >/dev/null ||
      die "secret '$secret_name' does not exist in namespace '$KUBE_NAMESPACE'"
    append_secret_env_entry "$env_name" "$secret_name" "$secret_key"
  done
}

append_bot_secret_entries() {
  local bot="$1"
  local raw_spec spec spec_bot rest secret_name env_name secret_key secret_rest
  local -a specs=()
  [[ -n "$BOT_SECRETS" ]] || return 0
  IFS=',' read -r -a specs <<<"$BOT_SECRETS"
  for raw_spec in "${specs[@]}"; do
    spec=$(printf '%s' "$raw_spec" | xargs)
    [[ -n "$spec" ]] || continue
    [[ "$spec" == *"="* ]] ||
      die "invalid --bot-secret '$spec' (expected bot=secret-name:ENV_NAME[:secret-key])"
    spec_bot=$(printf '%s' "${spec%%=*}" | xargs)
    [[ "$spec_bot" == "$bot" ]] || continue
    rest="${spec#*=}"
    [[ "$rest" == *:* ]] ||
      die "invalid --bot-secret '$spec' (expected bot=secret-name:ENV_NAME[:secret-key])"
    secret_name="${rest%%:*}"
    secret_rest="${rest#*:}"
    env_name="${secret_rest%%:*}"
    if [[ "$secret_rest" == *:* ]]; then
      secret_key="${secret_rest#*:}"
    else
      secret_key="$env_name"
    fi
    [[ -n "$secret_name" ]] || die "--bot-secret has an empty secret name"
    [[ -n "$env_name" ]] || die "--bot-secret has an empty env name"
    [[ -n "$secret_key" ]] || die "--bot-secret has an empty secret key"
    kubectl -n "$KUBE_NAMESPACE" get secret "$secret_name" >/dev/null ||
      die "secret '$secret_name' does not exist in namespace '$KUBE_NAMESPACE'"
    append_secret_env_entry "$env_name" "$secret_name" "$secret_key"
  done
}

default_home_for_agent() {
  case "$1" in
    kiro) echo "/home/agent" ;;
    *) echo "/home/node" ;;
  esac
}

default_steering_mount_path_for_agent() {
  case "$1" in
    kiro) echo "/home/agent/.kiro/steering" ;;
    *) echo "/home/node/AGENTS.md" ;;
  esac
}

build_steering_yaml() {
  local bot_agent="$1"
  local mount_path
  [[ -n "$STEERING_FILE" ]] || return 0
  mount_path="${STEERING_MOUNT_PATH:-$(default_steering_mount_path_for_agent "$bot_agent")}"
  if [[ "$mount_path" == *.md ]]; then
    volume_mount_entries="${volume_mount_entries}            - name: openab-steering
              mountPath: $mount_path
              subPath: $STEERING_KEY
              readOnly: true
"
  else
    volume_mount_entries="${volume_mount_entries}            - name: openab-steering
              mountPath: $mount_path
              readOnly: true
"
  fi
  volume_entries="${volume_entries}        - name: openab-steering
          configMap:
            name: $STEERING_CONFIGMAP
"
}

build_chair_github_app_yaml() {
  local bot="$1"
  local bot_agent="$2"
  local home
  [[ "$bot" == "$CHAIR_BOT" ]] || return 0
  [[ -n "$CHAIR_GITHUB_APP_SECRET" ]] || return 0
  home="${CHAIR_GITHUB_APP_HOME:-$(default_home_for_agent "$bot_agent")}"
  volume_mount_entries="${volume_mount_entries}            - name: github-app-key
              mountPath: $home/.github-app.pem
              subPath: .github-app.pem
              readOnly: true
            - name: github-app-bin
              mountPath: $home/bin
              readOnly: true
"
  volume_entries="${volume_entries}        - name: github-app-key
          secret:
            secretName: $CHAIR_GITHUB_APP_SECRET
            items:
              - key: .github-app.pem
                path: .github-app.pem
                mode: 0444
        - name: github-app-bin
          secret:
            secretName: $CHAIR_GITHUB_APP_SECRET
            items:
              - key: get-gh-app-token.sh
                path: get-gh-app-token.sh
                mode: 0555
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
    --extra-secret) EXTRA_SECRETS="${EXTRA_SECRETS:+$EXTRA_SECRETS,}${2:?--extra-secret needs a value}"; shift 2 ;;
    --extra-secret=*) EXTRA_SECRETS="${EXTRA_SECRETS:+$EXTRA_SECRETS,}${1#*=}"; shift ;;
    --bot-secret) BOT_SECRETS="${BOT_SECRETS:+$BOT_SECRETS,}${2:?--bot-secret needs a value}"; shift 2 ;;
    --bot-secret=*) BOT_SECRETS="${BOT_SECRETS:+$BOT_SECRETS,}${1#*=}"; shift ;;
    --chair-bot) CHAIR_BOT="${2:?--chair-bot needs a value}"; shift 2 ;;
    --chair-bot=*) CHAIR_BOT="${1#*=}"; shift ;;
    --chair-credential-env) CHAIR_CREDENTIAL_ENV="${2:?--chair-credential-env needs a value}"; shift 2 ;;
    --chair-credential-env=*) CHAIR_CREDENTIAL_ENV="${1#*=}"; shift ;;
    --chair-secret-name) CHAIR_SECRET_NAME="${2:?--chair-secret-name needs a value}"; shift 2 ;;
    --chair-secret-name=*) CHAIR_SECRET_NAME="${1#*=}"; shift ;;
    --chair-secret-key) CHAIR_SECRET_KEY="${2:?--chair-secret-key needs a value}"; shift 2 ;;
    --chair-secret-key=*) CHAIR_SECRET_KEY="${1#*=}"; shift ;;
    --chair-github-app-secret) CHAIR_GITHUB_APP_SECRET="${2:?--chair-github-app-secret needs a value}"; shift 2 ;;
    --chair-github-app-secret=*) CHAIR_GITHUB_APP_SECRET="${1#*=}"; shift ;;
    --chair-github-app-home) CHAIR_GITHUB_APP_HOME="${2:?--chair-github-app-home needs a value}"; shift 2 ;;
    --chair-github-app-home=*) CHAIR_GITHUB_APP_HOME="${1#*=}"; shift ;;
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
    --steering-file) STEERING_FILE="${2:?--steering-file needs a value}"; shift 2 ;;
    --steering-file=*) STEERING_FILE="${1#*=}"; shift ;;
    --steering-configmap) STEERING_CONFIGMAP="${2:?--steering-configmap needs a value}"; shift 2 ;;
    --steering-configmap=*) STEERING_CONFIGMAP="${1#*=}"; shift ;;
    --steering-key) STEERING_KEY="${2:?--steering-key needs a value}"; shift 2 ;;
    --steering-key=*) STEERING_KEY="${1#*=}"; shift ;;
    --steering-mount-path) STEERING_MOUNT_PATH="${2:?--steering-mount-path needs a value}"; shift 2 ;;
    --steering-mount-path=*) STEERING_MOUNT_PATH="${1#*=}"; shift ;;
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
[[ "$REPLICAS" =~ ^[0-9]+$ ]] || die "--replicas must be a non-negative integer"

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

if [[ -n "$STEERING_FILE" ]]; then
  [[ -f "$STEERING_FILE" ]] || die "steering file '$STEERING_FILE' does not exist"
  [[ -n "$STEERING_CONFIGMAP" ]] || die "--steering-configmap cannot be empty"
  [[ -n "$STEERING_KEY" ]] || die "--steering-key cannot be empty"
fi
if [[ -n "$CHAIR_GITHUB_APP_SECRET" ]]; then
  kubectl -n "$KUBE_NAMESPACE" get secret "$CHAIR_GITHUB_APP_SECRET" >/dev/null ||
    die "secret '$CHAIR_GITHUB_APP_SECRET' does not exist in namespace '$KUBE_NAMESPACE'"
fi

kubectl create namespace "$KUBE_NAMESPACE" --dry-run=client -o yaml | kubectl apply -f - >/dev/null

if [[ -n "$STEERING_FILE" ]]; then
  kubectl -n "$KUBE_NAMESPACE" create configmap "$STEERING_CONFIGMAP" \
    "--from-file=$STEERING_KEY=$STEERING_FILE" \
    --dry-run=client -o yaml | kubectl apply -f - >/dev/null
fi

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
  bot_yaml=$(yaml_quote "$bot")
  bot_image_yaml=$(yaml_quote "$bot_image")
  config_url_yaml=$(yaml_quote "$config_url")
  env_entries=""
  env_names=""
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
  append_secret_spec_env_entries
  append_bot_secret_entries "$bot"
  if [[ "$bot" == "$CHAIR_BOT" && -n "$CHAIR_SECRET_NAME" ]]; then
    append_secret_env_entry "$CHAIR_CREDENTIAL_ENV" "$CHAIR_SECRET_NAME" "$CHAIR_SECRET_KEY"
  fi
  env_yaml=""
  if [[ -n "$env_entries" ]]; then
    env_yaml="          env:
$env_entries"
  fi
  volume_mount_entries=""
  volume_entries=""
  build_steering_yaml "$bot_agent"
  build_chair_github_app_yaml "$bot" "$bot_agent"
  volume_mount_yaml=""
  volume_yaml=""
  if [[ -n "$volume_mount_entries" ]]; then
    volume_mount_yaml="          volumeMounts:
$volume_mount_entries"
  fi
  if [[ -n "$volume_entries" ]]; then
    volume_yaml="      volumes:
$volume_entries"
  fi

  echo "applying OpenAB bot deployment: $bot ($bot_agent, $bot_image)"
  kubectl -n "$KUBE_NAMESPACE" apply -f - >/dev/null <<YAML
apiVersion: apps/v1
kind: Deployment
metadata:
  name: $bot_yaml
  labels:
    app: openab-bot
    bot: $bot_yaml
spec:
  replicas: $REPLICAS
  selector:
    matchLabels:
      app: openab-bot
      bot: $bot_yaml
  template:
    metadata:
      labels:
        app: openab-bot
        bot: $bot_yaml
    spec:
      containers:
        - name: openab
          image: $bot_image_yaml
          imagePullPolicy: IfNotPresent
          command:
            - openab
            - run
            - -c
            - $config_url_yaml
$env_yaml
$volume_mount_yaml
$volume_yaml
YAML

  # Local OCP DBs are often recreated, which changes /bot-config tokens without
  # changing the Deployment spec. Restart so pods refetch config on every run.
  if [[ "$REPLICAS" != "0" ]]; then
    kubectl -n "$KUBE_NAMESPACE" rollout restart "deployment/$bot" >/dev/null
  fi

  if [[ "$WAIT_ROLLOUT" == "1" && "$REPLICAS" != "0" ]]; then
    kubectl -n "$KUBE_NAMESPACE" rollout status "deployment/$bot" --timeout=180s
    check_gateway_connected "$bot" || true
  fi
done

kubectl -n "$KUBE_NAMESPACE" get pods -l app=openab-bot \
  -o custom-columns=NAME:.metadata.name,READY:.status.containerStatuses[0].ready,IMAGE:.spec.containers[0].image,STATUS:.status.phase
