# GitHub App identity — end-to-end validation (L3)

Archived note. The maintained installation path is
[`../install-github-app.md`](../install-github-app.md); this file is only the
historical real-network validation checklist from the initial App enablement.

Tracking issue: [#9](https://github.com/canyugs/openab-control-plane/issues/9)

The GitHub App identity in [PR #8](https://github.com/canyugs/openab-control-plane/pull/8)
is fully tested **except the actual GitHub network path** — the JWT → installation-token
exchange and per-role permission scoping — which only a real App can confirm. App mode is
opt-in (`github_app = None` → PAT mode), so this is **enable-time** validation, not
merge-time.

## What's already proven (no App)

- **L1 unit** (`cargo test`): role→permissions, PEM normalization, token cache/freshness,
  HMAC algorithm, webhook trigger parsing.
- **L2 local end-to-end**: run the plane with a webhook secret and POST a signed payload —
  verify → open session, fail-closed on missing secret, idempotent re-delivery,
  `/github-token` returns 501 in PAT mode. (See the L2 commands at the bottom.)

## L3 — what only a real App can prove

1. App JWT (RS256) accepted by GitHub → installation token minted.
2. **Per-role scoping holds**: chair token creates a PR review (`pull_requests:write`);
   reviewer token is **403** on the same call.
3. A real GitHub webhook reaches `/api/v1/github_webhooks` and opens a session.

## Steps

1. **Create a GitHub App** (Settings → Developer settings → GitHub Apps → New):
   - Permissions: `Pull requests: Read & write`, `Contents: Read-only`.
   - Subscribe to events: `Pull request`, `Issue comment`.
   - Webhook URL: your plane's public `/api/v1/github_webhooks`.
   - Webhook secret: the same value you'll set as `GITHUB_WEBHOOK_SECRET`.
   - Generate a private key (`.pem`).
2. **Install** the App on a test repo → note the **installation id** (in the install URL
   or via `GET /app/installations`).
3. **Set env** for the gated tests:
   ```bash
   export GITHUB_APP_ID=...
   export GITHUB_APP_INSTALLATION_ID=...
   export GITHUB_APP_PRIVATE_KEY="$(cat path/to/key.pem)"
   export GITHUB_TEST_REPO=owner/repo      # an open PR the install can access
   export GITHUB_TEST_PR=123
   ```
4. **Run the gated tests** (`tests/l3_github_app.rs`):
   ```bash
   cargo test --test l3_github_app -- --ignored --nocapture
   ```
   - `l3_mints_chair_and_reviewer_tokens` → proves step 1 (App env only).
   - `l3_role_scoping_chair_writes_reviewer_blocked` → proves step 2 (needs the test PR).
5. **Webhook from real GitHub** (step 3): point the App webhook at the running plane (deploy
   a test service, or tunnel locally with smee.io / cloudflared), open a PR, and confirm a
   session appears (`GET /v1/sessions/:id` or the north SSE stream).

## Acceptance

All three proven → App mode is safe to enable; check the boxes on #9 and close it.

---

### Appendix — L2 local run (no App)

```bash
OABCP_DB=/tmp/ocp.db OABCP_ADDR=127.0.0.1:8099 GITHUB_WEBHOOK_SECRET=testsecret \
  ./target/debug/openab-control-plane &
PR='{"action":"opened","installation":{"id":99},"repository":{"full_name":"o/r"},"pull_request":{"number":1,"url":"https://api.github.com/repos/o/r/pulls/1"}}'
SIG=$(printf '%s' "$PR" | openssl dgst -sha256 -hmac testsecret -hex | sed 's/^.*= /sha256=/')
curl -s -X POST 127.0.0.1:8099/api/v1/github_webhooks \
  -H "X-GitHub-Event: pull_request" -H "X-Hub-Signature-256: $SIG" \
  -H 'Content-Type: application/json' -d "$PR"   # → {"ok":true,"triggered":true,"session_id":...}
```
