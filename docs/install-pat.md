# Install With PAT

Use this path for the quickest external-repo setup. A copied GitHub Action in the
target repo triggers OCP, and the chair posts as the owner of your fine-grained
PAT.

## Template

- Marketplace code: `Z7TQIR`
- Source file: [`zeabur-template-pat-Z7TQIR.yaml`](../zeabur-template-pat-Z7TQIR.yaml)

## Deploy

```sh
npx zeabur@latest template deploy -c Z7TQIR \
  --project-id <PROJECT_ID> \
  --var PUBLIC_DOMAIN=my-council \
  --var CLAUDE_CODE_OAUTH_TOKEN=<CLAUDE_CODE_OAUTH_TOKEN> \
  --var GH_TOKEN=<FINE_GRAINED_PAT> \
  --var BOT_TOKEN_CHAIR=$(openssl rand -hex 32) \
  --var BOT_TOKEN_REV1=$(openssl rand -hex 32) \
  --var BOT_TOKEN_REV2=$(openssl rand -hex 32)
```

`BOT_TOKEN_CHAIR`, `BOT_TOKEN_REV1`, and `BOT_TOKEN_REV2` are the per-pod gateway
tokens (ADR 016): the plane stores only their hashes and serves
`token = "${OABCP_BOT_TOKEN}"` in `/bot-config`, so an unauthenticated config
fetch leaks nothing. Use three distinct values. The inline `$(openssl rand …)`
values are persisted as Zeabur service variables (`OABCP_BOT_TOKEN_*` on the
plane, `OABCP_BOT_TOKEN` on each pod), so you can retrieve them later from the
dashboard or CLI — no need to record them separately.

`GH_TOKEN` should be scoped to the target repo:

- `Pull requests`: Read and write
- `Commit statuses`: Read and write (Checks tab "Details" links to the review)
- `Contents`: Read-only

After deploy, wait until `control-plane`, `chair`, `rev1`, and `rev2` are running.

## Target Repo Setup

Copy [`examples/pr-review.yml`](../examples/pr-review.yml) into the target repo:

```text
.github/workflows/pr-review.yml
```

Then set these target repo secrets:

```sh
gh secret set COUNCIL_PLANE --body "https://my-council.zeabur.app"
gh secret set COUNCIL_KEY --body "<OABCP_API_KEY>"
```

`OABCP_API_KEY` is the generated `PASSWORD` value exposed as `OABCP_API_KEY` on the
Zeabur `control-plane` service variables.

## Triggers

- Automatic: PR opened, synchronized, reopened, or marked ready for review.
- Manual: an `OWNER`, `MEMBER`, or `COLLABORATOR` comments `/review` on the PR.
- Manual fallback: run the `Council Review` GitHub Action with `workflow_dispatch`
  and a PR number.

The chair posts an in-progress PR comment first, then updates the same comment
with the final verdict.

## Notes

- Use either this PAT copied Action or the GitHub App webhook path, not both in the
  same target repo.
- The PAT copied Action path supports review triggers only. It does not support
  `/ask` or `@mention` follow-up; those require the GitHub App webhook path.
- For private repos, reviewer pods also need GitHub read access to self-fetch the
  PR diff. Public repos work anonymously.
