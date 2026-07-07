#!/usr/bin/env bash
# List installations for a GitHub App (JWT auth).
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib/github-app.sh
source "$SCRIPT_DIR/lib/github-app.sh"

APP_ID="${GITHUB_APP_ID:-}"
KEY_PATH="${GITHUB_APP_PRIVATE_KEY_PATH:-}"

usage() {
  cat <<'USAGE'
Usage:
  scripts/github-app-list-installations.sh --app-id <id> --key-path <pem>

Prints: installation_id account_login repository_selection
USAGE
}

die() { echo "error: $*" >&2; exit 1; }

while [[ $# -gt 0 ]]; do
  case "$1" in
    --app-id) APP_ID="${2:?}"; shift 2 ;;
    --key-path) KEY_PATH="${2:?}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown arg: $1" ;;
  esac
done

[[ -n "$APP_ID" ]] || die "--app-id required"
[[ -r "$KEY_PATH" ]] || die "--key-path must be readable"

gh_app_need gh
gh_app_need python3

json=$(gh_app_list_installations "$APP_ID" "$KEY_PATH")
python3 - <<'PY' "$json"
import json, sys
items = json.loads(sys.argv[1])
if not items:
    print("no installations")
    raise SystemExit(1)
for inst in items:
    acc = inst.get("account") or {}
    print(f"{inst['id']}\t{acc.get('login','?')}\t{inst.get('repository_selection','?')}")
PY