#!/usr/bin/env bash
# Wire a GitHub App to a deployed OpenAB Control Plane (webhook + chair identity).
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib/github-app.sh
source "$SCRIPT_DIR/lib/github-app.sh"

APP_ID="${GITHUB_APP_ID:-}"
INSTALLATION_ID="${GITHUB_APP_INSTALLATION_ID:-}"
KEY_PATH="${GITHUB_APP_PRIVATE_KEY_PATH:-}"
PLANE_URL="${PLANE_URL:-}"
WEBHOOK_SECRET="${GITHUB_WEBHOOK_SECRET:-}"
BOT_HANDLE="${OABCP_BOT_HANDLE:-}"
CHAIR_HOME="${CHAIR_HOME:-/home/agent}"
CHAIR_USER="${CHAIR_USER:-}"
DELIVERY="${DELIVERY:-auto}"

CHAIR_SERVICE_ID="${CHAIR_SERVICE_ID:-}"
# Optional space-separated reviewer service IDs to restart so they re-run the
# plane-fetch pre_boot hook too (they gain read-scoped gh auth). Chair is restarted
# regardless via CHAIR_SERVICE_ID.
BOT_SERVICE_IDS="${BOT_SERVICE_IDS:-}"
PLANE_SERVICE_ID="${PLANE_SERVICE_ID:-}"
SERVER_ID="${SERVER_ID:-}"
KUBE_NAMESPACE="${KUBE_NAMESPACE:-oabcp-local}"
KUBE_SECRET_NAME="${KUBE_SECRET_NAME:-github-app-chair}"
SYNC_PLANE_WEBHOOK_SECRET=1

usage() {
  cat <<'USAGE'
Usage:
  scripts/setup-github-app.sh \
    --app-id <id> \
    --installation-id <id> \
    --key-path <app.pem> \
    --plane-url <url> \
    --webhook-secret <secret> \
    [--bot-handle <slug>] \
    [delivery options]

Required:
  --app-id <id>              GitHub App ID
  --installation-id <id>     Installation ID (org or user)
  --key-path <pem>           App private key
  --plane-url <url>          Control plane base URL
  --webhook-secret <secret>  Same secret as GITHUB_WEBHOOK_SECRET on the plane

Optional:
  --bot-handle <slug>        OABCP_BOT_HANDLE (default: App slug from GitHub API)

Plane provisioning (ADR 019 D1 — the App key goes on the PLANE, not a pod):
  --plane-service-id <id>    REQUIRED. Sets GITHUB_APP_ID / _INSTALLATION_ID /
                             _PRIVATE_KEY (PKCS#8, base64) + OABCP_BOT_HANDLE +
                             webhook secret on the control-plane service, restarts it.
  --chair-service-id <id>    Restarted so it re-runs its plane-fetch pre_boot hook.
  BOT_SERVICE_IDS=<ids>      (env) space-separated reviewer service IDs to also restart.

No per-pod key delivery: bot pods fetch a short-lived, role-scoped token from the
plane at boot. The old --delivery/--server-id/--namespace flags are accepted but
ignored for backward compatibility.

Environment fallbacks: GITHUB_APP_ID, GITHUB_APP_INSTALLATION_ID, GITHUB_APP_PRIVATE_KEY_PATH,
GITHUB_WEBHOOK_SECRET, PLANE_URL, OABCP_BOT_HANDLE, CHAIR_SERVICE_ID, PLANE_SERVICE_ID
USAGE
}

die() { echo "error: $*" >&2; exit 1; }

while [[ $# -gt 0 ]]; do
  case "$1" in
    --app-id) APP_ID="${2:?}"; shift 2 ;;
    --installation-id) INSTALLATION_ID="${2:?}"; shift 2 ;;
    --key-path) KEY_PATH="${2:?}"; shift 2 ;;
    --plane-url) PLANE_URL="${2%/}"; shift 2 ;;
    --webhook-secret) WEBHOOK_SECRET="${2:?}"; shift 2 ;;
    --bot-handle) BOT_HANDLE="${2:?}"; shift 2 ;;
    --chair-home) CHAIR_HOME="${2:?}"; shift 2 ;;
    --chair-user) CHAIR_USER="${2:?}"; shift 2 ;;
    --delivery) DELIVERY="${2:?}"; shift 2 ;;
    --chair-service-id) CHAIR_SERVICE_ID="${2:?}"; shift 2 ;;
    --plane-service-id) PLANE_SERVICE_ID="${2:?}"; shift 2 ;;
    --server-id) SERVER_ID="${2:?}"; shift 2 ;;
    --namespace) KUBE_NAMESPACE="${2:?}"; shift 2 ;;
    --secret-name) KUBE_SECRET_NAME="${2:?}"; shift 2 ;;
    --no-sync-plane-secret) SYNC_PLANE_WEBHOOK_SECRET=0; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown arg: $1" ;;
  esac
done

[[ -n "$APP_ID" ]] || die "--app-id required"
[[ -n "$INSTALLATION_ID" ]] || die "--installation-id required"
[[ -r "$KEY_PATH" ]] || die "--key-path must be readable"
[[ -n "$PLANE_URL" ]] || die "--plane-url required"
[[ -n "$WEBHOOK_SECRET" ]] || die "--webhook-secret required (or set GITHUB_WEBHOOK_SECRET)"

gh_app_need gh
gh_app_need node
gh_app_need python3

if [[ -z "$BOT_HANDLE" ]]; then
  jwt=$(gh_app_jwt "$APP_ID" "$KEY_PATH")
  BOT_HANDLE=$(gh api /app -H "Authorization: Bearer $jwt" --jq .slug 2>/dev/null || true)
fi
[[ -n "$BOT_HANDLE" ]] || die "--bot-handle required (could not infer slug)"

if [[ -z "$CHAIR_USER" ]]; then
  CHAIR_USER=$(basename "$CHAIR_HOME")
fi

# The .pem is copied to a temp file solely so the webhook-patch helper can sign the
# App JWT locally; it is never delivered to a pod (D1). The temp dir is wiped on exit.
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT INT TERM
cp "$KEY_PATH" "$TMP/.github-app.pem"
chmod 600 "$TMP/.github-app.pem"

echo "patching GitHub App webhook -> ${PLANE_URL}/api/v1/github_webhooks"
gh_app_patch_webhook "$APP_ID" "$TMP/.github-app.pem" "$WEBHOOK_SECRET" "$PLANE_URL" >/dev/null

sync_plane_vars() {
  [[ -n "$PLANE_SERVICE_ID" ]] || return 0
  command -v npx >/dev/null 2>&1 || {
    echo "warn: npx not found; skip plane variable sync" >&2
    return 0
  }
  # ADR 019 D1: the App private key lives on the PLANE, never on a bot pod. Store
  # the PKCS#8 PEM base64-encoded — a single line the plane's normalize_pem decodes
  # back to a PEM (jsonwebtoken's rust_crypto backend needs valid RSA DER). Convert
  # + validate FIRST, before pushing anything, so an openssl failure can't leave the
  # plane half-provisioned (new handle/secret but a stale or missing key).
  #
  # NOTE (trust boundary): the Zeabur CLI takes values as argv (`-k "K=V"`), so every
  # secret set below — the webhook secret AND this key — is briefly visible in
  # /proc/<pid>/cmdline while the command runs. Run setup on a single-user, trusted
  # machine; the `.pem` is already local to that machine, so this adds no new at-rest
  # exposure, only a short argv window.
  local key_b64
  key_b64=$(openssl pkcs8 -topk8 -nocrypt -in "$KEY_PATH" 2>/dev/null | base64 | tr -d '\n')
  [[ -n "$key_b64" ]] || die "PKCS#8 conversion of $KEY_PATH failed (is openssl installed?)"

  if [[ "$SYNC_PLANE_WEBHOOK_SECRET" == "1" ]]; then
    npx zeabur@latest variable update --id "$PLANE_SERVICE_ID" \
      -k "GITHUB_WEBHOOK_SECRET=$WEBHOOK_SECRET" -y -i=false >/dev/null 2>&1 || \
    npx zeabur@latest variable create --id "$PLANE_SERVICE_ID" \
      -k "GITHUB_WEBHOOK_SECRET=$WEBHOOK_SECRET" -y -i=false >/dev/null
  fi
  npx zeabur@latest variable update --id "$PLANE_SERVICE_ID" \
    -k "OABCP_BOT_HANDLE=$BOT_HANDLE" -y -i=false >/dev/null 2>&1 || \
  npx zeabur@latest variable create --id "$PLANE_SERVICE_ID" \
    -k "OABCP_BOT_HANDLE=$BOT_HANDLE" -y -i=false >/dev/null
  for kv in "GITHUB_APP_ID=$APP_ID" \
            "GITHUB_APP_INSTALLATION_ID=$INSTALLATION_ID" \
            "GITHUB_APP_PRIVATE_KEY=$key_b64"; do
    npx zeabur@latest variable update --id "$PLANE_SERVICE_ID" -k "$kv" -y -i=false >/dev/null 2>&1 || \
    npx zeabur@latest variable create --id "$PLANE_SERVICE_ID" -k "$kv" -y -i=false >/dev/null
  done
  npx zeabur@latest service restart --id "$PLANE_SERVICE_ID" -y -i=false >/dev/null
}

# ADR 019 D1: the App key goes to the PLANE, not a pod. There is no per-pod key
# delivery any more — bot pods fetch a short-lived, role-scoped token from the plane
# (`POST /v1/bots/github-token`) via their pre_boot hook. Restarting the bots makes
# them re-run that hook against the freshly-provisioned plane.
[[ -n "$PLANE_SERVICE_ID" ]] || die "D1 needs --plane-service-id: the App key is provisioned onto the plane, not a pod"
sync_plane_vars
for svc in "$CHAIR_SERVICE_ID" $BOT_SERVICE_IDS; do
  [[ -n "$svc" ]] && npx zeabur@latest service restart --id "$svc" -y -i=false >/dev/null
done

echo "done: GitHub App $APP_ID wired to $PLANE_URL (bot: ${BOT_HANDLE}[bot]); key on plane, not on any pod"