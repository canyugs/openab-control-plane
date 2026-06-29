# OpenAB Review Council

A self-hosted, **CodeRabbit-style PR-review council**: one control plane + 3 stock
OpenAB Claude pods (1 chair + 2 reviewers) deliberate on a PR and post one verdict
comment. Runs on **your own Claude subscription** (`claude setup-token`); no per-bot
setup.

Pick **one** trigger track per target repo:

| | Track 1 — **PAT + copied Action** | Track 2 — **GitHub App webhook** |
|---|---|---|
| Setup effort | minimal (~5 min) | more (create + install an App) |
| GitHub App needed? | **no** | yes |
| Trigger | copied GitHub Action in the target repo | App/repo webhook (no per-repo file) |
| Verdict author | **your account** (PAT) | **`zeabur-council[bot]`** (clean) |
| Best for | external repos trying the service | this dogfood repo, teams, clean attribution |

---

## Track 1 — PAT + copied Action (external quickstart)

1. **Deploy template code `Z7TQIR`** (source: `zeabur-template.pat.yaml`) with:
   - `PUBLIC_DOMAIN` — e.g. `my-council` → `my-council.zeabur.app`
   - `CLAUDE_CODE_OAUTH_TOKEN` — from `claude setup-token`
   - `GH_TOKEN` — a fine-grained PAT (`pull_requests: write`, `contents: read`)
   ```sh
   npx zeabur@latest template deploy -c Z7TQIR \
     --project-id <PROJECT_ID> \
     --var PUBLIC_DOMAIN=my-council \
     --var CLAUDE_CODE_OAUTH_TOKEN=<OAUTH_TOKEN> \
     --var GH_TOKEN=<PAT>
   ```
2. **Add the copied Action** — copy [`examples/pr-review.yml`](examples/pr-review.yml) into the
   target repo's `.github/workflows/`, and set two repo secrets:
   ```sh
   gh secret set COUNCIL_PLANE --body "https://my-council.zeabur.app"
   gh secret set COUNCIL_KEY   --body "<PASSWORD from the control-plane service>"
   ```

Open a PR → reviewed automatically; the chair posts the verdict **as you** (the PAT
owner). Done.

The control-plane repository itself does **not** install this workflow for dogfood; it
uses Track 2 so the only automatic trigger is the GitHub App webhook.

---

## Track 2 — GitHub App webhook (dogfood/default)

For verdicts authored by `zeabur-council[bot]` instead of your account, and a webhook
trigger with no per-repo file:

1. **Create a GitHub App** (perms `Pull requests: write`, `Contents: read`; events:
   Pull requests + Issue comments), install it on the repo, generate a private key.
2. **Deploy template code `1E1Y97`** (source: `zeabur-template.app.yaml`) with `PUBLIC_DOMAIN`,
   `CLAUDE_CODE_OAUTH_TOKEN`, and a generated `GITHUB_WEBHOOK_SECRET`.
   ```sh
   SECRET=$(openssl rand -hex 32)
   npx zeabur@latest template deploy -c 1E1Y97 \
     --project-id <PROJECT_ID> \
     --var PUBLIC_DOMAIN=my-council \
     --var CLAUDE_CODE_OAUTH_TOKEN=<OAUTH_TOKEN> \
     --var GITHUB_WEBHOOK_SECRET=$SECRET
   ```
3. **Wire the webhook trigger** — point the App webhook at
   `POST <plane>/api/v1/github_webhooks` and use the same
   `GITHUB_WEBHOOK_SECRET`.
4. **Wire the chair's App posting identity** — put the App key + token-minter on the
   chair volume and restart the chair.

For private repos, reviewer pods also need GitHub read access to self-fetch the PR
diff. Public repos work anonymously; private repos should add read-only reviewer
credentials or use the separate per-role App token path.

Step-by-step: [docs/deploy.md](docs/deploy.md) §2–§3 +
[docs/github-app-validation.md](docs/github-app-validation.md).

---

> Use **one** automatic trigger per repo. Track 1 installs a workflow in the target
> repo; Track 2 configures a webhook on the App/repo. Installing both means one PR
> event convenes two councils. On demand, either track can also run
> `scripts/open-council.sh owner/repo#N --watch`.

## Knobs (both tracks)
- **Review depth** — default **lite** (1 angle; fast/cheap for small PRs). Add a
  `review:quick` / `review:standard` / `review:full` **label** to a PR for 3 / 5 / 7
  angles, or set `OABCP_COUNCIL_PRESET` on the plane to change the global default.

Full guide: [docs/deploy.md](docs/deploy.md) · [README](README.md).
