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
Stream the deliberation/verdict from the URL the script prints.

## Image hosting
The template references `docker.io/canyugs/openab-control-plane:<version>` (public).
Images build + push automatically via `.github/workflows/release.yml` on a `v*`
git tag — `git tag v0.1.1 && git push origin v0.1.1` publishes `:0.1.1` and
`:latest`. Bump the template's `image:` tag to match the release you want.
