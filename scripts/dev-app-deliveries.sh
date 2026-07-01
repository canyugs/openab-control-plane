#!/usr/bin/env bash
# List recent GitHub App webhook deliveries without printing App credentials.
set -euo pipefail

APP_ID="${GITHUB_APP_ID:-4146119}"
KEY_PATH="${GITHUB_APP_PRIVATE_KEY_PATH:-}"
LIMIT="${LIMIT:-10}"

usage() {
  cat <<'USAGE'
Usage:
  scripts/dev-app-deliveries.sh --key-path <app-private-key.pem> [--limit 10]

Options:
  --key-path <path>       GitHub App private key PEM. Or set GITHUB_APP_PRIVATE_KEY_PATH.
  --app-id <id>           GitHub App ID. Default: GITHUB_APP_ID or 4146119.
  --limit <n>             Number of recent deliveries to show. Default: 10.

Output columns:
  delivered_at, event, action, status_code, status, guid
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
    --key-path) KEY_PATH="${2:?--key-path needs a path}"; shift 2 ;;
    --key-path=*) KEY_PATH="${1#*=}"; shift ;;
    --app-id) APP_ID="${2:?--app-id needs a value}"; shift 2 ;;
    --app-id=*) APP_ID="${1#*=}"; shift ;;
    --limit) LIMIT="${2:?--limit needs a value}"; shift 2 ;;
    --limit=*) LIMIT="${1#*=}"; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
done

need gh
need node
[[ -n "$KEY_PATH" ]] || die "set --key-path or GITHUB_APP_PRIVATE_KEY_PATH"
[[ -r "$KEY_PATH" ]] || die "private key file is not readable: $KEY_PATH"

JWT=$(APP_ID="$APP_ID" KEY_PATH="$KEY_PATH" node - <<'NODE'
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
)

gh api /app/hook/deliveries \
  -H "Authorization: Bearer $JWT" \
  -H "Accept: application/vnd.github+json" \
  --jq ".[:$LIMIT][] | [.delivered_at, .event, .action, .status_code, .status, .guid] | @tsv"
