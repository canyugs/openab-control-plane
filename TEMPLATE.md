# OpenAB Review Council

A self-hosted, **CodeRabbit-style PR-review council**: one control plane + 3 stock
OpenAB Claude pods (1 chair + 2 reviewers) deliberate on a PR and post one verdict
comment. Runs on **your own Claude subscription** (`claude setup-token`); no per-bot
setup.

Pick one of two install tracks:

| | Track 1 — **PAT + Action** | Track 2 — **GitHub App** |
|---|---|---|
| Setup effort | minimal (~5 min) | more (create + install an App) |
| GitHub App needed? | **no** | yes |
| Trigger | a GitHub Action in your repo | the App's webhook (no per-repo file) |
| Verdict author | **your account** (PAT) | **`zeabur-council[bot]`** (clean) |
| Best for | trying it / personal repos | teams / clean attribution |

---

## Track 1 — PAT + Action (quickest)

1. **Deploy this template** with:
   - `PUBLIC_DOMAIN` — e.g. `my-council` → `my-council.zeabur.app`
   - `CLAUDE_CODE_OAUTH_TOKEN` — from `claude setup-token`
   - `GH_TOKEN` — a fine-grained PAT (`pull_requests: write`, `contents: read`)
2. **Add the Action** — copy [`examples/pr-review.yml`](examples/pr-review.yml) into the
   target repo's `.github/workflows/`, and set two repo secrets:
   ```sh
   gh secret set COUNCIL_PLANE --body "https://my-council.zeabur.app"
   gh secret set COUNCIL_KEY   --body "<PASSWORD from the control-plane service>"
   ```

Open a PR → reviewed automatically; the chair posts the verdict **as you** (the PAT
owner). Done.

---

## Track 2 — GitHub App (clean `[bot]` identity)

For verdicts authored by `zeabur-council[bot]` instead of your account, and a webhook
trigger with no per-repo file:

1. **Create a GitHub App** (perms `Pull requests: write`, `Contents: read`; events:
   Pull requests + Issue comments), install it on the repo, generate a private key.
2. **Deploy this template** (leave `GH_TOKEN` blank).
3. **Wire identity + webhook** — put the App key on the chair's volume + remove
   `GH_TOKEN` (pod-local App auth), set `GITHUB_WEBHOOK_SECRET` on the plane, and point
   the App's webhook at `POST <plane>/api/v1/github_webhooks`.

Step-by-step: [docs/deploy.md](docs/deploy.md) §3 + [docs/github-app-validation.md](docs/github-app-validation.md).

---

> Use **one** track per repo (both auto-triggering = a PR convenes two councils). On
> demand, either track can also run `scripts/open-council.sh owner/repo#N --watch`.

## Knobs (both tracks)
- **Review depth** — default **lite** (1 angle; fast/cheap for small PRs). Add a
  `review:quick` / `review:standard` / `review:full` **label** to a PR for 3 / 5 / 7
  angles, or set `OABCP_COUNCIL_PRESET` on the plane to change the global default.

Full guide: [docs/deploy.md](docs/deploy.md) · [README](README.md).
