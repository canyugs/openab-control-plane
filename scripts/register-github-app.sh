#!/usr/bin/env bash
# Register a GitHub App for OpenAB Control Plane (manifest or manual checklist).
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib/github-app.sh
source "$SCRIPT_DIR/lib/github-app.sh"

MODE=""
PLANE_URL=""
APP_NAME="OpenAB Council"
GITHUB_ORG=""
REDIRECT_BASE=""
PORT="${PORT:-8795}"
CODE_FILE="${CODE_FILE:-/tmp/github-app-manifest-code.txt}"
OUTPUT_DIR="${OUTPUT_DIR:-.}"
START_TUNNEL=1
WAIT_FOR_CODE=1
APP_SLUG=""

usage() {
  cat <<'USAGE'
Usage:
  scripts/register-github-app.sh manifest --plane-url <url> [options]
  scripts/register-github-app.sh manual --plane-url <url> [options]
  scripts/register-github-app.sh exchange --code-file <path> [options]
  scripts/register-github-app.sh install-url --slug <slug> [--org <login>]

Subcommands:
  manifest   Serve a GitHub App manifest page and exchange the callback code.
  manual     Print a UI checklist (no local server).
  exchange   Exchange an existing manifest code (skip registration page).
  install-url
             Print the GitHub App installation URL.

Common options:
  --plane-url <url>       Control plane base URL (required for manifest/manual)
  --app-name <name>       App display name (default: OpenAB Council)
  --org <login>           Register under an organization (e.g. zeabur)
  --output-dir <dir>      Where to write PEM + summary JSON (default: .)

Manifest-only options:
  --redirect-base <url>   Public callback base URL. If omitted, starts cloudflared.
  --no-tunnel             Fail instead of auto-starting cloudflared when redirect-base unset.
  --port <n>              Local manifest server port (default: 8795)
  --code-file <path>      Where the callback code is written
  --no-wait               Start server/tunnel and exit (do not block for code)

Requires for manifest/exchange: gh, python3.
Cloudflared is optional — only used when --redirect-base is not provided.
USAGE
}

die() { echo "error: $*" >&2; exit 1; }

cleanup() {
  [[ -n "${MANIFEST_PID:-}" ]] && kill "$MANIFEST_PID" 2>/dev/null || true
  [[ -n "${TUNNEL_PID:-}" ]] && kill "$TUNNEL_PID" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

wait_for_cloudflared_url() {
  local log_file="$1" timeout="${2:-30}" i=0 url=""
  while (( i < timeout )); do
    url=$(grep -oE 'https://[a-z0-9-]+\.trycloudflare\.com' "$log_file" | tail -1 || true)
    [[ -n "$url" ]] && { printf '%s' "$url"; return 0; }
    sleep 1
    ((i++)) || true
  done
  return 1
}

run_manifest() {
  [[ -n "$PLANE_URL" ]] || die "--plane-url required for manifest"
  gh_app_need gh
  gh_app_need python3

  local tunnel_log="${TMPDIR:-/tmp}/github-app-cloudflared.log"
  if [[ -z "$REDIRECT_BASE" ]]; then
    [[ "$START_TUNNEL" == "1" ]] || die "--redirect-base required (or allow auto tunnel)"
    command -v cloudflared >/dev/null 2>&1 || die "cloudflared not found; pass --redirect-base or install cloudflared"
    : >"$tunnel_log"
    cloudflared tunnel --url "http://127.0.0.1:${PORT}" >"$tunnel_log" 2>&1 &
    TUNNEL_PID=$!
    REDIRECT_BASE=$(wait_for_cloudflared_url "$tunnel_log" 45) ||
      die "timed out waiting for cloudflared URL (see $tunnel_log)"
    echo "cloudflared: $REDIRECT_BASE"
  fi

  rm -f "$CODE_FILE"
  PLANE_URL="$PLANE_URL" APP_NAME="$APP_NAME" GITHUB_ORG="$GITHUB_ORG" \
    REDIRECT_BASE="$REDIRECT_BASE" PORT="$PORT" CODE_FILE="$CODE_FILE" \
    python3 "$SCRIPT_DIR/github-app-manifest-server.py" &
  MANIFEST_PID=$!
  sleep 1

  local open_url="${REDIRECT_BASE%/}/"
  echo ""
  echo "Open this URL and click Create ${APP_NAME} GitHub App:"
  echo "  $open_url"
  echo ""
  echo "Webhook target: ${PLANE_URL%/}/api/v1/github_webhooks"
  if [[ -n "$GITHUB_ORG" ]]; then
    echo "Owner: organization $GITHUB_ORG (requires org owner or Manage GitHub Apps permission)"
  fi
  echo ""

  if [[ "$WAIT_FOR_CODE" != "1" ]]; then
    echo "Manifest server running (pid $MANIFEST_PID). Callback code -> $CODE_FILE"
    trap - EXIT INT TERM
    exit 0
  fi

  echo "Waiting for GitHub redirect (up to 60 minutes)..."
  local deadline=$((SECONDS + 3600))
  while (( SECONDS < deadline )); do
    if [[ -s "$CODE_FILE" ]]; then
      break
    fi
    if ! kill -0 "$MANIFEST_PID" 2>/dev/null; then
      die "manifest server exited before callback"
    fi
    sleep 2
  done
  [[ -s "$CODE_FILE" ]] || die "timed out waiting for manifest code in $CODE_FILE"

  kill "$MANIFEST_PID" 2>/dev/null || true
  MANIFEST_PID=""
  if [[ -n "${TUNNEL_PID:-}" ]]; then
    kill "$TUNNEL_PID" 2>/dev/null || true
    TUNNEL_PID=""
  fi

  "$SCRIPT_DIR/exchange-github-app-manifest.sh" \
    --code-file "$CODE_FILE" \
    --output-dir "$OUTPUT_DIR"
}

run_manual() {
  [[ -n "$PLANE_URL" ]] || die "--plane-url required for manual"
  local webhook="${PLANE_URL%/}/api/v1/github_webhooks"
  local owner
  if [[ -n "$GITHUB_ORG" ]]; then
    owner="https://github.com/organizations/${GITHUB_ORG}/settings/apps/new"
  else
    owner="https://github.com/settings/apps/new"
  fi
  cat <<EOF
GitHub App manual setup checklist
=================================

1. Open: $owner
2. Create app: $APP_NAME
3. Homepage URL: ${PLANE_URL%/}
4. Webhook URL: $webhook
5. Webhook secret: same value as control-plane GITHUB_WEBHOOK_SECRET
   (generate with: openssl rand -hex 32)
6. Permissions:
   - Pull requests: Read and write
   - Contents: Read-only
   - Commit statuses: Read and write
   - Issues: Read and write
7. Events:
   - Pull requests
   - Issue comments
8. Generate a private key (.pem) and download it.
9. Install the app on your org/repos. Install URL after creation:
   scripts/register-github-app.sh install-url --slug <slug> --org <org>
10. Wire the deployment:
   scripts/setup-github-app.sh \\
     --app-id <APP_ID> \\
     --installation-id <INSTALLATION_ID> \\
     --key-path path/to/app.pem \\
     --plane-url ${PLANE_URL%/} \\
     --webhook-secret <same secret as step 5> \\
     --bot-handle <slug>

List installations after install:
  scripts/github-app-list-installations.sh --app-id <APP_ID> --key-path path/to/app.pem
EOF
}

run_exchange() {
  [[ -r "$CODE_FILE" ]] || die "--code-file must be readable for exchange"
  gh_app_need gh
  "$SCRIPT_DIR/exchange-github-app-manifest.sh" \
    --code-file "$CODE_FILE" \
    --output-dir "$OUTPUT_DIR"
}

run_install_url() {
  [[ -n "$APP_SLUG" ]] || die "--slug required for install-url"
  gh_app_need gh
  gh_app_install_url "$APP_SLUG" "$GITHUB_ORG"
  echo
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    manifest|manual|exchange|install-url)
      MODE="$1"; shift ;;
    --plane-url) PLANE_URL="${2%/}"; shift 2 ;;
    --app-name) APP_NAME="${2:?}"; shift 2 ;;
    --org) GITHUB_ORG="${2:?}"; shift 2 ;;
    --redirect-base) REDIRECT_BASE="${2%/}"; shift 2 ;;
    --port) PORT="${2:?}"; shift 2 ;;
    --code-file) CODE_FILE="${2:?}"; shift 2 ;;
    --output-dir) OUTPUT_DIR="${2:?}"; shift 2 ;;
    --slug) APP_SLUG="${2:?}"; shift 2 ;;
    --no-tunnel) START_TUNNEL=0; shift ;;
    --no-wait) WAIT_FOR_CODE=0; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown arg: $1" ;;
  esac
done

[[ -n "$MODE" ]] || { usage; exit 1; }

case "$MODE" in
  manifest) run_manifest ;;
  manual) run_manual ;;
  exchange) run_exchange ;;
  install-url) run_install_url ;;
esac