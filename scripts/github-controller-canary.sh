#!/usr/bin/env bash
# Inspect the external GitHub canary and enforce the no-in-flight rollback gate.
set -euo pipefail

MODE="${1:-}"
if [[ $# -gt 0 ]]; then
  shift
fi

CONTROLLER_URL="${GITHUB_CONTROLLER_URL:-http://127.0.0.1:8091}"
PLANE_URL="${OABCP_URL:-http://127.0.0.1:8080}"
OBSERVER_SECRET="${GITHUB_CONTROLLER_OBSERVER_SECRET:-}"
PLANE_KEY="${OABCP_KEY:-}"
REPOSITORY=""
EXPECTED_EMBEDDED_COUNT=""
ROUTE_REVISION=""

usage() {
  cat <<'USAGE'
Usage:
  scripts/github-controller-canary.sh preflight --repository <owner/repo> [--url <controller-url>]
  scripts/github-controller-canary.sh summary [--url <controller-url>]
  scripts/github-controller-canary.sh rollback-gate --repository <owner/repo> \
    --expected-embedded-count <count> --route-revision <revision> \
    [--url <controller-url>] [--plane-url <ocp-url>]

Environment:
  GITHUB_CONTROLLER_OBSERVER_SECRET  Required for summary and rollback-gate.
  GITHUB_CONTROLLER_URL              Optional controller base URL.
  OABCP_KEY                          Required for rollback-gate telemetry.
  OABCP_URL                          Optional OCP base URL.

rollback-gate is read-only. Run it after stopping the external route and before
atomically restoring the embedded route. It fails while a controller delivery
is processing/retryable or the repository's embedded counter changed.
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
    --plane-url) PLANE_URL="${2:?--plane-url needs a value}"; shift 2 ;;
    --plane-url=*) PLANE_URL="${1#*=}"; shift ;;
    --repository) REPOSITORY="${2:?--repository needs a value}"; shift 2 ;;
    --repository=*) REPOSITORY="${1#*=}"; shift ;;
    --expected-embedded-count)
      EXPECTED_EMBEDDED_COUNT="${2:?--expected-embedded-count needs a value}"
      shift 2
      ;;
    --expected-embedded-count=*) EXPECTED_EMBEDDED_COUNT="${1#*=}"; shift ;;
    --route-revision) ROUTE_REVISION="${2:?--route-revision needs a value}"; shift 2 ;;
    --route-revision=*) ROUTE_REVISION="${1#*=}"; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
done

[[ "$MODE" == "preflight" || "$MODE" == "summary" || "$MODE" == "rollback-gate" ]] || {
  usage >&2
  exit 1
}

need curl
need jq
CONTROLLER_URL="${CONTROLLER_URL%/}"
PLANE_URL="${PLANE_URL%/}"

canary_summary() {
  [[ -n "$OBSERVER_SECRET" ]] || die "set GITHUB_CONTROLLER_OBSERVER_SECRET"
  need node
  local signature
  signature="$(GITHUB_CONTROLLER_OBSERVER_SECRET="$OBSERVER_SECRET" node <<'NODE'
const crypto = require("crypto");
const digest = crypto
  .createHmac("sha256", process.env.GITHUB_CONTROLLER_OBSERVER_SECRET)
  .update(Buffer.alloc(0))
  .digest("hex");
process.stdout.write(`sha256=${digest}`);
NODE
)"
  curl -sS --fail-with-body \
    -H "x-canary-signature-256: ${signature}" \
    "${CONTROLLER_URL}/api/v1/canary/summary"
}

if [[ "$MODE" == "summary" ]]; then
  canary_summary | jq
  exit 0
fi

[[ "$REPOSITORY" =~ ^[^/]+/[^/]+$ ]] || die "--repository must be owner/repo"
readiness="$(curl -sS --fail-with-body "${CONTROLLER_URL}/readyz")"
jq -e --arg repository "$REPOSITORY" '
  .status == "ready" and
  .mode == "external_canary" and
  .components.ingress.ready == true and
  .components.ocp.ready == true and
  .components.runtime_events.ready == true and
  .components.github.enabled == false and
  .components.ownership.detail == ("external ingress owned for exactly " + $repository)
' >/dev/null <<<"$readiness" || die "controller readiness or exact repository ownership failed"

if [[ "$MODE" == "preflight" ]]; then
  jq '{status, mode, components}' <<<"$readiness"
  exit 0
fi

[[ "$EXPECTED_EMBEDDED_COUNT" =~ ^[0-9]+$ ]] || {
  die "--expected-embedded-count must be a non-negative integer"
}
[[ -n "$ROUTE_REVISION" ]] || die "--route-revision is required"
[[ -n "$PLANE_KEY" ]] || die "set OABCP_KEY"
need node

summary="$(canary_summary)"
jq -e '
  .ok == true and
  .summary.processing_deliveries == 0 and
  .summary.retryable_deliveries == 0
' >/dev/null <<<"$summary" || die "controller still has processing or retryable deliveries"

repository_digest="$(REPOSITORY="$REPOSITORY" node <<'NODE'
const crypto = require("crypto");
process.stdout.write(crypto.createHash("sha256").update(process.env.REPOSITORY).digest("hex"));
NODE
)"
surface="embedded_github_webhook_repo:${repository_digest}"
usage="$(curl -sS --fail-with-body \
  -H "Authorization: Bearer ${PLANE_KEY}" \
  "${PLANE_URL}/v1/compatibility-usage")"
current_count="$(jq -r --arg surface "$surface" '
  ([.usage[]? | select(.surface == $surface) | .uses][0] // 0)
' <<<"$usage")"
[[ "$current_count" == "$EXPECTED_EMBEDDED_COUNT" ]] || {
  die "embedded repository counter changed: expected ${EXPECTED_EMBEDDED_COUNT}, got ${current_count}"
}

jq -n \
  --arg repository "$REPOSITORY" \
  --arg route_revision "$ROUTE_REVISION" \
  --argjson embedded_count "$current_count" \
  --argjson controller_summary "$(jq '.summary' <<<"$summary")" \
  '{
    ok: true,
    repository: $repository,
    stopped_external_route_revision: $route_revision,
    embedded_counter: $embedded_count,
    controller: $controller_summary,
    next: "atomically restore the recorded embedded route; never enable both routes"
  }'
