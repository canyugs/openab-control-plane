#!/usr/bin/env bash
# Shared helpers for GitHub App install scripts. Source, do not execute.
[[ -n "${GITHUB_APP_LIB_LOADED:-}" ]] && return 0
GITHUB_APP_LIB_LOADED=1

gh_app_need() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "error: missing required command: $1" >&2
    exit 1
  }
}

gh_app_slug_from_name() {
  printf '%s' "$1" | tr '[:upper:]' '[:lower:]' | sed -E 's/[^a-z0-9]+/-/g; s/^-+|-+$//g'
}

gh_app_jwt() {
  local app_id="$1" key_path="$2"
  gh_app_need node
  APP_ID="$app_id" KEY_PATH="$key_path" node - <<'NODE'
const fs = require("fs");
const crypto = require("crypto");
const appId = process.env.APP_ID;
const key = fs.readFileSync(process.env.KEY_PATH, "utf8");
const b64 = (obj) => Buffer.from(JSON.stringify(obj)).toString("base64url");
const now = Math.floor(Date.now() / 1000);
const data = [b64({ alg: "RS256", typ: "JWT" }), b64({ iat: now - 60, exp: now + 540, iss: appId })].join(".");
const sig = crypto.createSign("RSA-SHA256").update(data).sign(key, "base64url");
process.stdout.write(`${data}.${sig}`);
NODE
}

gh_app_patch_webhook() {
  local app_id="$1" key_path="$2" webhook_secret="$3" plane_url="$4"
  local jwt target
  jwt=$(gh_app_jwt "$app_id" "$key_path")
  target="${plane_url%/}/api/v1/github_webhooks"
  gh api /app/hook/config -X PATCH \
    -f "url=$target" \
    -f content_type=json \
    -f insecure_ssl=0 \
    -f "secret=$webhook_secret" \
    -H "Authorization: Bearer $jwt" \
    -H "Accept: application/vnd.github+json"
}

gh_app_list_installations() {
  local app_id="$1" key_path="$2"
  local jwt
  jwt=$(gh_app_jwt "$app_id" "$key_path")
  gh api /app/installations \
    -H "Authorization: Bearer $jwt" \
    -H "Accept: application/vnd.github+json"
}

gh_app_install_url() {
  local slug="$1" org_login="${2:-}"
  if [[ -n "$org_login" ]]; then
    local org_id
    org_id=$(gh api "/orgs/$org_login" --jq .id)
    printf 'https://github.com/apps/%s/installations/new?target_id=%s&target_type=Organization' "$slug" "$org_id"
  else
    printf 'https://github.com/apps/%s/installations/new' "$slug"
  fi
}