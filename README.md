# openab-control-plane

OpenAB Control Plane is a gateway-native runtime for coordinating multiple stock
OpenAB pods. PR review is the first product profile on top of it.

```text
                    GitHub
     pull_request / issue_comment webhooks
                      |
                      v
          github-pr-controller  ----- the ONLY GitHub writer:
          webhook auth, dedupe,       round comments, verdict,
          admission, SessionPlan,     openab/council status,
          durable write outbox        formal PR review
                      |
        versioned action API (installation token)
        signed runtime events back
                      v
North API / SSE  <->  OpenAB Control Plane  <->  SQLite or Postgres
(CLI, operators)      - sessions / roster / fanout   (ADR 033)
                      - coordinator policy, quorum
                      - findings ledger, waivers
                      - durable delivery, watchdog
                      |
             gateway /ws (per-bot OCP tokens)
                      v
        stock OpenAB pods (config via configUrl or mount)
          chair, rev* -- read-only PR context; NO GitHub
                         write credentials (ADR 031)
```

The plane does not run an LLM and does not touch GitHub. Bots deliberate; the
controller writes. The plane's job is to make the session deterministic: who is
in the room, who is prompted, when quorum is reached, when the chair may close,
and how a stuck session terminates. (The published quickstart templates still
run the older embedded profile where the plane receives webhooks directly and
the chair posts from its pod — see Deploy below.)

## Current PR Review Flow

The production path is controller-driven (ADR 031, live since 2026-07-31):

1. GitHub sends `pull_request` / `issue_comment` webhooks to the
   **github-pr-controller**, which authenticates, dedupes, applies
   repository/author admission, and builds a `SessionPlan`.
2. The controller calls the plane's action API to open a `council` session
   (chair + reviewers). The controller then posts the round's
   **"Review Council started"** comment (round number + PR baseline) — it
   stays in the thread for the round's lifetime.
3. The trigger is a PR pointer, not an inlined diff. Bots self-fetch PR
   context with the read-only tools their pods are given.
4. Reviewers post findings and signal done with `[done]` / `🆗`; the plane
   relays each settled review to the chair.
5. At reviewer quorum the plane prompts the chair to synthesize. Active
   operator waivers (ADR 035) were injected into the chair's opening input;
   matching findings land in a `Waived` row instead of blocking.
6. The chair's final message carries the human report, a machine findings
   block (ADR 020), and a `[[verdict:…]]` trailer. A synthesis turn without a
   parseable verdict re-queues (bounded, with chair-pool rotation) instead of
   closing verdict-less.
7. The plane closes the session, records the findings ledger and waiver
   counters, and emits a signed terminal event. The controller turns it into
   the GitHub writes: the **verdict as its own comment**, the
   `openab/council` commit status (pinned to the webhook head sha), and the
   formal PR review. A round that ends without a verdict (superseded /
   timeout) gets its started comment rewritten into a tombstone instead.

A chair `[done]` before reviewer quorum is intentionally ignored, and every
GitHub write goes through the controller's durable outbox with round-marker
reconciliation, so crash replays never double-post.

## Comment Interaction

PR comments are an interaction surface:

| Comment | Result |
|---|---|
| `/review` | Opens or dedupes a full review council for the PR (new started comment; verdict follows as its own comment) |
| `/ask <question>` | Opens a comment-scoped `solo` session; the answer arrives as a new PR comment (no round comment) |
| `@<bot-handle> review <notes>` | Re-runs the council with the notes relayed as author fix notes |
| `@<bot-handle> <question>` | Same as `/ask`, when the bot handle is configured |

Comment commands are accepted only from write-ish GitHub users
(`OWNER`, `MEMBER`, or `COLLABORATOR`). `GITHUB_CONTROLLER_ALLOWED_REPOS`
restricts which repositories the controller will serve.

## Deploy

> **Template status:** the published templates deploy the control plane and
> three pods only. The embedded GitHub ingress they used to rely on was
> removed in v0.1.67 (ADR 031 invariant #9), so a template deploy reviews
> nothing until you also run `github-pr-controller` and point the App at it —
> see [docs/github-pr-controller.md](docs/github-pr-controller.md) and
> [docs/controller-action-api.md](docs/controller-action-api.md). A
> controller-included template is on the roadmap.

The templates deploy one control plane plus three stock OpenAB pods: one chair
and two reviewers. The pod image/config chooses the agent CLI; the control plane
only owns gateway identity, routing, and coordination. Pick one install track per
repository:

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

Switching or mixing CLIs means changing the bot image/config and the pod Secret
carrying that CLI's credential. Current templates mount pod-owned OpenAB config
and steering files directly into the pods; `/bot-config/<id>` remains a legacy
compatibility path. See [ADR 010](docs/adr/010-openab-configurl-boundary.md).

Full install docs:

- [docs/install-pat.md](docs/install-pat.md) for the PAT copied Action path.
- [docs/install-github-app-quickstart.md](docs/install-github-app-quickstart.md) — 一頁 Quick Start（非技術用戶，繁中）.
- [docs/install-github-app.md](docs/install-github-app.md) — 完整 SOP（`scripts/install-github-app.sh`）.
- [docs/controller-action-api.md](docs/controller-action-api.md) — provider-neutral external controller installation and action API.

## Run A Review

Manual review through the north API:

```sh
PLANE=https://my-council.zeabur.app KEY=<OABCP_API_KEY> \
  scripts/open-council.sh owner/repo#123 --watch
```

Automatic review through GitHub:

- Configure `GITHUB_CONTROLLER_WEBHOOK_SECRET`, point the GitHub App at
  `https://<controller-domain>/api/v1/github/webhooks`, and subscribe to Pull
  requests and Issue comments. Reviews run on PR open/reopen/ready-for-review
  and on a write-ish user's `/review` PR comment.
- The CI/PAT track (`examples/pr-review.yml`, `POST /v1/review`) was retired
  with the embedded ingress — the controller has no manual convene endpoint
  yet. A signed webhook is the only trigger.

Review depth is controlled by labels:

| Label | Angles |
|---|---:|
| `review:lite` | 1 |
| `review:quick` | 3 |
| `review:standard` | 5 |
| `review:full` | 7 |

The default is `lite`, unless `GITHUB_CONTROLLER_COUNCIL_PRESET` overrides it.

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
GET  /v1/bots?role=...&provider=...&capability=...&connected=true&enabled=true&health=...
POST /v1/bots/discover
PATCH /v1/bots/:id
POST /v1/sessions
GET  /v1/sessions?trigger_ref=...&state=...&limit=20
GET  /v1/sessions/:id
GET  /v1/sessions/:id/result
GET  /v1/sessions/:id/log
GET  /v1/session-log?trigger_ref=...
GET  /v1/sessions/:id/stream
POST /v1/sessions/:id/messages
POST /v1/sessions/:id/roster
POST /v1/sessions/:id/roster/replace
GET  /v1/council/roster
PUT  /v1/council/roster
POST /v1/council/roster/replace
```

The ADR 020 findings ledger moved to `github-pr-controller`
(`GET /api/v1/review/findings`, signed-observation auth): the controller
records each finding with the repo/PR/head_sha it learned from the webhook,
which the kernel never knows — its own copy had written NULL identity columns
since the controller cutover (SEI-895).

## Core Concepts

| Concept | Meaning |
|---|---|
| Runtime kernel | OCP's session, routing, delivery, state, auth, and liveness machinery |
| Coordinator | Pluggable in-session policy: `council`, `solo`, `pipeline` |
| Control plugin | Product packaging around the runtime: triggers, prompts, tools, secrets, side effects, templates |
| Chair | The synthesizer and only expected PR writer |
| Reviewer | A bot that produces findings and contributes to quorum |
| Watchdog | The timeout fallback that closes stale non-terminal sessions with a `TIMEOUT` verdict and north `timeout` event |

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
| `scripts/dev-run-host-ocp.sh` | local host OCP runner for Docker Desktop bot pods |
| `scripts/dev-deploy-bots.sh` | local Kubernetes OpenAB bot pod deployment |
| `scripts/dev-tunnel-k8s.sh` | local Kubernetes cloudflared tunnel to OCP |
| `scripts/dev-sync-gh-token-secret.sh` | local GitHub token Secret helper for bot pod tests |
| `scripts/dev-sync-gh-app-secret.sh` | local GitHub App key/minter Secret helper for chair tests |

## Docs

- [docs/flow.md](docs/flow.md) for the current PR review and follow-up flow.
- [docs/design.md](docs/design.md) for OCP's ownership boundary.
- [docs/coordinators.md](docs/coordinators.md) for the coordinator seam.
- [docs/config-reference.md](docs/config-reference.md) for environment variables.
- [docs/bot-operations.md](docs/bot-operations.md) for reducing, adding, or
  replacing council bots and providers.
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

## License

MIT. See [LICENSE](LICENSE).
