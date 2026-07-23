# GitHub PR controller (plan-only)

The GitHub PR controller is an independently deployable product adapter. It
owns GitHub webhook authentication, delivery deduplication, repository and
author admission, trigger parsing, and `SessionPlan` construction. It does not
link to OCP, open OCP's database, call the controller action API, or perform
GitHub writes.

This is the P6 extraction state. A planned request is durable in the
controller's own SQLite database and returned to the caller, but is not sent to
OCP. Later migration phases add shadow comparison before enabling either
external client.

## Run

```sh
GITHUB_CONTROLLER_WEBHOOK_SECRET=development-secret \
GITHUB_CONTROLLER_ALLOWED_REPOS=owner/repo \
cargo run -p github-pr-controller
```

Build the separate runtime image with:

```sh
docker build -f Dockerfile.github-controller -t github-pr-controller .
```

The container listens on port 8091 and stores delivery records in
`/data/github-controller.db`. Give `/data` a persistent volume.

## Endpoints

- `GET /healthz` is process liveness and always returns the component report.
- `GET /readyz` gates ingress on webhook HMAC configuration and product-store
  availability. OCP and GitHub clients are reported independently and do not
  gate plan-only readiness because they are intentionally disabled.
- `POST /api/v1/github/webhooks` accepts at most 1 MiB and requires
  `x-hub-signature-256`, `x-github-delivery`, and `x-github-event`.

Webhook HMAC covers the exact raw request body. A delivery ID is a durable
idempotency key; replaying the same ID and body returns the stored result,
while reusing an ID with a different body returns `409`. An in-progress replay
returns a retryable `503`; a five-minute-old processing lease is reclaimed after
a crash. Completed delivery records are retained for seven days and pruned
hourly.

## Admission and output

The controller recognizes non-draft PR `opened`, `reopened`,
`ready_for_review`, and `synchronize` events, plus trusted PR comments using
`/review`, `/ask`, or a leading configured bot mention. `OWNER`, `MEMBER`, and
`COLLABORATOR` associations are trusted. The `oab-review` label is the
maintainer opt-in for other PR authors.

GitHub's webhook association can hide private organization membership. P6 has
no GitHub permission client, so such events are acknowledged and ignored
fail-closed instead of spending tokens. A later phase can verify them with a
read-only GitHub App client.

An accepted trigger returns `202` with a deterministic `SessionPlan`. Its
`proposed_writes` is empty by construction. No session or GitHub object is
created.

## Configuration

| Variable | Default | Description |
|----------|---------|-------------|
| `GITHUB_CONTROLLER_ADDR` | `0.0.0.0:8091` | Listen address |
| `GITHUB_CONTROLLER_DB` | `github-controller.db` | Controller-owned SQLite database |
| `GITHUB_CONTROLLER_WEBHOOK_SECRET` | _(missing)_ | GitHub webhook HMAC secret; missing is not-ready and fail-closed |
| `GITHUB_CONTROLLER_ALLOWED_REPOS` | _(allow all)_ | Comma-separated `owner/repo` allowlist |
| `GITHUB_CONTROLLER_BOT_HANDLE` | _(none)_ | Bot handle without `@`, used for mention commands |
| `GITHUB_CONTROLLER_ROSTER` | `chair,rev1,rev2` | Planned council roster; first entry is chair |
| `GITHUB_CONTROLLER_GITHUB_APP_ID` | _(disabled)_ | Future GitHub App client configuration; no writes in P6 |
| `GITHUB_CONTROLLER_GITHUB_APP_INSTALLATION_ID` | _(disabled)_ | Future GitHub App installation |
| `GITHUB_CONTROLLER_GITHUB_APP_PRIVATE_KEY` | _(disabled)_ | Future GitHub App key; avoid setting in P6 shadow deployment |

The controller deliberately ignores all `OABCP_*` variables. Run OCP and this
controller with separate databases, environment groups, images, and health
checks.
