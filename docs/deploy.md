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

## Auto-review every PR (GitHub Action)

For CodeRabbit-style "open a PR → it gets reviewed" with no manual trigger, copy
[`.github/workflows/council-review.yml`](../.github/workflows/council-review.yml)
into the target repo and set two repo secrets:

- `COUNCIL_PLANE` — the plane URL, e.g. `https://my-council.zeabur.app`
- `COUNCIL_KEY` — the control-plane `OABCP_API_KEY`

```sh
gh secret set COUNCIL_PLANE --body "https://my-council.zeabur.app"
gh secret set COUNCIL_KEY   --body "<OABCP_API_KEY>"
```

The workflow runs on `pull_request` (opened / synchronize / reopened) and on manual
`workflow_dispatch`. It convenes a council via the plane's REST API and exits
(fire-and-forget) — the chair posts the verdict back to the PR asynchronously. Fork
PRs are skipped (they can't read the secrets); same-repo PRs and manual dispatch run.

If a review never appears: check the **Action run log** for convene errors (wrong
`COUNCIL_PLANE`/`COUNCIL_KEY`), then the **plane / chair logs** for the session — a
session that never reaches quorum is force-closed by the 900s liveness watchdog. If
the council *runs* but no comment lands, the chair couldn't post — verify `GH_TOKEN`
has `pull_requests: write` + `contents: read`.

> This is the **PAT track**: the chair comments using the `GH_TOKEN` you gave the
> deploy, so verdicts appear under that account. Posting as a distinct **bot
> identity** is the GitHub App track below.

## GitHub App + webhook (identity track)

The Action above is the **PAT track** — simple and turnkey, but verdicts post under
the `GH_TOKEN` account. The **identity track** (shipped v0.1.5) instead has the chair
post as a **GitHub App bot**, with per-role scoped tokens (chair `pull_requests:write`,
reviewers read-only) the plane mints per session, plus a built-in webhook so GitHub
triggers reviews directly — no Action to copy. Set on the control-plane service:

| Variable | Purpose |
|----------|---------|
| `GITHUB_APP_ID` · `GITHUB_APP_INSTALLATION_ID` · `GITHUB_APP_PRIVATE_KEY` | App identity — the plane mints per-role installation tokens; a pod fetches its scoped token via `/v1/sessions/:id/github-token` (the App key never leaves the plane) |
| `GITHUB_WEBHOOK_SECRET` | HMAC secret for the webhook endpoint (fail-closed if unset) |

Point the App's webhook at `POST <plane>/api/v1/github_webhooks` (subscribe to pull
requests + issue comments). A PR `opened`/`reopened`/`ready_for_review`, or a `/review`
comment on a PR, then opens a session.

> **Status (v0.1.5):** the App identity (scoped tokens) and the webhook *ingress*
> (signature-verified, opens a session) are in place, but **roster recruitment + angle
> assignment for webhook-opened sessions is Phase 2** — a webhook today opens a session
> that Phase-2 recruitment will fill, not yet a turnkey reviewer. There is also **no
> per-repo allowlist and no permission gate on `/review`** yet (any signed webhook can
> open a session) — gate these before production. **The Action (PAT) path above is the
> turnkey route today.** Setup + end-to-end validation:
> [github-app-validation.md](github-app-validation.md).

## Image hosting
The template references `docker.io/canyu/openab-control-plane:<version>` (public).
Images build + push automatically via `.github/workflows/release.yml` on a `v*`
git tag — `git tag v0.1.1 && git push origin v0.1.1` publishes `:0.1.1` and
`:latest`. Bump the template's `image:` tag to match the release you want.
