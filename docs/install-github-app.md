# Install With GitHub App

Use this path when you want webhook-triggered reviews and verdicts authored by a
GitHub App bot instead of a personal account. No copied workflow file is needed in
each target repo.

## Template

- Marketplace code: `1E1Y97`
- Source file: [`zeabur-template-app-1E1Y97.yaml`](../zeabur-template-app-1E1Y97.yaml)

## Deploy

```sh
SECRET=$(openssl rand -hex 32)

npx zeabur@latest template deploy -c 1E1Y97 \
  --project-id <PROJECT_ID> \
  --var PUBLIC_DOMAIN=my-council \
  --var CLAUDE_CODE_OAUTH_TOKEN=<CLAUDE_CODE_OAUTH_TOKEN> \
  --var GITHUB_WEBHOOK_SECRET=$SECRET \
  --var BOT_TOKEN_CHAIR=$(openssl rand -hex 32) \
  --var BOT_TOKEN_REV1=$(openssl rand -hex 32) \
  --var BOT_TOKEN_REV2=$(openssl rand -hex 32)
```

`BOT_TOKEN_CHAIR`, `BOT_TOKEN_REV1`, and `BOT_TOKEN_REV2` are the per-pod gateway
tokens (ADR 016): the plane stores only their hashes and serves
`token = "${OABCP_BOT_TOKEN}"` in `/bot-config`, so an unauthenticated config
fetch leaks nothing. Use three distinct values.

After deploy, wait until `control-plane`, `chair`, `rev1`, and `rev2` are running.

## GitHub App Setup

Create a GitHub App with:

- Permissions:
  - `Pull requests`: Read and write
  - `Commit statuses`: Read and write (Checks tab "Details" links to the review)
  - `Contents`: Read-only
- Events:
  - Pull requests
  - Issue comments
- Webhook URL:
  - `https://my-council.zeabur.app/api/v1/github_webhooks`
- Webhook secret:
  - the same `$SECRET` used for `GITHUB_WEBHOOK_SECRET`

Install the App on the target repo, generate a private key (`.pem`), and note the
App ID plus installation ID.

## Chair App Identity

Copy [`scripts/get-gh-app-token.sh`](../scripts/get-gh-app-token.sh), then set:

```sh
APP_ID=<APP_ID>
INSTALLATION_ID=<INSTALLATION_ID>
```

Upload the private key and edited token-minter to the `chair` service:

```sh
CHAIR=<chair-service-id>

npx zeabur@latest service exec --id $CHAIR -- sh -c 'cat > /home/node/.github-app.pem' < path/to/app.pem
npx zeabur@latest service exec --id $CHAIR -- sh -c 'mkdir -p /home/node/bin; cat > /home/node/bin/get-gh-app-token.sh' < scripts/get-gh-app-token.sh
npx zeabur@latest service exec --id $CHAIR -- sh -c 'chmod 600 /home/node/.github-app.pem; chmod +x /home/node/bin/get-gh-app-token.sh; chown node:node /home/node/.github-app.pem /home/node/bin/get-gh-app-token.sh'
```

If `GH_TOKEN` was ever set on the chair service, remove it. `gh` prefers the env
token over the App login.

```sh
npx zeabur@latest variable delete --id $CHAIR --delete-keys GH_TOKEN -y -i=false
```

Restart the chair:

```sh
npx zeabur@latest service restart --id $CHAIR -y -i=false
```

Verify the chair is logged in as the App bot:

```sh
npx zeabur@latest service exec --id $CHAIR -- sh -lc 'HOME=/home/node gh auth status'
```

## Triggers

- Automatic: PR opened, reopened, or marked ready for review.
- Manual review: an `OWNER`, `MEMBER`, or `COLLABORATOR` comments `/review` on the
  PR.
- Follow-up: `/ask <question>` opens a solo answer session.
- Follow-up by mention: set `OABCP_BOT_HANDLE` on `control-plane`, then comment
  `@<bot-handle> <question>`.

The chair posts an in-progress PR comment first, then updates the same comment
with the final verdict. Follow-up answers are posted as new PR comments, separate
from the review verdict comment.

## Notes

- Use either this GitHub App webhook path or the PAT copied Action path, not both
  in the same target repo.
- For private repos, reviewer pods also need GitHub read access to self-fetch the
  PR diff. Public repos work anonymously.
- `GITHUB_APP_ID`, `GITHUB_APP_INSTALLATION_ID`, and `GITHUB_APP_PRIVATE_KEY` are
  not required on `control-plane` for this pod-local chair posting path.
