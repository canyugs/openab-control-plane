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
  --chair-home <path>        Chair pod home (default: /home/agent for Kiro, /home/node for Claude)
  --chair-user <user>        Chair pod user (default: basename of chair-home)
  --delivery <mode>          auto | zeabur-ssh | zeabur-exec | k8s | files-only

Zeabur delivery (zeabur-ssh or zeabur-exec):
  --chair-service-id <id>
  --plane-service-id <id>    Needed to set OABCP_BOT_HANDLE and webhook secret
  --server-id <id>           Dedicated server ID for zeabur-ssh (recommended; avoids exec 524)

Kubernetes delivery (k8s):
  --namespace <name>         Default: oabcp-local
  --secret-name <name>       Default: github-app-chair
  Then redeploy chair with: scripts/dev-deploy-bots.sh --chair-github-app-secret <name>

files-only:
  Patches the App webhook and writes a local chair bundle under ./chair-github-app-bundle/

Environment fallbacks: GITHUB_APP_ID, GITHUB_APP_INSTALLATION_ID, GITHUB_APP_PRIVATE_KEY_PATH,
GITHUB_WEBHOOK_SECRET, PLANE_URL, OABCP_BOT_HANDLE, CHAIR_SERVICE_ID, PLANE_SERVICE_ID, SERVER_ID
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

if [[ "$DELIVERY" == "auto" ]]; then
  if [[ -n "$SERVER_ID" && -n "$CHAIR_SERVICE_ID" ]]; then
    DELIVERY=zeabur-ssh
  elif [[ -n "$CHAIR_SERVICE_ID" ]]; then
    DELIVERY=zeabur-exec
  elif command -v kubectl >/dev/null 2>&1; then
    DELIVERY=k8s
  else
    DELIVERY=files-only
  fi
  echo "delivery: $DELIVERY"
fi

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT INT TERM
cp "$KEY_PATH" "$TMP/.github-app.pem"
sed \
  -e "s/^APP_ID=.*/APP_ID=$APP_ID/" \
  -e "s/^INSTALLATION_ID=.*/INSTALLATION_ID=$INSTALLATION_ID/" \
  "$SCRIPT_DIR/get-gh-app-token.sh" >"$TMP/get-gh-app-token.sh"
chmod 600 "$TMP/.github-app.pem"
chmod 755 "$TMP/get-gh-app-token.sh"

echo "patching GitHub App webhook -> ${PLANE_URL}/api/v1/github_webhooks"
gh_app_patch_webhook "$APP_ID" "$TMP/.github-app.pem" "$WEBHOOK_SECRET" "$PLANE_URL" >/dev/null

sync_plane_vars() {
  [[ -n "$PLANE_SERVICE_ID" ]] || return 0
  command -v npx >/dev/null 2>&1 || {
    echo "warn: npx not found; skip plane variable sync" >&2
    return 0
  }
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
  npx zeabur@latest service restart --id "$PLANE_SERVICE_ID" -y -i=false >/dev/null
}

upload_chair_zeabur_ssh() {
  [[ -n "$SERVER_ID" && -n "$CHAIR_SERVICE_ID" ]] || die "zeabur-ssh needs --server-id and --chair-service-id"
  command -v sshpass >/dev/null 2>&1 || die "sshpass required for zeabur-ssh delivery"
  local ssh_info
  ssh_info=$(npx zeabur@latest server ssh-info --id "$SERVER_ID" -i=false --json)
  export TMP CHAIR_SERVICE_ID CHAIR_HOME CHAIR_USER ssh_info
  python3 <<'PY'
import base64, json, os, pathlib, subprocess
tmp = pathlib.Path(os.environ["TMP"])
chair = os.environ["CHAIR_SERVICE_ID"]
home = os.environ["CHAIR_HOME"]
user = os.environ["CHAIR_USER"]
info = json.loads(os.environ["ssh_info"])
pem_b64 = base64.b64encode((tmp / ".github-app.pem").read_bytes()).decode()
script_b64 = base64.b64encode((tmp / "get-gh-app-token.sh").read_bytes()).decode()
cmd = f'''
NS=$(sudo kubectl get pods -A | awk '/service-{chair}/ && /Running/ {{print $1; exit}}')
POD=$(sudo kubectl get pods -A | awk '/service-{chair}/ && /Running/ {{print $2; exit}}')
sudo kubectl exec -n "$NS" "$POD" -- sh -c 'mkdir -p {home}/bin'
echo '{pem_b64}' | base64 -d | sudo kubectl exec -i -n "$NS" "$POD" -- sh -c 'cat > {home}/.github-app.pem'
echo '{script_b64}' | base64 -d | sudo kubectl exec -i -n "$NS" "$POD" -- sh -c 'cat > {home}/bin/get-gh-app-token.sh'
sudo kubectl exec -n "$NS" "$POD" -- sh -c 'chmod 600 {home}/.github-app.pem; chmod +x {home}/bin/get-gh-app-token.sh; chown -R {user}:{user} {home}/.github-app.pem {home}/bin/get-gh-app-token.sh'
sudo kubectl exec -n "$NS" "$POD" -- sh -lc 'HOME={home} gh auth logout -h github.com -u zeabur-council[bot] 2>/dev/null || true'
sudo kubectl exec -n "$NS" "$POD" -- sh -lc 'HOME={home} {home}/bin/get-gh-app-token.sh | HOME={home} gh auth login --with-token'
sudo kubectl exec -n "$NS" "$POD" -- sh -lc 'HOME={home} gh auth status'
'''
subprocess.run(
    ["sshpass", "-p", info["password"], "ssh", "-o", "StrictHostKeyChecking=no",
     "-p", str(info["port"]), f'{info["username"]}@{info["ip"]}', cmd],
    check=True,
)
PY
}

upload_chair_zeabur_exec() {
  [[ -n "$CHAIR_SERVICE_ID" ]] || die "zeabur-exec needs --chair-service-id"
  local chair="$CHAIR_SERVICE_ID"
  echo "uploading via zeabur service exec (large keys may timeout on shared clusters)..."
  npx zeabur@latest service exec --id "$chair" -- \
    sh -c "mkdir -p ${CHAIR_HOME}/bin && cat > ${CHAIR_HOME}/.github-app.pem" \
    <"$TMP/.github-app.pem"
  npx zeabur@latest service exec --id "$chair" -- \
    sh -c "cat > ${CHAIR_HOME}/bin/get-gh-app-token.sh" \
    <"$TMP/get-gh-app-token.sh"
  npx zeabur@latest service exec --id "$chair" -- sh -c \
    "chmod 600 ${CHAIR_HOME}/.github-app.pem; chmod +x ${CHAIR_HOME}/bin/get-gh-app-token.sh"
  npx zeabur@latest service exec --id "$chair" -- sh -lc \
    "HOME=${CHAIR_HOME} gh auth logout -h github.com -u zeabur-council[bot] 2>/dev/null || true; \
     HOME=${CHAIR_HOME} ${CHAIR_HOME}/bin/get-gh-app-token.sh | HOME=${CHAIR_HOME} gh auth login --with-token; \
     HOME=${CHAIR_HOME} gh auth status"
}

upload_chair_k8s() {
  "$SCRIPT_DIR/dev-sync-gh-app-secret.sh" \
    --app-id "$APP_ID" \
    --installation-id "$INSTALLATION_ID" \
    --key-path "$TMP/.github-app.pem" \
    --namespace "$KUBE_NAMESPACE" \
    --secret-name "$KUBE_SECRET_NAME" \
    --any-context
  echo "mount ${KUBE_NAMESPACE}/${KUBE_SECRET_NAME} into chair via dev-deploy-bots.sh and restart chair"
}

upload_chair_files_only() {
  local out="./chair-github-app-bundle"
  mkdir -p "$out/bin"
  cp "$TMP/.github-app.pem" "$out/.github-app.pem"
  cp "$TMP/get-gh-app-token.sh" "$out/bin/get-gh-app-token.sh"
  chmod 600 "$out/.github-app.pem"
  chmod 755 "$out/bin/get-gh-app-token.sh"
  cat <<EOF

Wrote chair bundle to $out/
Upload to the chair pod:
  ${CHAIR_HOME}/.github-app.pem
  ${CHAIR_HOME}/bin/get-gh-app-token.sh
Then run:
  chmod 600 ${CHAIR_HOME}/.github-app.pem
  chmod +x ${CHAIR_HOME}/bin/get-gh-app-token.sh
  HOME=${CHAIR_HOME} ${CHAIR_HOME}/bin/get-gh-app-token.sh | HOME=${CHAIR_HOME} gh auth login --with-token
  HOME=${CHAIR_HOME} gh auth status
EOF
}

case "$DELIVERY" in
  zeabur-ssh)
    upload_chair_zeabur_ssh
    sync_plane_vars
    [[ -n "$CHAIR_SERVICE_ID" ]] && npx zeabur@latest service restart --id "$CHAIR_SERVICE_ID" -y -i=false >/dev/null
    ;;
  zeabur-exec)
    upload_chair_zeabur_exec
    sync_plane_vars
    npx zeabur@latest service restart --id "$CHAIR_SERVICE_ID" -y -i=false >/dev/null
    ;;
  k8s)
    upload_chair_k8s
    ;;
  files-only)
    upload_chair_files_only
    ;;
  *) die "unknown delivery: $DELIVERY" ;;
esac

echo "done: GitHub App $APP_ID wired to $PLANE_URL (bot: ${BOT_HANDLE}[bot])"