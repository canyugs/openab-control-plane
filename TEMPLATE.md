# OpenAB Review Council

A multi-agent **PR-review council** for your repos — a CodeRabbit-style reviewer you
self-host. One control plane + 3 stock OpenAB Claude pods (1 chair + 2 reviewers)
deliberate on a PR and post one verdict comment. Runs on **your own Claude
subscription** (`claude setup-token`); no per-bot setup.

## Deploy (this template)

Set three variables:

| Variable | What |
|---|---|
| **Control plane domain** (`PUBLIC_DOMAIN`) | public URL for the plane, e.g. `my-council` → `my-council.zeabur.app` |
| **Claude Code OAuth Token** (`CLAUDE_CODE_OAUTH_TOKEN`) | from `claude setup-token` — your own subscription quota |
| **GitHub Token** (`GH_TOKEN`, optional) | fine-grained PAT (`pull_requests: write`, `contents: read`) so the chair can post verdicts. Leave blank to deliberate without posting. |

It comes up as **1 control-plane + 3 bots** (chair / rev1 / rev2). The plane's API key
is the auto-generated **`PASSWORD`** on the *control-plane* service (Variables tab).

## Get reviews — pick ONE auto-trigger

### A. GitHub Action (easiest — ~2 min)
Copy [`examples/pr-review.yml`](examples/pr-review.yml) into the target repo's
`.github/workflows/`, and set two repo secrets:
```sh
gh secret set COUNCIL_PLANE --body "https://my-council.zeabur.app"
gh secret set COUNCIL_KEY   --body "<PASSWORD from control-plane>"
```
Open a PR → it's reviewed automatically; the chair posts one verdict comment.

### B. GitHub webhook (no per-repo file)
Set `GITHUB_WEBHOOK_SECRET` on the control-plane and point a GitHub App / repo webhook
at `POST <plane>/api/v1/github_webhooks`. See [docs/deploy.md](docs/deploy.md).

> Use **A or B**, not both on one repo (a PR would convene two councils). Either way,
> on demand you can also run `scripts/open-council.sh owner/repo#N --watch`.

## Knobs
- **Review depth** — default **lite** (1 angle, fast, cheap for small PRs). Add a
  `review:quick` / `review:standard` / `review:full` **label** to a PR for a deeper
  review (1 / 3 / 5 / 7 angles), or set `OABCP_COUNCIL_PRESET` on the plane for a
  different global default.
- **Clean bot identity** — by default the chair posts under your PAT. To post as a
  GitHub App (`zeabur-council[bot]`), see the App-identity upgrade in
  [docs/deploy.md](docs/deploy.md).

Full guide: [docs/deploy.md](docs/deploy.md) · [README](README.md).
