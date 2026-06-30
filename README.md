# openab-control-plane

OpenAB Control Plane is a gateway-native runtime for coordinating multiple stock
OpenAB pods. PR review is the first product profile on top of it.

```text
GitHub webhook / north API
        |
        v
OpenAB Control Plane
  - sessions, roster, fanout
  - coordinator policy
  - durable messages/reactions
  - liveness watchdog
        |
        v
OpenAB pods
  - chair
  - reviewers
  - pod-local tools and credentials
```

The plane does not run an LLM and does not post PR comments itself. Bots do that
from their pods. The plane's job is to make the session deterministic: who is in
the room, who is prompted, when quorum is reached, when the chair may close, and
how a stuck session terminates.

## Current PR Review Flow

The dogfood path is GitHub webhook driven:

1. GitHub sends `pull_request` or `issue_comment` to
   `POST /api/v1/github_webhooks`.
2. OCP opens a `review_council` session with `chair`, `rev1`, `rev2`.
3. The trigger is a PR pointer, not an inlined diff. Bots self-fetch the PR.
4. The chair and reviewers are mentioned. The chair posts/updates a short
   "OpenAB Council review started" PR status comment from its pod; reviewers
   produce findings.
5. Reviewer `[done]` / `🆗` counts toward quorum.
6. After reviewer quorum, OCP prompts the chair.
7. The chair updates the same PR comment with the verdict, then sends `[done]`.
8. OCP closes the session. If bots stall, the watchdog force-closes later.

A chair `[done]` before reviewer quorum is intentionally ignored. This prevents
an opening-trigger chair response from closing the session before reviewers or PR
side effects happen.

## Comment Interaction

PR comments are an interaction surface:

| Comment | Result |
|---|---|
| `/review` | Opens or dedupes a full review council for the PR |
| `/ask <question>` | Opens a comment-scoped `solo` session; the chair answers as a new PR comment |
| `@<bot-handle> <question>` | Same as `/ask`, when `OABCP_BOT_HANDLE` is configured |

Comment commands are accepted only from write-ish GitHub users
(`OWNER`, `MEMBER`, or `COLLABORATOR`). `OABCP_ALLOWED_REPOS` can restrict which
repositories the webhook will serve.

## Deploy

The templates deploy one control plane plus three stock OpenAB Claude pods: one
chair and two reviewers. Pick one install track per repository:

| Track | Template | Best for |
|---|---|---|
| PAT + copied Action | `zeabur-template-pat-Z7TQIR.yaml` / code `Z7TQIR` | Fast external quickstart; verdicts are authored by the PAT owner |
| GitHub App webhook | `zeabur-template-app-1E1Y97.yaml` / code `1E1Y97` | Dogfood and team installs; PR events arrive through webhook and the chair can post as the App bot |

PAT quickstart:

```sh
npx zeabur@latest template deploy -c Z7TQIR \
  --project-id <PROJECT_ID> \
  --var PUBLIC_DOMAIN=my-council \
  --var CLAUDE_CODE_OAUTH_TOKEN=<OAUTH_TOKEN> \
  --var GH_TOKEN=<PAT>
```

GitHub App webhook track:

```sh
SECRET=$(openssl rand -hex 32)
npx zeabur@latest template deploy -c 1E1Y97 \
  --project-id <PROJECT_ID> \
  --var PUBLIC_DOMAIN=my-council \
  --var CLAUDE_CODE_OAUTH_TOKEN=<OAUTH_TOKEN> \
  --var GITHUB_WEBHOOK_SECRET=$SECRET
```

When developing unpublished template changes from this repository, use
`-f zeabur-template-pat-Z7TQIR.yaml` or `-f zeabur-template-app-1E1Y97.yaml` instead of `-c`.

Full install docs:

- [docs/install-pat.md](docs/install-pat.md) for the PAT copied Action path.
- [docs/install-github-app.md](docs/install-github-app.md) for the GitHub App webhook path.
- [docs/github-app-validation.md](docs/github-app-validation.md) for App identity validation.

## Run A Review

Manual review through the north API:

```sh
PLANE=https://my-council.zeabur.app KEY=<OABCP_API_KEY> \
  scripts/open-council.sh owner/repo#123 --watch
```

Automatic review through GitHub:

- Use exactly one automatic trigger per repository: webhook or copied Action, not both.
- For the PAT track, copy `examples/pr-review.yml` into the target repo and set
  `COUNCIL_PLANE` / `COUNCIL_KEY` secrets. It runs on PR open/update/reopen/ready,
  a write-ish user's `/review` PR comment, or `workflow_dispatch`.
- For the webhook track, configure `GITHUB_WEBHOOK_SECRET`, point the GitHub App
  or repo webhook at `https://<domain>/api/v1/github_webhooks`, and subscribe to
  Pull requests and Issue comments. It runs on PR open/reopen/ready-for-review and
  a write-ish user's `/review` PR comment.

Review depth is controlled by labels:

| Label | Angles |
|---|---:|
| `review:lite` | 1 |
| `review:quick` | 3 |
| `review:standard` | 5 |
| `review:full` | 7 |

The default is `lite`, unless `OABCP_COUNCIL_PRESET` overrides it.

## Debug A Session

Use the north API before reaching for the database or Zeabur shell.

```sh
curl -H "Authorization: Bearer $KEY" \
  "$PLANE/v1/sessions?trigger_ref=github%3Apr%2Fowner%2Frepo%2343"

curl -H "Authorization: Bearer $KEY" \
  "$PLANE/v1/session-log?trigger_ref=github%3Aask%2Fowner%2Frepo%2343%4012345"
```

`trigger_ref` must be URL-encoded because PR refs contain `#`.

Useful north endpoints:

```text
POST /v1/bots
POST /v1/sessions
GET  /v1/sessions?trigger_ref=...&state=...&limit=20
GET  /v1/sessions/:id
GET  /v1/sessions/:id/log
GET  /v1/session-log?trigger_ref=...
GET  /v1/sessions/:id/stream
POST /v1/sessions/:id/messages
POST /v1/sessions/:id/roster
POST /v1/sessions/:id/roster/replace
GET  /v1/council/roster
PUT  /v1/council/roster
POST /v1/council/roster/replace
POST /v1/review
```

## Core Concepts

| Concept | Meaning |
|---|---|
| Runtime kernel | OCP's session, routing, delivery, state, auth, and liveness machinery |
| Coordinator | Pluggable in-session policy: `council`, `solo`, `pipeline` |
| Control plugin | Product packaging around the runtime: triggers, prompts, tools, secrets, side effects, templates |
| Chair | The synthesizer and only expected PR writer |
| Reviewer | A bot that produces findings and contributes to quorum |
| Watchdog | The timeout fallback that closes stale non-terminal sessions |

See [ADR 007](docs/adr/007-control-plugins-and-oab-father.md) for the
Control Plugin / OAB Father direction, and
[ADR 008](docs/adr/008-external-controller-protocol.md) for the proposed
external controller protocol.

## Source Map

| Path | Role |
|---|---|
| `src/api.rs` | north REST/SSE API and webhook routes |
| `src/orchestrator.rs` | runtime mechanism: fanout, delivery, state transitions |
| `src/coordinator.rs` | coordination policies |
| `src/council.rs` | PR-review trigger construction |
| `src/github_webhook.rs` | GitHub webhook parsing and permission gates |
| `src/store.rs` | SQLite store and domain types |
| `src/ws.rs` | south gateway server for OpenAB pods |
| `scripts/open-council.sh` | manual PR review client |
| `examples/pr-review.yml` | copied Action option for external repos |

## Docs

- [docs/flow.md](docs/flow.md) for the current PR review and follow-up flow.
- [docs/design.md](docs/design.md) for OCP's ownership boundary.
- [docs/coordinators.md](docs/coordinators.md) for the coordinator seam.
- [docs/config-reference.md](docs/config-reference.md) for environment variables.
- [docs/roadmap.md](docs/roadmap.md) for planned work and known gaps.
- [docs/adr/](docs/adr/) for decision records, including
  [ADR 007](docs/adr/007-control-plugins-and-oab-father.md) and
  [ADR 008](docs/adr/008-external-controller-protocol.md).

## Develop

```sh
cargo test
```

The spike tests drive mock bots over the real gateway wire and cover 1/3/5-bot
councils, solo follow-up, pipeline handoff, text `[done]`, and close-path
regressions. GitHub App L3 tests are present but ignored unless run with real App
credentials.

For fast webhook development without deploying to Zeabur, run OCP on Docker
Desktop Kubernetes and expose it through a temporary Cloudflare Tunnel. See
[docs/local-development.md](docs/local-development.md).
