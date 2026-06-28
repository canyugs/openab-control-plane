# OpenAB Review Council — deploy & install

One-command deploy of a multi-agent PR-review council: a control plane plus stock
OpenAB Claude pods (1 chair + 2 reviewers) that deliberate and post a verdict.

There are two install tiers:

- **Quick start (PAT)** — deploy + review in a few minutes. The chair posts the
  verdict using a personal access token, so comments are authored by *your* account.
- **Upgrade to App identity** — the chair posts as a GitHub App
  (`zeabur-council[bot]`, clean attribution). A few extra pod-local steps.

## What deploys
- `control-plane` — this repo's image, REST/SSE on a public domain, `/ws` gateway
  internally, SQLite on a `/data` volume. Seeds a fixed roster at boot
  (`OABCP_BOTS=chair:chair,rev1:reviewer,rev2:reviewer`) — no manual bot registration.
- `chair`, `rev1`, `rev2` — stock `ghcr.io/openabdev/openab` Claude pods, each pointed
  at `…/bot-config/<name>` and depending on the plane. `chair` also gets a `/home/node`
  volume (for the optional App-identity upgrade).

## Variables
- `PUBLIC_DOMAIN` — domain for the plane's API.
- `CLAUDE_CODE_OAUTH_TOKEN` — agent auth for every pod (`claude setup-token`).
- `GH_TOKEN` (optional) — fine-grained PAT (`pull_requests: write`, `contents: read`)
  so the **chair** can post verdicts. This is the **quick-start** write path; leave it
  blank to deliberate without PR write-back, or drop it later when upgrading to App
  identity.

---

## 1. Quick start (PAT)

**Deploy:**
```sh
npx zeabur@latest template deploy -i=false -f zeabur-template.yaml \
  --project-id <PROJECT_ID> \
  --var PUBLIC_DOMAIN=my-council \
  --var CLAUDE_CODE_OAUTH_TOKEN=<token> \
  --var GH_TOKEN=<pat>
```
The plane comes up at `https://my-council.zeabur.app`. Its API key is the
auto-generated `OABCP_API_KEY` (the `PASSWORD` var) on the **control-plane** service —
copy it from the dashboard's *Variables* tab.

**Review a PR on demand** (needs `node`):
```sh
PLANE=https://my-council.zeabur.app KEY=<OABCP_API_KEY> \
  scripts/open-council.sh owner/repo#123 --watch
```
The chair posts a single verdict comment (authored by the PAT owner); `--watch`
streams progress and prints the verdict on close. `--preset lite|quick|standard|full`
assigns review angles to the reviewers (lite=1 → full=7); **without `--preset`,
`open-council.sh` runs generic all-rounder reviewers (no angle split)**. (The `lite`
default applies to the webhook path — `OABCP_COUNCIL_PRESET`/code default — not this
script.)

That's the whole quick path: **deploy → run a review → verdict on the PR.**

---

## 2. Auto-review every PR (trigger paths)

**Trigger and identity are orthogonal.** The *trigger* (below) is how a review is
convened; the *identity* the chair posts under (PAT or App bot) is the chair pod's
`gh` auth (§1 = PAT, §3 = App) and is independent of the trigger.

| Trigger (pick one for auto) | How | Setup |
|------|-----|-------|
| **GitHub Action** (easiest) | drop `examples/pr-review.yml` into the repo → PR open/update fires it | copy 1 file + 2 secrets |
| **GitHub App / repo webhook** | PR event or `/review` comment hits the plane directly | create an App/webhook + secret |
| `scripts/open-council.sh` | manual, on demand (terminal / CI) | none |

> **Use the Action *or* the webhook for auto — not both on one repo, or a PR convenes
> two councils.** Both call the same convene (pointer trigger, bots self-fetch, ADR 004).

**Set up the GitHub Action (easiest):** copy [`examples/pr-review.yml`](../examples/pr-review.yml)
to the target repo's `.github/workflows/`, and set two repo secrets:
```sh
gh secret set COUNCIL_PLANE --body "https://my-council.zeabur.app"
gh secret set COUNCIL_KEY   --body "<OABCP_API_KEY>"
```
On a PR it POSTs `<plane>/v1/review {repo, pr}` — the plane convenes a council and the
chair posts the verdict. (Any CI / script can hit `/v1/review` the same way.)

**Set up the webhook (alternative):**
1. On the **control-plane** service set `GITHUB_WEBHOOK_SECRET` (HMAC secret; the
   endpoint is **fail-closed** — unset ⇒ every webhook is rejected).
2. Optionally set `OABCP_COUNCIL_PRESET` (`lite`/`quick`/`standard`/`full`, default
   `lite`) as the global default, and `OABCP_COUNCIL_ROSTER` (default `chair,rev1,rev2`).
3. Point a webhook at `POST <plane>/api/v1/github_webhooks` (content-type JSON, the
   same secret), subscribed to **Pull requests** + **Issue comments**. Either a GitHub
   App webhook (required if you want App-identity posting) or a plain repo webhook
   (fine for PAT posting).

On a trigger the plane convenes a council and posts a **pointer** trigger (PR ref +
optional angle assignment, *not* the diff — the plane makes **zero GitHub calls**, ADR
004); reviewers **self-fetch** (`gh pr diff`); the chair posts one verdict with its own
`gh`. Re-deliveries are idempotent (one open council per PR).

**Per-PR review depth:** add a `review:<preset>` label (`review:lite` … `review:full`)
to a PR — it overrides the global default for that PR (read from the webhook payload, no
GitHub call). Precedence: label > `OABCP_COUNCIL_PRESET` > `lite`.

**Conversational follow-up (`@mention` / `/ask`, ADR 006):** a PR comment that is
`/ask <question>` — or `@`s the bot when `OABCP_BOT_HANDLE` is set (e.g.
`@zeabur-council why is this a P1?`) — convenes a **solo** session that answers as a
**new** PR comment (separate from the review verdict). The bot self-fetches the PR +
thread (zero plane GitHub calls); multi-turn just re-asks. Only **write-ish** commenters
(`author_association` OWNER/MEMBER/COLLABORATOR) can ask — it's on-demand token spend.

**Restricting who/what can trigger:** set `OABCP_ALLOWED_REPOS` (comma-separated
`owner/repo`; unset = allow all) to ignore webhooks from any other repo. Note `/ask`/
`@mention` is additionally permission-gated as above; a plain `/review` comment is not,
so anyone who can comment can convene one review council (bounded — one council per PR).

---

## 3. Upgrade to App identity (chair posts as `zeabur-council[bot]`)

By default the chair posts with its `GH_TOKEN` PAT. To post as a GitHub App instead —
clean `[bot]` attribution, no broad PAT — give the **chair pod** its own App credential.
This is **pod-local** (the plane never mints or holds the posting token): a chair-only
`[hooks.pre_boot]` (served automatically by `bot_config`) mints an installation token
and `gh auth login`s as the App, refreshing before the 1-hour expiry. Mirrors
`multi-agent-review-ops/github-apps` (same pre_boot + `gh auth login --with-token` +
refresher recipe).

**Prereqs:** a GitHub App (perms `Pull requests: write`, `Contents: read`), installed on
the target repo(s); note its **App ID**, **installation ID**, and a generated **private
key** (`.pem`).

**Steps** (the chair already has a `/home/node` volume from the template; for an existing
service without one, mount a volume at `/home/node` first — dashboard or the
`mountVolume` API):

1. Put the key + token-minter on the chair's volume (one-time; persists). Edit
   `scripts/get-gh-app-token.sh` to set your `APP_ID` / `INSTALLATION_ID`, then stream
   both files in and lock down ownership (the pre_boot hook runs as `node` and can't read
   a root-owned key):
   ```sh
   CHAIR=<chair-service-id>   # use explicit /home/node (the pod's HOME) — `service exec` may run as root, so $HOME could be /root
   npx zeabur@latest service exec --id $CHAIR -- sh -c 'cat > /home/node/.github-app.pem'      < path/to/app.pem
   npx zeabur@latest service exec --id $CHAIR -- sh -c 'mkdir -p /home/node/bin; cat > /home/node/bin/get-gh-app-token.sh' < scripts/get-gh-app-token.sh
   npx zeabur@latest service exec --id $CHAIR -- sh -c 'chmod 600 /home/node/.github-app.pem; chmod +x /home/node/bin/get-gh-app-token.sh; chown node:node /home/node/.github-app.pem /home/node/bin/get-gh-app-token.sh'
   ```
2. **Remove `GH_TOKEN`** from the chair service — `gh` prefers the env token over the
   App auth, so it must go:
   ```sh
   npx zeabur@latest variable delete --id $CHAIR --delete-keys GH_TOKEN -y -i=false
   ```
3. **Restart the chair** (variable changes don't auto-restart the pod):
   ```sh
   npx zeabur@latest service restart --id $CHAIR -y -i=false
   ```

**Verify:** in the chair pod, `HOME=/home/node gh auth status` shows
`Logged in … account zeabur-council[bot]`; trigger a `/review` and the verdict is
authored by the App. Reviewers don't write to the PR, so they keep self-fetching with
their own `gh` and need no change.

> Validation/runbook: [github-app-validation.md](github-app-validation.md). The plane's
> `/v1/sessions/:id/github-token` endpoint is a separate north/operator capability and is
> **not** used by this posting path (ADR 004 — identity is pod-local).

---

## Manual / fallback review (GitHub Action)
`.github/workflows/council-review.yml` is the PAT-track manual path — Actions → *Run
workflow*, or a fallback if the webhook is down. `workflow_dispatch`-only (the auto
`pull_request` trigger moved to the webhook so they don't double-convene). Set:
```sh
gh secret set COUNCIL_PLANE --body "https://my-council.zeabur.app"
gh secret set COUNCIL_KEY   --body "<OABCP_API_KEY>"
```
It convenes via REST and exits; the chair posts asynchronously. No review? Check the
Action run log, then plane/chair logs — a session that never reaches quorum is
force-closed by the 900s watchdog.

## Image hosting
The template references `docker.io/canyu/openab-control-plane:<version>` (public).
Images build + push via `.github/workflows/release.yml` on a `v*` tag
(`git tag v0.1.1 && git push origin v0.1.1` → `:0.1.1` + `:latest`). Bump the
template's `image:` tag to the release you want.
