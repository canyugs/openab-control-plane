#!/bin/sh
# Mint a GitHub App installation token for the chair's App identity.
#
# Lives on the chair pod's persistent volume at ~/bin/get-gh-app-token.sh and is
# called (no args) by the chair-only [hooks.pre_boot] that bot_config serves:
#   get-gh-app-token.sh | gh auth login --with-token
# so the chair's `gh` posts the verdict as the App (e.g. zeabur-council[bot]).
#
# Edit APP_ID / INSTALLATION_ID below for your App; the App private key (PEM) lives
# next to it at ~/.github-app.pem (chmod 600, owned by the pod user). No env is read
# — the pre_boot hook runs with a sanitized environment, so the values are baked here.
# Mirrors multi-agent-review-ops/github-apps/get-token.sh. Needs: openssl, curl.
set -e

APP_ID=REPLACE_WITH_APP_ID
INSTALLATION_ID=REPLACE_WITH_INSTALLATION_ID
KEY="${HOME}/.github-app.pem"

NOW=$(date +%s)
b64() { openssl base64 -A | tr '+/' '-_' | tr -d '='; }
HEADER=$(printf '{"alg":"RS256","typ":"JWT"}' | b64)
PAYLOAD=$(printf '{"iat":%d,"exp":%d,"iss":"%s"}' "$((NOW - 60))" "$((NOW + 540))" "$APP_ID" | b64)
SIG=$(printf '%s.%s' "$HEADER" "$PAYLOAD" | openssl dgst -sha256 -sign "$KEY" -binary | b64)
JWT="${HEADER}.${PAYLOAD}.${SIG}"

# Capture the response and check curl's exit BEFORE extracting — with `set -e` and a
# pipe, the pipeline's status is the last command's (`head`), so a failed `curl -sf`
# would otherwise pass silently as empty output and the caller's `gh auth login` would
# fail with a misleading error. GitHub's JSON has a space after the colon → match loosely
# (no jq dependency).
RESP=$(curl -sf -X POST \
  -H "Authorization: Bearer ${JWT}" \
  -H "Accept: application/vnd.github+json" \
  "https://api.github.com/app/installations/${INSTALLATION_ID}/access_tokens") || {
  echo "get-gh-app-token: GitHub access_tokens call failed (bad JWT / installation id / PEM?)" >&2
  exit 1
}
printf '%s' "$RESP" | sed -n 's/.*"token"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -1
