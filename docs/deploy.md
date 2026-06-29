# OpenAB Review Council — deploy & install

One-command deploy of a multi-agent PR-review council: a control plane plus stock
OpenAB Claude pods (1 chair + 2 reviewers) that deliberate and post a verdict.

There are two normal install tracks. Pick **one automatic trigger** for a target repo:

- **PAT + copied Action** — fastest external quickstart. The chair posts with a
  personal access token, so verdicts are authored by *your* account.
- **GitHub App webhook + App identity** — dogfood/default. PR events hit the plane
  directly, and the chair posts as a GitHub App bot (`zeabur-council[bot]`).

Manual reruns use `scripts/open-council.sh` and do not require either auto trigger.

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
- `GH_TOKEN` (PAT template only) — fine-grained PAT (`pull_requests: write`,
  `contents: read`) so the **chair** can post verdicts.
- `GITHUB_WEBHOOK_SECRET` (App template / webhook track only) — HMAC secret set on
  the **control-plane** service. After changing it, restart the control-plane so
  the running process sees the new env.

---

## 1. Quick start (PAT)

**Deploy:**
```sh
npx zeabur@latest template deploy -i=false -c Z7TQIR \
  --project-id <PROJECT_ID> \
  --var PUBLIC_DOMAIN=my-council \
  --var CLAUDE_CODE_OAUTH_TOKEN=<token> \
  --var GH_TOKEN=<pat>
```
Template source lives in `zeabur-template.pat.yaml`; use `-f zeabur-template.pat.yaml`
instead of `-c Z7TQIR` when deploying an unpublished local edit.

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

For this repository's dogfood deployment, the automatic trigger is the **GitHub App
webhook only**. There is no repo-local `.github/workflows/council-review.yml`, and
`COUNCIL_PLANE` / `COUNCIL_KEY` are not part of dogfood. That keeps automatic PR
review on one path:

`pull_request` / `/review` webhook → `POST <plane>/api/v1/github_webhooks` → council

| Trigger (pick one for auto) | How | Setup |
|------|-----|-------|
| **GitHub App / repo webhook** | PR event or `/review` comment hits the plane directly | create an App/webhook + secret |
| **Copied GitHub Action** | drop `examples/pr-review.yml` into an external repo → PR open/update fires it | copy 1 file + 2 secrets |
| `scripts/open-council.sh` | manual, on demand (terminal / CI) | none |

> **Use the copied Action *or* the webhook for auto — not both on one repo, or a PR
> convenes two councils.** Both call the same convene (pointer trigger, bots self-fetch,
> ADR 004).

**Set up the webhook (dogfood/default):**
1. For a fresh install, deploy the App template with a generated HMAC secret:
   ```sh
   SECRET=$(openssl rand -hex 32)
   npx zeabur@latest template deploy -i=false -c 1E1Y97 \
     --project-id <PROJECT_ID> \
     --var PUBLIC_DOMAIN=my-council \
     --var CLAUDE_CODE_OAUTH_TOKEN=<token> \
     --var GITHUB_WEBHOOK_SECRET=$SECRET
   ```
   Template source lives in `zeabur-template.app.yaml`; use
   `-f zeabur-template.app.yaml` instead of `-c 1E1Y97` when deploying an
   unpublished local edit.
   For an existing deployment, set `GITHUB_WEBHOOK_SECRET` on the **control-plane**
   service to that value, then restart the control-plane. The endpoint is
   **fail-closed** — unset ⇒ every webhook is rejected.
2. Optionally set `OABCP_COUNCIL_PRESET` (`lite`/`quick`/`standard`/`full`, default
   `lite`) as the global default, and `OABCP_COUNCIL_ROSTER` (default `chair,rev1,rev2`).
3. Point a webhook at `POST <plane>/api/v1/github_webhooks` (content-type JSON, the
   same secret), subscribed to **Pull requests** + **Issue comments**. Either a GitHub
   App webhook (required if you want App-identity posting) or a plain repo webhook
   (fine for PAT posting).

For the App-bot track, continue to §3 after the webhook is configured. For a PAT-backed
webhook track, `GH_TOKEN` on the chair is enough for write-back.

On a trigger the plane convenes a council and posts a **pointer** trigger (PR ref +
optional angle assignment, *not* the diff — the plane makes **zero GitHub calls**, ADR
004); reviewers **self-fetch** (`gh pr diff`); the chair posts one verdict with its own
`gh`. Public repos can be fetched anonymously; private repos need read access on the
reviewer pods (for example a read-only reviewer credential, or the separate per-role
App token path). Re-deliveries are idempotent (one open council per PR).

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
`owner/repo`; unset = allow all) to ignore webhooks from any other repo. Comment
commands (`/review`, `/ask`, `@mention`) are permission-gated to write-ish GitHub
commenters (`author_association` OWNER/MEMBER/COLLABORATOR). Pull-request lifecycle
events are gated by the webhook signature and optional repo allowlist.

**Set up the copied Action (external/PAT track):** copy
[`examples/pr-review.yml`](../examples/pr-review.yml) to the target repo's
`.github/workflows/`, and set two repo secrets:
```sh
gh secret set COUNCIL_PLANE --body "https://my-council.zeabur.app"
gh secret set COUNCIL_KEY   --body "<OABCP_API_KEY>"
```
On a PR it POSTs `<plane>/v1/review {repo, pr}` — the plane convenes a council and the
chair posts the verdict. This is an install option for repos that do not want to create
a webhook/App, not the dogfood route for this repository. Any trusted CI/script can hit
`/v1/review` the same way for manual or custom automation.

**Manual fallback / troubleshooting:** use `scripts/open-council.sh owner/repo#123
--watch` from a trusted terminal or CI job when the webhook is down or a PR needs a
rerun. It convenes via REST, then the chair posts asynchronously. If a review stalls,
check the plane/chair logs; `OABCP_SESSION_TIMEOUT_SECS` defaults to 900s and the
watchdog force-closes sessions that never reach a normal terminal verdict.

---

## 3. Upgrade to App identity (chair posts as `zeabur-council[bot]`)

By default the chair posts with its `GH_TOKEN` PAT. To post as a GitHub App instead —
clean `[bot]` attribution, no broad PAT — give the **chair pod** its own App credential.
This is **pod-local** (the plane never mints or holds the posting token): a chair-only
`[hooks.pre_boot]` (served automatically by `bot_config`) mints an installation token
and `gh auth login`s as the App, refreshing before the 1-hour expiry. Mirrors
`multi-agent-review-ops/github-apps` (same pre_boot + `gh auth login --with-token` +
refresher recipe).

This pod-local posting path is separate from the control-plane's optional
`GITHUB_APP_*` env vars. You do **not** need `GITHUB_APP_ID`,
`GITHUB_APP_INSTALLATION_ID`, or `GITHUB_APP_PRIVATE_KEY` on the control-plane for the
chair to post as the App; those env vars only enable the separate
`/v1/sessions/:id/github-token` operator capability.

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
2. **Remove `GH_TOKEN`** from the chair service if it was ever set — `gh` prefers the
   env token over the App auth, so it must go:
   ```sh
   npx zeabur@latest variable delete --id $CHAIR --delete-keys GH_TOKEN -y -i=false
   ```
3. **Restart the chair** (variable changes don't auto-restart the pod):
   ```sh
   npx zeabur@latest service restart --id $CHAIR -y -i=false
   ```

**Verify:** in the chair pod, `HOME=/home/node gh auth status` shows
`Logged in … account zeabur-council[bot]`; trigger a `/review` and the verdict is
authored by the App. Reviewers don't write to the PR, but they still need enough
read access to self-fetch private repo diffs.

> Validation/runbook: [github-app-validation.md](github-app-validation.md). The plane's
> `/v1/sessions/:id/github-token` endpoint is a separate north/operator capability and is
> **not** used by this posting path (ADR 004 — identity is pod-local).

---

## Image hosting
The templates reference `docker.io/canyu/openab-control-plane:<version>` (public).
Images build + push via `.github/workflows/release.yml` on a `v*` tag
(`git tag v0.1.1 && git push origin v0.1.1` → `:0.1.1` + `:latest`). Bump the
template `image:` tags to the release you want.
