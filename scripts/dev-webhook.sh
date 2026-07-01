#!/usr/bin/env bash
# Expose a local OCP through cloudflared and point the zeabur-council GitHub App
# webhook at it for development.
#
# Safe defaults:
# - does not print the App private key or JWT;
# - quick tunnels restore the original App webhook URL on Ctrl-C;
# - fixed URLs patch once and exit unless --wait is passed.
set -euo pipefail

APP_ID="${GITHUB_APP_ID:-4146119}"
KEY_PATH="${GITHUB_APP_PRIVATE_KEY_PATH:-}"
LOCAL_URL="${LOCAL_URL:-http://localhost:8090}"
WEBHOOK_PATH="${WEBHOOK_PATH:-/api/v1/github_webhooks}"
KUBE_NAMESPACE="${KUBE_NAMESPACE:-oabcp-local}"
KUBE_SERVICE="${KUBE_SERVICE:-control-plane}"
REMOTE_PORT="${REMOTE_PORT:-8090}"
MODE=""
TARGET_URL=""
CHECK_ONLY=0
WAIT_AFTER_PATCH=0
RESTORE_ON_EXIT=""
START_PORT_FORWARD=1
PORT_FORWARD_PID=""
CLOUDFLARED_PID=""
CLOUDFLARED_LOG=""
PORT_FORWARD_LOG=""
TMP_DIR=""
ORIGINAL_URL=""
PATCHED=0

usage() {
  cat <<'USAGE'
Usage:
  scripts/dev-webhook.sh --check --key-path <app-private-key.pem>
  scripts/dev-webhook.sh --quick --key-path <app-private-key.pem>
  scripts/dev-webhook.sh --url https://<host> --key-path <app-private-key.pem>

Options:
  --quick                 Start a Cloudflare quick tunnel to LOCAL_URL.
  --url <url>             Patch the App webhook to this base URL or full webhook URL.
  --check                 Print current GitHub App webhook config.
  --wait                  Keep running after --url patch; restores on exit unless --no-restore.
  --no-restore            Do not restore the previous App webhook URL on exit.
  --no-port-forward       Do not auto-start kubectl port-forward when LOCAL_URL is down.
  --app-id <id>           GitHub App ID. Default: GITHUB_APP_ID or 4146119.
  --key-path <path>       GitHub App private key PEM. Or set GITHUB_APP_PRIVATE_KEY_PATH.
  --local-url <url>       Local OCP URL. Default: http://localhost:8090.
  --webhook-path <path>   Webhook path. Default: /api/v1/github_webhooks.
  --namespace <name>      Kubernetes namespace for port-forward. Default: oabcp-local.
  --service <name>        Kubernetes service for port-forward. Default: control-plane.

Examples:
  scripts/dev-webhook.sh --quick \
    --key-path /Users/can/Downloads/zeabur-council.2026-06-27.private-key.pem

  scripts/dev-webhook.sh --url https://ocp-dev.example.com \
    --key-path /Users/can/Downloads/zeabur-council.2026-06-27.private-key.pem
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
    --quick) MODE="quick"; shift ;;
    --url) MODE="url"; TARGET_URL="${2:?--url needs a URL}"; shift 2 ;;
    --url=*) MODE="url"; TARGET_URL="${1#*=}"; shift ;;
    --check) CHECK_ONLY=1; shift ;;
    --wait) WAIT_AFTER_PATCH=1; shift ;;
    --no-restore) RESTORE_ON_EXIT=0; shift ;;
    --no-port-forward) START_PORT_FORWARD=0; shift ;;
    --app-id) APP_ID="${2:?--app-id needs a value}"; shift 2 ;;
    --app-id=*) APP_ID="${1#*=}"; shift ;;
    --key-path) KEY_PATH="${2:?--key-path needs a path}"; shift 2 ;;
    --key-path=*) KEY_PATH="${1#*=}"; shift ;;
    --local-url) LOCAL_URL="${2:?--local-url needs a URL}"; shift 2 ;;
    --local-url=*) LOCAL_URL="${1#*=}"; shift ;;
    --webhook-path) WEBHOOK_PATH="${2:?--webhook-path needs a path}"; shift 2 ;;
    --webhook-path=*) WEBHOOK_PATH="${1#*=}"; shift ;;
    --namespace) KUBE_NAMESPACE="${2:?--namespace needs a value}"; shift 2 ;;
    --namespace=*) KUBE_NAMESPACE="${1#*=}"; shift ;;
    --service) KUBE_SERVICE="${2:?--service needs a value}"; shift 2 ;;
    --service=*) KUBE_SERVICE="${1#*=}"; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
done

need gh
need node
need curl
[[ -n "$KEY_PATH" ]] || die "set --key-path or GITHUB_APP_PRIVATE_KEY_PATH"
[[ -r "$KEY_PATH" ]] || die "private key file is not readable: $KEY_PATH"
[[ "$WEBHOOK_PATH" == /* ]] || die "--webhook-path must start with /"

if [[ "$CHECK_ONLY" == "0" && -z "$MODE" ]]; then
  die "choose --quick, --url, or --check"
fi
if [[ "$CHECK_ONLY" == "1" && -n "$MODE" ]]; then
  die "--check cannot be combined with --quick or --url"
fi

generate_jwt() {
  APP_ID="$APP_ID" KEY_PATH="$KEY_PATH" node - <<'NODE'
const fs = require("fs");
const crypto = require("crypto");

const appId = process.env.APP_ID;
const key = fs.readFileSync(process.env.KEY_PATH, "utf8");
const b64 = (obj) => Buffer.from(JSON.stringify(obj)).toString("base64url");
const now = Math.floor(Date.now() / 1000);
const data = [
  b64({ alg: "RS256", typ: "JWT" }),
  b64({ iat: now - 60, exp: now + 540, iss: appId }),
].join(".");
const sig = crypto.sign("RSA-SHA256", Buffer.from(data), key).toString("base64url");
process.stdout.write(`${data}.${sig}`);
NODE
}

github_app_api() {
  local method="$1"
  local path="$2"
  shift 2
  local jwt
  jwt=$(generate_jwt)
  gh api "$path" \
    -X "$method" \
    -H "Authorization: Bearer $jwt" \
    -H "Accept: application/vnd.github+json" \
    "$@"
}

normalize_webhook_url() {
  URL_IN="$1" WEBHOOK_PATH="$WEBHOOK_PATH" node - <<'NODE'
const urlIn = process.env.URL_IN;
const webhookPath = process.env.WEBHOOK_PATH;
const u = new URL(urlIn);
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
  LOCAL_URL="$LOCAL_URL" node - <<'NODE'
const u = new URL(process.env.LOCAL_URL);
process.stdout.write(u.port || (u.protocol === "https:" ? "443" : "80"));
NODE
}

local_reachable() {
  local code
  code=$(curl -sS -o /dev/null -w '%{http_code}' --max-time 2 \
    "$LOCAL_URL$WEBHOOK_PATH" || true)
  [[ "$code" != "000" ]]
}

ensure_tmp_dir() {
  if [[ -z "$TMP_DIR" ]]; then
    TMP_DIR=$(mktemp -d "${TMPDIR:-/tmp}/oabcp-dev-webhook.XXXXXX")
  fi
}

start_port_forward_if_needed() {
  [[ "$START_PORT_FORWARD" == "1" ]] || return 0
  if local_reachable; then
    return 0
  fi
  need kubectl
  ensure_tmp_dir
  PORT_FORWARD_LOG="$TMP_DIR/port-forward.log"
  local port
  port=$(local_port)
  echo "starting port-forward: $KUBE_NAMESPACE/service/$KUBE_SERVICE $port:$REMOTE_PORT"
  kubectl -n "$KUBE_NAMESPACE" port-forward "service/$KUBE_SERVICE" \
    "$port:$REMOTE_PORT" >"$PORT_FORWARD_LOG" 2>&1 &
  PORT_FORWARD_PID=$!
  for _ in $(seq 1 30); do
    if local_reachable; then
      return 0
    fi
    if ! kill -0 "$PORT_FORWARD_PID" >/dev/null 2>&1; then
      cat "$PORT_FORWARD_LOG" >&2 || true
      die "port-forward exited before local OCP became reachable"
    fi
    sleep 1
  done
  cat "$PORT_FORWARD_LOG" >&2 || true
  die "local OCP did not become reachable at $LOCAL_URL"
}

wait_for_quick_tunnel_url() {
  local log="$1"
  for _ in $(seq 1 45); do
    local url
    url=$(sed -nE 's/.*(https:\/\/[A-Za-z0-9.-]+\.trycloudflare\.com).*/\1/p' \
      "$log" | tail -n 1)
    if [[ -n "$url" ]]; then
      printf '%s' "$url"
      return 0
    fi
    if ! kill -0 "$CLOUDFLARED_PID" >/dev/null 2>&1; then
      cat "$log" >&2 || true
      die "cloudflared exited before exposing a URL"
    fi
    sleep 1
  done
  cat "$log" >&2 || true
  die "timed out waiting for cloudflared quick tunnel URL"
}

patch_webhook() {
  local target="$1"
  github_app_api PATCH /app/hook/config \
    -f "url=$target" \
    -f content_type=json \
    -f insecure_ssl=0 >/dev/null
  PATCHED=1
}

cleanup() {
  local status=$?
  trap - EXIT INT TERM
  if [[ "${RESTORE_ON_EXIT:-0}" == "1" && "$PATCHED" == "1" && -n "$ORIGINAL_URL" ]]; then
    echo "restoring GitHub App webhook URL: $ORIGINAL_URL"
    patch_webhook "$ORIGINAL_URL" || true
  fi
  if [[ -n "${CLOUDFLARED_PID:-}" ]]; then
    kill "$CLOUDFLARED_PID" >/dev/null 2>&1 || true
    wait "$CLOUDFLARED_PID" >/dev/null 2>&1 || true
  fi
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

if [[ "$CHECK_ONLY" == "1" ]]; then
  github_app_api GET /app --jq '{id,slug,name,html_url}'
  github_app_api GET /app/hook/config --jq '{url,content_type,insecure_ssl}'
  exit 0
fi

ORIGINAL_URL=$(github_app_api GET /app/hook/config --jq .url)
echo "current GitHub App webhook URL: $ORIGINAL_URL"

case "$MODE" in
  quick)
    need cloudflared
    [[ "$RESTORE_ON_EXIT" == "" ]] && RESTORE_ON_EXIT=1
    start_port_forward_if_needed
    ensure_tmp_dir
    CLOUDFLARED_LOG="$TMP_DIR/cloudflared.log"
    echo "starting cloudflared quick tunnel: $LOCAL_URL"
    cloudflared tunnel --url "$LOCAL_URL" >"$CLOUDFLARED_LOG" 2>&1 &
    CLOUDFLARED_PID=$!
    TARGET_URL=$(wait_for_quick_tunnel_url "$CLOUDFLARED_LOG")
    ;;
  url)
    if [[ "$RESTORE_ON_EXIT" == "" ]]; then
      if [[ "$WAIT_AFTER_PATCH" == "1" ]]; then
        RESTORE_ON_EXIT=1
      else
        RESTORE_ON_EXIT=0
      fi
    fi
    ;;
esac

TARGET_URL=$(normalize_webhook_url "$TARGET_URL")
echo "patching GitHub App webhook URL: $TARGET_URL"
patch_webhook "$TARGET_URL"
github_app_api GET /app/hook/config --jq '{url,content_type,insecure_ssl}'

if [[ "$MODE" == "quick" || "$WAIT_AFTER_PATCH" == "1" ]]; then
  echo
  echo "GitHub App webhook is pointed at local OCP."
  echo "Press Ctrl-C to stop."
  if [[ "$RESTORE_ON_EXIT" == "1" ]]; then
    echo "The original webhook URL will be restored on exit."
  else
    echo "Webhook URL will be left as-is on exit."
  fi
  while true; do
    sleep 3600
  done
fi
