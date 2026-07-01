#!/usr/bin/env bash
# Run OCP on the host while local Kubernetes runs OpenAB bot pods.
set -euo pipefail

HOST="${HOST:-127.0.0.1}"
PORT="${PORT:-18090}"
OABCP_ADDR="${OABCP_ADDR:-$HOST:$PORT}"
OABCP_DB="${OABCP_DB:-/tmp/oabcp-local.db}"
OABCP_BOTS="${OABCP_BOTS:-chair:chair,rev1:reviewer,rev2:reviewer}"
OABCP_COUNCIL_ROSTER="${OABCP_COUNCIL_ROSTER:-chair,rev1,rev2}"
OABCP_WS_URL="${OABCP_WS_URL:-ws://host.docker.internal:$PORT/ws}"
OABCP_API_KEY="${OABCP_API_KEY:-local-test-key}"
OABCP_AGENT_COMMAND="${OABCP_AGENT_COMMAND:-}"
OABCP_AGENT_PROFILES="${OABCP_AGENT_PROFILES:-}"
OABCP_AGENT_WORKING_DIR="${OABCP_AGENT_WORKING_DIR:-}"
OABCP_AGENT_INHERIT_ENV="${OABCP_AGENT_INHERIT_ENV:-}"
KUBE_NAMESPACE="${KUBE_NAMESPACE:-oabcp-local}"
KUBE_CONTEXT="${KUBE_CONTEXT:-docker-desktop}"
FETCH_WEBHOOK_SECRET=1
CHECK_CONTEXT=1
RELEASE=0

usage() {
  cat <<'USAGE'
Usage:
  scripts/dev-run-host-ocp.sh

Options:
  --host <host>              Bind host. Default: 127.0.0.1.
  --port <port>              Bind port. Default: 18090.
  --addr <host:port>         Full OABCP_ADDR override.
  --db <path>                SQLite DB path. Default: /tmp/oabcp-local.db.
  --bots <spec>              OABCP_BOTS. Default: chair:chair,rev1:reviewer,rev2:reviewer.
  --roster <names>           OABCP_COUNCIL_ROSTER. Default: chair,rev1,rev2.
  --ws-url <url>             Bot WebSocket URL. Default: ws://host.docker.internal:<port>/ws.
  --api-key <key>            OABCP_API_KEY. Default: local-test-key.
  --agent-command <profile>  Default /bot-config agent profile when ?agent= is absent.
  --agent-profiles-json <j>  JSON object overriding or adding OAB agent profiles.
  --agent-profiles-file <p>  Read OABCP_AGENT_PROFILES JSON from a file.
  --agent-working-dir <dir>  Force [agent].working_dir for served bot configs.
  --agent-inherit-env <csv>  Extra env names appended to [agent].inherit_env.
  --no-k8s-webhook-secret    Do not read GITHUB_WEBHOOK_SECRET from local K8s OCP.
  --namespace <name>         Kubernetes namespace. Default: oabcp-local.
  --context <name>           Expected kubectl context. Default: docker-desktop.
  --any-context              Do not enforce the kubectl context.
  --release                  Run cargo in release mode.

Environment:
  OABCP_ADDR, OABCP_DB, OABCP_BOTS, OABCP_COUNCIL_ROSTER, OABCP_WS_URL,
  OABCP_API_KEY, OABCP_AGENT_COMMAND, OABCP_AGENT_PROFILES,
  OABCP_AGENT_WORKING_DIR, OABCP_AGENT_INHERIT_ENV, GITHUB_WEBHOOK_SECRET.

When GITHUB_WEBHOOK_SECRET is unset, this script copies the secret value from the
local Kubernetes control-plane deployment. It never prints the secret.
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
    --host) HOST="${2:?--host needs a value}"; shift 2 ;;
    --host=*) HOST="${1#*=}"; shift ;;
    --port) PORT="${2:?--port needs a value}"; OABCP_ADDR="${HOST}:${PORT}"; OABCP_WS_URL="ws://host.docker.internal:${PORT}/ws"; shift 2 ;;
    --port=*) PORT="${1#*=}"; OABCP_ADDR="${HOST}:${PORT}"; OABCP_WS_URL="ws://host.docker.internal:${PORT}/ws"; shift ;;
    --addr) OABCP_ADDR="${2:?--addr needs a value}"; shift 2 ;;
    --addr=*) OABCP_ADDR="${1#*=}"; shift ;;
    --db) OABCP_DB="${2:?--db needs a path}"; shift 2 ;;
    --db=*) OABCP_DB="${1#*=}"; shift ;;
    --bots) OABCP_BOTS="${2:?--bots needs a value}"; shift 2 ;;
    --bots=*) OABCP_BOTS="${1#*=}"; shift ;;
    --roster) OABCP_COUNCIL_ROSTER="${2:?--roster needs a value}"; shift 2 ;;
    --roster=*) OABCP_COUNCIL_ROSTER="${1#*=}"; shift ;;
    --ws-url) OABCP_WS_URL="${2:?--ws-url needs a value}"; shift 2 ;;
    --ws-url=*) OABCP_WS_URL="${1#*=}"; shift ;;
    --api-key) OABCP_API_KEY="${2:?--api-key needs a value}"; shift 2 ;;
    --api-key=*) OABCP_API_KEY="${1#*=}"; shift ;;
    --agent-command) OABCP_AGENT_COMMAND="${2:?--agent-command needs a value}"; shift 2 ;;
    --agent-command=*) OABCP_AGENT_COMMAND="${1#*=}"; shift ;;
    --agent-profiles-json) OABCP_AGENT_PROFILES="${2:?--agent-profiles-json needs a value}"; shift 2 ;;
    --agent-profiles-json=*) OABCP_AGENT_PROFILES="${1#*=}"; shift ;;
    --agent-profiles-file)
      profile_file="${2:?--agent-profiles-file needs a path}"
      [[ -r "$profile_file" ]] || die "agent profiles file is not readable: $profile_file"
      OABCP_AGENT_PROFILES="$(<"$profile_file")"
      shift 2
      ;;
    --agent-profiles-file=*)
      profile_file="${1#*=}"
      [[ -r "$profile_file" ]] || die "agent profiles file is not readable: $profile_file"
      OABCP_AGENT_PROFILES="$(<"$profile_file")"
      shift
      ;;
    --agent-working-dir) OABCP_AGENT_WORKING_DIR="${2:?--agent-working-dir needs a value}"; shift 2 ;;
    --agent-working-dir=*) OABCP_AGENT_WORKING_DIR="${1#*=}"; shift ;;
    --agent-inherit-env) OABCP_AGENT_INHERIT_ENV="${2:?--agent-inherit-env needs a value}"; shift 2 ;;
    --agent-inherit-env=*) OABCP_AGENT_INHERIT_ENV="${1#*=}"; shift ;;
    --no-k8s-webhook-secret) FETCH_WEBHOOK_SECRET=0; shift ;;
    --namespace) KUBE_NAMESPACE="${2:?--namespace needs a value}"; shift 2 ;;
    --namespace=*) KUBE_NAMESPACE="${1#*=}"; shift ;;
    --context) KUBE_CONTEXT="${2:?--context needs a value}"; shift 2 ;;
    --context=*) KUBE_CONTEXT="${1#*=}"; shift ;;
    --any-context) CHECK_CONTEXT=0; shift ;;
    --release) RELEASE=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
done

need cargo

if [[ -z "${GITHUB_WEBHOOK_SECRET:-}" && "$FETCH_WEBHOOK_SECRET" == "1" ]]; then
  need kubectl
  if [[ "$CHECK_CONTEXT" == "1" ]]; then
    current_context=$(kubectl config current-context)
    [[ "$current_context" == "$KUBE_CONTEXT" ]] ||
      die "kubectl context is '$current_context', expected '$KUBE_CONTEXT' (or pass --any-context)"
  fi
  GITHUB_WEBHOOK_SECRET=$(kubectl -n "$KUBE_NAMESPACE" get deployment control-plane \
    -o jsonpath='{range .spec.template.spec.containers[0].env[?(@.name=="GITHUB_WEBHOOK_SECRET")]}{.value}{end}' \
    2>/dev/null || true)
fi

export OABCP_ADDR
export OABCP_DB
export OABCP_BOTS
export OABCP_COUNCIL_ROSTER
export OABCP_WS_URL
export OABCP_API_KEY
export OABCP_AGENT_COMMAND
export OABCP_AGENT_PROFILES
export OABCP_AGENT_WORKING_DIR
export OABCP_AGENT_INHERIT_ENV
export GITHUB_WEBHOOK_SECRET

echo "starting host OCP at $OABCP_ADDR (db=$OABCP_DB, ws=$OABCP_WS_URL)"
if [[ -z "${GITHUB_WEBHOOK_SECRET:-}" ]]; then
  echo "warning: GITHUB_WEBHOOK_SECRET is unset; GitHub webhook requests will be rejected" >&2
fi

if [[ "$RELEASE" == "1" ]]; then
  exec cargo run --release
else
  exec cargo run
fi
