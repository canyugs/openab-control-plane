#!/usr/bin/env bash
# Copy the current local gh token into a Kubernetes Secret for local PR-write tests.
set -euo pipefail

KUBE_NAMESPACE="${KUBE_NAMESPACE:-oabcp-local}"
KUBE_CONTEXT="${KUBE_CONTEXT:-docker-desktop}"
SECRET_NAME="${SECRET_NAME:-gh-token}"
SECRET_KEY="${SECRET_KEY:-GH_TOKEN}"
CHECK_CONTEXT=1

usage() {
  cat <<'USAGE'
Usage:
  scripts/dev-sync-gh-token-secret.sh

Options:
  --secret-name <name>  Kubernetes Secret name. Default: gh-token.
  --secret-key <key>    Secret key name. Default: GH_TOKEN.
  --namespace <name>    Kubernetes namespace. Default: oabcp-local.
  --context <name>      Expected kubectl context. Default: docker-desktop.
  --any-context         Do not enforce the kubectl context.

This uses `gh auth token` and stores it as a Kubernetes Secret without printing
the token. Use it only for local development. The production GitHub App track
should keep using the chair pod's App identity setup.
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
    --secret-name) SECRET_NAME="${2:?--secret-name needs a value}"; shift 2 ;;
    --secret-name=*) SECRET_NAME="${1#*=}"; shift ;;
    --secret-key) SECRET_KEY="${2:?--secret-key needs a value}"; shift 2 ;;
    --secret-key=*) SECRET_KEY="${1#*=}"; shift ;;
    --namespace) KUBE_NAMESPACE="${2:?--namespace needs a value}"; shift 2 ;;
    --namespace=*) KUBE_NAMESPACE="${1#*=}"; shift ;;
    --context) KUBE_CONTEXT="${2:?--context needs a value}"; shift 2 ;;
    --context=*) KUBE_CONTEXT="${1#*=}"; shift ;;
    --any-context) CHECK_CONTEXT=0; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
done

need gh
need kubectl

if [[ "$CHECK_CONTEXT" == "1" ]]; then
  current_context=$(kubectl config current-context)
  [[ "$current_context" == "$KUBE_CONTEXT" ]] ||
    die "kubectl context is '$current_context', expected '$KUBE_CONTEXT' (or pass --any-context)"
fi

TMP_DIR=$(mktemp -d "${TMPDIR:-/tmp}/oabcp-gh-token.XXXXXX")
cleanup() {
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT INT TERM

umask 077
TOKEN_FILE="$TMP_DIR/token"
TOKEN=$(gh auth token)
printf '%s' "$TOKEN" >"$TOKEN_FILE"
[[ -s "$TOKEN_FILE" ]] || die "gh auth token returned an empty token"

kubectl create namespace "$KUBE_NAMESPACE" --dry-run=client -o yaml | kubectl apply -f - >/dev/null
kubectl -n "$KUBE_NAMESPACE" create secret generic "$SECRET_NAME" \
  "--from-file=$SECRET_KEY=$TOKEN_FILE" \
  --dry-run=client -o yaml | kubectl apply -f - >/dev/null

echo "updated Kubernetes Secret $KUBE_NAMESPACE/$SECRET_NAME key $SECRET_KEY"
