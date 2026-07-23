#!/usr/bin/env bash
# Submit authenticated plan-only shadow comparisons without exposing the HMAC secret in argv.
set -euo pipefail

MODE="${1:-}"
if [[ $# -gt 0 ]]; then
  shift
fi

CONTROLLER_URL="${GITHUB_CONTROLLER_URL:-http://127.0.0.1:8091}"
COMPARISON_ID=""
DELIVERY_ID=""
EVENT_TYPE=""
PAYLOAD_PATH=""
EMBEDDED_PATH=""
SHADOW_SECRET="${GITHUB_CONTROLLER_SHADOW_SECRET:-}"

usage() {
  cat <<'USAGE'
Usage:
  scripts/github-controller-shadow.sh compare [options]
  scripts/github-controller-shadow.sh summary [--url <controller-url>]

Compare options:
  --url <url>               Controller base URL. Default: http://127.0.0.1:8091.
  --comparison-id <id>      Unique comparison id (letters, numbers, and hyphens).
  --delivery-id <id>        Original or synthetic GitHub delivery id.
  --event <event>           GitHub event type, for example pull_request.
  --payload <path>          Raw GitHub JSON payload file.
  --embedded <path>         Embedded parity outcome JSON file, or a file containing null.

Environment:
  GITHUB_CONTROLLER_SHADOW_SECRET  Required HMAC secret.
  GITHUB_CONTROLLER_URL            Optional base URL.
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
    --url) CONTROLLER_URL="${2:?--url needs a value}"; shift 2 ;;
    --url=*) CONTROLLER_URL="${1#*=}"; shift ;;
    --comparison-id) COMPARISON_ID="${2:?--comparison-id needs a value}"; shift 2 ;;
    --comparison-id=*) COMPARISON_ID="${1#*=}"; shift ;;
    --delivery-id) DELIVERY_ID="${2:?--delivery-id needs a value}"; shift 2 ;;
    --delivery-id=*) DELIVERY_ID="${1#*=}"; shift ;;
    --event) EVENT_TYPE="${2:?--event needs a value}"; shift 2 ;;
    --event=*) EVENT_TYPE="${1#*=}"; shift ;;
    --payload) PAYLOAD_PATH="${2:?--payload needs a path}"; shift 2 ;;
    --payload=*) PAYLOAD_PATH="${1#*=}"; shift ;;
    --embedded) EMBEDDED_PATH="${2:?--embedded needs a path}"; shift 2 ;;
    --embedded=*) EMBEDDED_PATH="${1#*=}"; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
done

[[ "$MODE" == "compare" || "$MODE" == "summary" ]] || {
  usage >&2
  exit 1
}
[[ -n "$SHADOW_SECRET" ]] || die "set GITHUB_CONTROLLER_SHADOW_SECRET"

need curl
need node
CONTROLLER_URL="${CONTROLLER_URL%/}"

sign_body() {
  GITHUB_CONTROLLER_SHADOW_SECRET="$SHADOW_SECRET" BODY_PATH="${1:-}" node <<'NODE'
const crypto = require("crypto");
const fs = require("fs");

const path = process.env.BODY_PATH;
const body = path ? fs.readFileSync(path) : Buffer.alloc(0);
const digest = crypto
  .createHmac("sha256", process.env.GITHUB_CONTROLLER_SHADOW_SECRET)
  .update(body)
  .digest("hex");
process.stdout.write(`sha256=${digest}`);
NODE
}

if [[ "$MODE" == "summary" ]]; then
  signature="$(sign_body)"
  curl -sS --fail-with-body \
    -H "x-shadow-signature-256: ${signature}" \
    "${CONTROLLER_URL}/api/v1/shadow/summary"
  echo
  exit 0
fi

need jq
[[ -n "$COMPARISON_ID" ]] || die "--comparison-id is required"
[[ -n "$DELIVERY_ID" ]] || die "--delivery-id is required"
[[ -n "$EVENT_TYPE" ]] || die "--event is required"
[[ -r "$PAYLOAD_PATH" ]] || die "payload file is not readable: $PAYLOAD_PATH"
[[ -r "$EMBEDDED_PATH" ]] || die "embedded file is not readable: $EMBEDDED_PATH"

TEMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/github-controller-shadow.XXXXXX")"
trap 'rm -rf "$TEMP_DIR"' EXIT
WRAPPER_PATH="${TEMP_DIR}/wrapper.json"

jq -c -n \
  --arg comparison_id "$COMPARISON_ID" \
  --arg delivery_id "$DELIVERY_ID" \
  --arg event_type "$EVENT_TYPE" \
  --slurpfile payload "$PAYLOAD_PATH" \
  --slurpfile embedded "$EMBEDDED_PATH" \
  '{
    comparison_id: $comparison_id,
    delivery_id: $delivery_id,
    event_type: $event_type,
    payload: $payload[0],
    embedded: $embedded[0]
  }' >"$WRAPPER_PATH"

signature="$(sign_body "$WRAPPER_PATH")"
curl -sS --fail-with-body \
  -H 'content-type: application/json' \
  -H "x-shadow-signature-256: ${signature}" \
  --data-binary "@${WRAPPER_PATH}" \
  "${CONTROLLER_URL}/api/v1/shadow/compare"
echo
