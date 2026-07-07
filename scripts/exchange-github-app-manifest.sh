#!/usr/bin/env bash
# Exchange a GitHub App manifest temporary code for App credentials.
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib/github-app.sh
source "$SCRIPT_DIR/lib/github-app.sh"

CODE=""
CODE_FILE=""
OUTPUT_DIR="${OUTPUT_DIR:-.}"
OUTPUT_PEM=""
OUTPUT_JSON=""

usage() {
  cat <<'USAGE'
Usage:
  scripts/exchange-github-app-manifest.sh --code <code>
  scripts/exchange-github-app-manifest.sh --code-file <path>

Options:
  --output-dir <dir>     Directory for artifacts (default: .)
  --output-pem <path>    PEM path (default: <output-dir>/<slug>.private-key.pem)
  --output-json <path>   JSON path (default: <output-dir>/<slug>.github-app.json)

Exchanges the manifest code within one hour of registration. Requires `gh` and network.
Writes the private key and a redacted JSON summary (no client_secret in summary file).
USAGE
}

die() { echo "error: $*" >&2; exit 1; }

while [[ $# -gt 0 ]]; do
  case "$1" in
    --code) CODE="${2:?}"; shift 2 ;;
    --code-file) CODE_FILE="${2:?}"; shift 2 ;;
    --output-dir) OUTPUT_DIR="${2:?}"; shift 2 ;;
    --output-pem) OUTPUT_PEM="${2:?}"; shift 2 ;;
    --output-json) OUTPUT_JSON="${2:?}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown arg: $1" ;;
  esac
done

[[ -n "$CODE" || -n "$CODE_FILE" ]] || die "--code or --code-file required"
gh_app_need gh
gh_app_need python3

if [[ -z "$CODE" ]]; then
  [[ -r "$CODE_FILE" ]] || die "code file not readable: $CODE_FILE"
  CODE=$(tr -d '[:space:]' <"$CODE_FILE")
fi
[[ -n "$CODE" ]] || die "manifest code is empty"

mkdir -p "$OUTPUT_DIR"
RAW_JSON=$(mktemp "${TMPDIR:-/tmp}/gh-app-manifest.XXXXXX.json")
trap 'rm -f "$RAW_JSON"' EXIT INT TERM

gh api -X POST "/app-manifests/${CODE}/conversions" >"$RAW_JSON"

python3 - "$RAW_JSON" "$OUTPUT_DIR" "$OUTPUT_PEM" "$OUTPUT_JSON" <<'PY'
import json, pathlib, sys

raw_path, out_dir, pem_override, json_override = sys.argv[1:5]
data = json.loads(pathlib.Path(raw_path).read_text())
slug = data.get("slug") or "github-app"
out = pathlib.Path(out_dir)
pem_path = pathlib.Path(pem_override) if pem_override else out / f"{slug}.private-key.pem"
json_path = pathlib.Path(json_override) if json_override else out / f"{slug}.github-app.json"

pem = data.get("pem") or ""
if not pem:
    raise SystemExit("conversion response missing pem")
pem_path.write_text(pem)
pem_path.chmod(0o600)

summary = {
    "id": data.get("id"),
    "slug": slug,
    "name": data.get("name"),
    "owner": (data.get("owner") or {}).get("login"),
    "html_url": data.get("html_url"),
    "webhook_secret": data.get("webhook_secret"),
    "permissions": data.get("permissions"),
    "events": data.get("events"),
    "pem_path": str(pem_path),
}
json_path.write_text(json.dumps(summary, indent=2) + "\n")
json_path.chmod(0o600)

print(json.dumps(summary, indent=2))
PY