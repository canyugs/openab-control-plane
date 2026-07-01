#!/usr/bin/env bash
# Verify that the local webhook path is reachable and actually opens a session.
set -euo pipefail

KUBE_NAMESPACE="${KUBE_NAMESPACE:-oabcp-local}"
KUBE_SERVICE="${KUBE_SERVICE:-control-plane}"
REMOTE_PORT="${REMOTE_PORT:-8090}"
LOCAL_URL="${LOCAL_URL:-http://localhost:8090}"
WEBHOOK_PATH="${WEBHOOK_PATH:-/api/v1/github_webhooks}"
REPO="${REPO:-canyugs/openab-control-plane}"
PR="${PR:-53}"
ACTION="${ACTION:-synchronize}"
START_PORT_FORWARD=1
PORT_FORWARD_PID=""
PORT_FORWARD_LOG=""
TMP_DIR=""

usage() {
  cat <<'USAGE'
Usage:
  scripts/dev-webhook-ready.sh [--url <base-or-webhook-url>] [--repo owner/repo] [--pr <num>]

Options:
  --url <url>             Base URL or full webhook URL. Default: http://localhost:8090.
  --repo <owner/repo>     Repository in the synthetic payload.
  --pr <number>           PR number in the synthetic payload.
  --action <action>       PR action. Default: synchronize.
  --namespace <name>      Kubernetes namespace. Default: oabcp-local.
  --service <name>        Kubernetes service. Default: control-plane.
  --no-port-forward       Do not auto-start kubectl port-forward.
  --webhook-secret <val>  Webhook secret. Default: env or local deployment env.

Environment:
  LOCAL_URL, WEBHOOK_PATH, REPO, PR, ACTION, KUBE_NAMESPACE, GITHUB_WEBHOOK_SECRET.

The script signs a synthetic GitHub pull_request webhook and expects OCP to return
triggered:true. Run this before pushing when testing GitHub App webhooks locally.
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
    --url) LOCAL_URL="${2:?--url needs a value}"; shift 2 ;;
    --url=*) LOCAL_URL="${1#*=}"; shift ;;
    --repo) REPO="${2:?--repo needs a value}"; shift 2 ;;
    --repo=*) REPO="${1#*=}"; shift ;;
    --pr) PR="${2:?--pr needs a value}"; shift 2 ;;
    --pr=*) PR="${1#*=}"; shift ;;
    --action) ACTION="${2:?--action needs a value}"; shift 2 ;;
    --action=*) ACTION="${1#*=}"; shift ;;
    --namespace) KUBE_NAMESPACE="${2:?--namespace needs a value}"; shift 2 ;;
    --namespace=*) KUBE_NAMESPACE="${1#*=}"; shift ;;
    --service) KUBE_SERVICE="${2:?--service needs a value}"; shift 2 ;;
    --service=*) KUBE_SERVICE="${1#*=}"; shift ;;
    --no-port-forward) START_PORT_FORWARD=0; shift ;;
    --webhook-secret) WEBHOOK_SECRET="${2:?--webhook-secret needs a value}"; shift 2 ;;
    --webhook-secret=*) WEBHOOK_SECRET="${1#*=}"; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
done

need curl
need node

target_url() {
  URL_IN="$LOCAL_URL" WEBHOOK_PATH="$WEBHOOK_PATH" node - <<'NODE'
const input = process.env.URL_IN;
const webhookPath = process.env.WEBHOOK_PATH;
const u = new URL(input);
const normalizedPath = webhookPath.replace(/\/+$/, "");
if (u.pathname.replace(/\/+$/, "") !== normalizedPath) {
  u.pathname = normalizedPath;
  u.search = "";
  u.hash = "";
}
process.stdout.write(u.toString().replace(/\/$/, ""));
NODE
}

local_port() {
  URL_IN="$LOCAL_URL" node - <<'NODE'
const u = new URL(process.env.URL_IN);
process.stdout.write(u.port || (u.protocol === "https:" ? "443" : "80"));
NODE
}

is_local_url() {
  URL_IN="$LOCAL_URL" node - <<'NODE'
const host = new URL(process.env.URL_IN).hostname;
process.exit(["localhost", "127.0.0.1", "::1"].includes(host) ? 0 : 1);
NODE
}

ensure_tmp_dir() {
  if [[ -z "$TMP_DIR" ]]; then
    TMP_DIR=$(mktemp -d "${TMPDIR:-/tmp}/oabcp-webhook-ready.XXXXXX")
  fi
}

cleanup() {
  local status=$?
  trap - EXIT INT TERM
  if [[ -n "${PORT_FORWARD_PID:-}" ]]; then
    kill "$PORT_FORWARD_PID" >/dev/null 2>&1 || true
    wait "$PORT_FORWARD_PID" >/dev/null 2>&1 || true
  fi
  if [[ -n "${TMP_DIR:-}" ]]; then
    rm -rf "$TMP_DIR"
  fi
  exit "$status"
}
trap cleanup EXIT INT TERM

TARGET_URL=$(target_url)

reachable() {
  local code
  code=$(curl -sS -o /dev/null -w '%{http_code}' --max-time 3 "$TARGET_URL" || true)
  [[ "$code" != "000" ]]
}

if ! reachable && [[ "$START_PORT_FORWARD" == "1" ]]; then
  if is_local_url; then
    need kubectl
    ensure_tmp_dir
    PORT_FORWARD_LOG="$TMP_DIR/port-forward.log"
    port=$(local_port)
    echo "starting port-forward: $KUBE_NAMESPACE/service/$KUBE_SERVICE $port:$REMOTE_PORT"
    kubectl -n "$KUBE_NAMESPACE" port-forward "service/$KUBE_SERVICE" \
      "$port:$REMOTE_PORT" >"$PORT_FORWARD_LOG" 2>&1 &
    PORT_FORWARD_PID=$!
    for _ in $(seq 1 30); do
      reachable && break
      if ! kill -0 "$PORT_FORWARD_PID" >/dev/null 2>&1; then
        cat "$PORT_FORWARD_LOG" >&2 || true
        die "port-forward exited before webhook became reachable"
      fi
      sleep 1
    done
  fi
fi

reachable || die "webhook URL is not reachable: $TARGET_URL"

if [[ -z "$WEBHOOK_SECRET" ]]; then
  need kubectl
  WEBHOOK_SECRET=$(kubectl -n "$KUBE_NAMESPACE" get deployment control-plane \
    -o jsonpath='{range .spec.template.spec.containers[0].env[?(@.name=="GITHUB_WEBHOOK_SECRET")]}{.value}{end}' \
    2>/dev/null || true)
fi
[[ -n "$WEBHOOK_SECRET" ]] || die "GITHUB_WEBHOOK_SECRET not found; pass --webhook-secret"

BODY=$(REPO="$REPO" PR="$PR" ACTION="$ACTION" node - <<'NODE'
const repo = process.env.REPO;
const pr = Number(process.env.PR);
const action = process.env.ACTION;
process.stdout.write(JSON.stringify({
  action,
  installation: { id: 999999 },
  repository: { full_name: repo },
  pull_request: {
    number: pr,
    url: `https://api.github.com/repos/${repo}/pulls/${pr}`,
    labels: [],
  },
}));
NODE
)

SIG=$(BODY="$BODY" WEBHOOK_SECRET="$WEBHOOK_SECRET" node - <<'NODE'
const crypto = require("crypto");
process.stdout.write(
  "sha256=" +
    crypto
      .createHmac("sha256", process.env.WEBHOOK_SECRET)
      .update(process.env.BODY)
      .digest("hex")
);
NODE
)

response=$(curl -sS -X POST "$TARGET_URL" \
  -H "content-type: application/json" \
  -H "x-github-event: pull_request" \
  -H "x-hub-signature-256: $SIG" \
  --data "$BODY")

RESPONSE="$response" node - <<'NODE'
const response = JSON.parse(process.env.RESPONSE);
if (!response.ok || !response.triggered) {
  console.error(`webhook probe did not trigger a session: ${JSON.stringify(response)}`);
  process.exit(1);
}
const suffix = response.deduped ? " (deduped)" : "";
console.log(`webhook ready: session ${response.session_id}${suffix}`);
NODE
