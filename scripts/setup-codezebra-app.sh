#!/usr/bin/env bash
# Deprecated wrapper — use scripts/setup-github-app.sh instead.
set -euo pipefail
SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
echo "note: setup-codezebra-app.sh is a thin wrapper; prefer scripts/setup-github-app.sh" >&2
exec "$SCRIPT_DIR/setup-github-app.sh" \
  --bot-handle "${BOT_HANDLE:-opencodezebra}" \
  "$@"