#!/usr/bin/env bash
# SOP entry point for GitHub App install — see docs/install-github-app.md
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
DOC="$SCRIPT_DIR/../docs/install-github-app.md"
QUICKSTART="$SCRIPT_DIR/../docs/install-github-app-quickstart.md"

usage() {
  cat <<USAGE
OpenAB Control Plane — GitHub App install (SOP)

Quick start (non-technical, 繁中): docs/install-github-app-quickstart.md
Full SOP: docs/install-github-app.md

Quick map:
  1. Deploy OCP (template 1E1Y97) → save WEBHOOK_SECRET and PLANE_URL
  2. Create App:
       manual   — production, no tunnel
       manifest — scripted; needs python3; optional cloudflared
  3. Install App on org/repos → get installation_id
  4. Wire: setup-github-app.sh (--delivery zeabur-ssh | zeabur-exec | k8s | files-only)
  5. Verify: chair gh auth status; PR or /review

Subcommands (wrap SOP steps):
  quickstart        Print path to one-page quick start (繁中)
  docs              Print path to SOP document
  manual            §6.2 — print manual UI checklist
  manifest          §6.3 — manifest registration (see register-github-app.sh)
  install-url       §6.4 — print GitHub install URL
  list-installations §6.4 — list installation IDs
  wire              §7   — alias for setup-github-app.sh
  worksheet         Print blank artifact worksheet

Examples:
  $0 docs
  $0 manual --plane-url https://my-council.zeabur.app --app-name "My Council" --org my-org
  $0 manifest --plane-url https://my-council.zeabur.app --app-name "My Council" --org my-org
  $0 install-url --slug my-council --org my-org
  $0 wire --app-id ID --installation-id IID --key-path ./app.pem --plane-url https://... --webhook-secret SECRET ...

Environment shortcuts for wire:
  PLANE_URL, GITHUB_WEBHOOK_SECRET, GITHUB_APP_ID, GITHUB_APP_INSTALLATION_ID,
  GITHUB_APP_PRIVATE_KEY_PATH, CHAIR_SERVICE_ID, PLANE_SERVICE_ID, SERVER_ID
USAGE
}

worksheet() {
  cat <<'WS'
Artifact worksheet (copy from docs/install-github-app.md §12)
PLANE_URL=________________________
WEBHOOK_SECRET=__________________
APP_NAME=________________________
APP_ID=__________________________
APP_SLUG=________________________
INSTALLATION_ID=__________________
PEM_PATH=_________________________
CHAIR_SERVICE_ID=_________________
PLANE_SERVICE_ID=_________________
SERVER_ID=________________________
CHAIR_HOME=/home/agent or /home/node
WS
}

cmd="${1:-}"
shift || true

case "$cmd" in
  ""|-h|--help|help) usage ;;
  quickstart) printf '%s\n' "$QUICKSTART" ;;
  docs) printf '%s\n' "$DOC" ;;
  worksheet) worksheet ;;
  manual|manifest|install-url|exchange)
    exec "$SCRIPT_DIR/register-github-app.sh" "$cmd" "$@"
    ;;
  list-installations)
    exec "$SCRIPT_DIR/github-app-list-installations.sh" "$@"
    ;;
  wire)
    exec "$SCRIPT_DIR/setup-github-app.sh" "$@"
    ;;
  *)
    echo "error: unknown subcommand: $cmd" >&2
    usage >&2
    exit 1
    ;;
esac