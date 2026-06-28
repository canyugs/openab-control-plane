# OpenAB Review Council — Zeabur template

One-click deploy of a multi-agent PR-review council: a control plane plus stock
OpenAB Claude pods (1 chair + 2 reviewers) that deliberate and post a verdict.

## What deploys
- `control-plane` — this repo's image, REST/SSE on a public domain, `/ws` gateway
  internally, SQLite on a `/data` volume. Seeds a fixed roster at boot
  (`OABCP_BOTS=chair:chair,rev1:reviewer,rev2:reviewer`) so no manual bot
  registration is needed.
- `chair`, `rev1`, `rev2` — stock `ghcr.io/openabdev/openab` Claude pods, each
  pointed at `…/bot-config/<name>` and depending on the plane.

## Variables
- `PUBLIC_DOMAIN` — domain for the plane's API.
- `CLAUDE_CODE_OAUTH_TOKEN` — agent auth for every pod (`claude setup-token`).
- `GH_TOKEN` (optional) — fine-grained PAT so the **chair** can comment/label/approve
  PRs. Leave blank to deliberate without PR write-back.

## Deploy
```
npx zeabur@latest template deploy -i=false -f zeabur-template.yaml \
  --project-id <PROJECT_ID> \
  --var PUBLIC_DOMAIN=my-council \
  --var CLAUDE_CODE_OAUTH_TOKEN=<token> \
  --var GH_TOKEN=<pat>
```

## Run a council
The plane's API key is the auto-generated `OABCP_API_KEY` on the control-plane
service. Then:
```
PLANE=https://my-council.zeabur.app KEY=<OABCP_API_KEY> \
  scripts/open-council.sh owner/repo#123      # or a quoted free-text task
```
The script prints a stream URL to follow by hand, or pass `--watch` (or `FOLLOW=1`)
to follow inline and print the verdict on close. Council size is the `ROSTER` env
(default `["chair","rev1","rev2"]`); `QUORUM`/`MODE` override the derived defaults.
Pass `--preset quick|standard|full` (PR path only) to assign review angles to the
reviewers — angles are round-robined onto the roster, extra reviewers sit out,
quorum = participating reviewers. Without a preset, reviewers are generic
all-rounders (today's behaviour). Needs `node` (not python3/jq) — same runtime as
CI and the dev sandbox.

## Auto-review every PR (GitHub App webhook)

The turnkey "open a PR → it gets reviewed" path. Create a GitHub App, install it on
the repo, and put its credentials on the control-plane service:

| Variable | Purpose |
|----------|---------|
| `GITHUB_APP_ID` · `GITHUB_APP_INSTALLATION_ID` · `GITHUB_APP_PRIVATE_KEY` | App identity — the plane mints per-role installation tokens (chair `pull_requests:write`, reviewers read-only); a pod fetches its scoped token via `/v1/sessions/:id/github-token` (the App key never leaves the plane) |
| `GITHUB_WEBHOOK_SECRET` | HMAC secret for the webhook endpoint (fail-closed if unset) |
| `OABCP_COUNCIL_ROSTER` | Roster the webhook convenes (default `chair,rev1,rev2`; `[0]` is chair) |
| `OABCP_COUNCIL_PRESET` | Optional review preset for webhook councils: `quick`/`standard`/`full` (angle assignment) |

Point the App's webhook at `POST <plane>/api/v1/github_webhooks` (subscribe to pull
requests + issue comments). A PR `opened`/`reopened`/`ready_for_review`, or a `/review`
comment on a PR, then **convenes a real council** — `src/council.rs:convene_for_pr`
reads the PR diff and posts the same trigger as the CLI path (shared
`scripts/pr-review-trigger.tmpl`) — and the chair posts one verdict comment back.
Re-deliveries are idempotent (one open council per PR).

> **Status:** the webhook convenes a real council (v0.1.6) with preset/angle assignment
> (v0.1.7). Two gaps to close before production: **no per-repo allowlist and no
> permission gate on `/review`** — any signed webhook can open a session. The chair
> still posts the verdict from its pod's `GH_TOKEN`; switching the *post* path to the
> App bot identity is a separate parity step. Setup + end-to-end validation:
> [github-app-validation.md](github-app-validation.md).

## Manual / fallback review (GitHub Action)

`.github/workflows/council-review.yml` is the **PAT-track manual path** — use it to
re-review a PR on demand (Actions → *Run workflow*) or as a fallback if the webhook is
down. It is `workflow_dispatch`-only (the auto `pull_request` trigger moved to the
webhook above, so they don't double-convene). Set two repo secrets:

```sh
gh secret set COUNCIL_PLANE --body "https://my-council.zeabur.app"
gh secret set COUNCIL_KEY   --body "<OABCP_API_KEY>"
```

It convenes via the plane's REST API and exits (fire-and-forget); the chair posts the
verdict asynchronously from its pod's `GH_TOKEN`. If a review never appears: check the
**Action run log** for convene errors, then the **plane / chair logs** — a session that
never reaches quorum is force-closed by the 900s watchdog. If the council *runs* but no
comment lands, verify `GH_TOKEN` has `pull_requests: write` + `contents: read`.

## Image hosting
The template references `docker.io/canyu/openab-control-plane:<version>` (public).
Images build + push automatically via `.github/workflows/release.yml` on a `v*`
git tag — `git tag v0.1.1 && git push origin v0.1.1` publishes `:0.1.1` and
`:latest`. Bump the template's `image:` tag to match the release you want.
