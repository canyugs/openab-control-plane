#!/usr/bin/env bash
# Copy a GitHub App private key plus a filled token-minter into a local K8s Secret.
set -euo pipefail

KUBE_NAMESPACE="${KUBE_NAMESPACE:-oabcp-local}"
KUBE_CONTEXT="${KUBE_CONTEXT:-docker-desktop}"
SECRET_NAME="${SECRET_NAME:-github-app-chair}"
APP_ID="${GITHUB_APP_ID:-4146119}"
INSTALLATION_ID="${GITHUB_APP_INSTALLATION_ID:-}"
KEY_PATH="${GITHUB_APP_PRIVATE_KEY_PATH:-}"
CHECK_CONTEXT=1

usage() {
  cat <<'USAGE'
Usage:
  scripts/dev-sync-gh-app-secret.sh \
    --key-path <app-private-key.pem> \
    --installation-id <id>

Options:
  --app-id <id>             GitHub App ID. Default: GITHUB_APP_ID or 4146119.
  --installation-id <id>    GitHub App installation ID.
  --key-path <path>         GitHub App private key PEM. Or set GITHUB_APP_PRIVATE_KEY_PATH.
  --secret-name <name>      Kubernetes Secret name. Default: github-app-chair.
  --namespace <name>        Kubernetes namespace. Default: oabcp-local.
  --context <name>          Expected kubectl context. Default: docker-desktop.
  --any-context             Do not enforce the kubectl context.

This creates a Secret with:
  .github-app.pem
  get-gh-app-token.sh

Mount it only into the chair pod via:
  scripts/dev-deploy-bots.sh --chair-github-app-secret github-app-chair
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
    --app-id) APP_ID="${2:?--app-id needs a value}"; shift 2 ;;
    --app-id=*) APP_ID="${1#*=}"; shift ;;
    --installation-id) INSTALLATION_ID="${2:?--installation-id needs a value}"; shift 2 ;;
    --installation-id=*) INSTALLATION_ID="${1#*=}"; shift ;;
    --key-path) KEY_PATH="${2:?--key-path needs a path}"; shift 2 ;;
    --key-path=*) KEY_PATH="${1#*=}"; shift ;;
    --secret-name) SECRET_NAME="${2:?--secret-name needs a value}"; shift 2 ;;
    --secret-name=*) SECRET_NAME="${1#*=}"; shift ;;
    --namespace) KUBE_NAMESPACE="${2:?--namespace needs a value}"; shift 2 ;;
    --namespace=*) KUBE_NAMESPACE="${1#*=}"; shift ;;
    --context) KUBE_CONTEXT="${2:?--context needs a value}"; shift 2 ;;
    --context=*) KUBE_CONTEXT="${1#*=}"; shift ;;
    --any-context) CHECK_CONTEXT=0; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
done

need kubectl
[[ -n "$APP_ID" ]] || die "--app-id is required"
[[ -n "$INSTALLATION_ID" ]] || die "--installation-id is required"
[[ -n "$KEY_PATH" ]] || die "--key-path or GITHUB_APP_PRIVATE_KEY_PATH is required"
[[ -r "$KEY_PATH" ]] || die "private key file is not readable: $KEY_PATH"

if [[ "$CHECK_CONTEXT" == "1" ]]; then
  current_context=$(kubectl config current-context)
  [[ "$current_context" == "$KUBE_CONTEXT" ]] ||
    die "kubectl context is '$current_context', expected '$KUBE_CONTEXT' (or pass --any-context)"
fi

TMP_DIR=$(mktemp -d "${TMPDIR:-/tmp}/oabcp-gh-app.XXXXXX")
cleanup() {
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT INT TERM

umask 077
cp "$KEY_PATH" "$TMP_DIR/.github-app.pem"
sed \
  -e "s/^APP_ID=.*/APP_ID=$APP_ID/" \
  -e "s/^INSTALLATION_ID=.*/INSTALLATION_ID=$INSTALLATION_ID/" \
  scripts/get-gh-app-token.sh >"$TMP_DIR/get-gh-app-token.sh"
chmod 600 "$TMP_DIR/.github-app.pem"
chmod 755 "$TMP_DIR/get-gh-app-token.sh"

kubectl create namespace "$KUBE_NAMESPACE" --dry-run=client -o yaml | kubectl apply -f - >/dev/null
kubectl -n "$KUBE_NAMESPACE" create secret generic "$SECRET_NAME" \
  "--from-file=.github-app.pem=$TMP_DIR/.github-app.pem" \
  "--from-file=get-gh-app-token.sh=$TMP_DIR/get-gh-app-token.sh" \
  --dry-run=client -o yaml | kubectl apply -f - >/dev/null

echo "updated Kubernetes Secret $KUBE_NAMESPACE/$SECRET_NAME for GitHub App $APP_ID installation $INSTALLATION_ID"
