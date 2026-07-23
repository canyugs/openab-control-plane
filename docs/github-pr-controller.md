# GitHub PR controller

The GitHub PR controller is an independently deployable product adapter. It
owns GitHub webhook authentication, delivery deduplication, repository and
author admission, trigger parsing, and `SessionPlan` construction. It does not
link to OCP internals, open OCP's database, or perform GitHub writes. In
`external_canary` mode it can call only the versioned OCP action API with an
installation-scoped token and receive signed provider-neutral runtime events.

The default `plan_only` mode is the P7 shadow state: requests are planned and
stored but never sent to OCP. P8's `external_canary` mode owns raw ingress for
exactly one configured repository and sends its `open_session` plan through the
generic action API. There is no fallback to embedded ingress. Both modes keep
the GitHub write client disabled; compatibility findings and write ownership
remain with OCP until later migration phases.

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
- `GET /readyz` gates ingress on webhook HMAC configuration, product-store
  availability, mode-specific ownership/action/event configuration, and the
  absence of GitHub App credentials.
- `POST /api/v1/github/webhooks` accepts at most 1 MiB and requires
  `x-hub-signature-256`, `x-github-delivery`, and `x-github-event`.
- `POST /api/v1/shadow/compare` accepts a wrapper signed with
  `GITHUB_CONTROLLER_SHADOW_SECRET`. It compares the embedded reference with a
  newly generated controller plan and persists counts only.
- `GET /api/v1/shadow/summary` requires a shadow HMAC over an empty body and
  returns aggregate exact, identity/ownership, and presentation mismatch report
  counts. It returns no payload or prompt text.
- `POST /api/v1/openab/events` accepts signed provider-neutral v1 runtime
  events. The signature covers the exact target, body, timestamp, controller
  id, and event id. Receipts retain only identifiers, hashes, types, and
  timestamps for seven days; raw payloads are not stored.
- `GET /api/v1/canary/summary` requires an observer HMAC over an empty body in
  `x-canary-signature-256`. It exposes aggregate acted, processing, retryable,
  and runtime-event counts for promotion and rollback gates.

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

GitHub's webhook association can hide private organization membership. P7 has
no GitHub permission client, so such events are acknowledged and ignored
fail-closed instead of spending tokens. A later phase can verify them with a
read-only GitHub App client.

An accepted trigger returns `202` with a deterministic `SessionPlan`. The plan
contains the exact generic `open_session` fields plus dedupe/supersede policy,
terminal projection inputs, and proposed GitHub write intents. In `plan_only`
these remain comparison data. In `external_canary`, the controller submits only
the generic `open_session` action and records the action result; proposed
GitHub writes remain data and are not executed by this binary.

The OCP action id is deterministically derived from the GitHub delivery id.
Replaying an accepted delivery therefore creates at most one OCP session. An
action outage returns `503` and marks the delivery immediately retryable using
the same action id. It never invokes the embedded webhook as a fallback.

The six P0 fixtures run through both the embedded planner and the external
planner in one test. Their trigger decision, identity, roster, chair, quorum,
mode, recipient inputs, and prompt bytes must match. The controller template
copies are also byte-pinned to the embedded templates.

## Shadow comparison

The mirror wrapper contains `comparison_id`, `delivery_id`, `event_type`, the
raw synthetic or selected live `payload`, and the normalized `embedded` parity
outcome. A planned outcome contains its snapshot; an ignored outcome contains
the exact decision reason. Use `null` only when the embedded reference is
unavailable, which is always a blocking mismatch. Sign the exact wrapper bytes
as `sha256=<hex>` in `x-shadow-signature-256`.

Identity and ownership mismatches set `promotion_blocked=true`. This class
includes trigger identity/fingerprint, roster, chair, quorum, mode, recipient
inputs, dedupe/supersede semantics, terminal projection, and proposed write
ownership. Prompt-only drift is classified as presentation and must be reviewed
explicitly, but does not automatically satisfy or waive the identity budget.

Comparison IDs are idempotent and bound to the wrapper SHA-256. Replaying the
same bytes returns the same report; reusing an ID with different bytes returns
`409`. Reports retain only repository and mismatch counts for seven days, not
raw payload, prompt, or comment content. See the
[shadow runbook](github-controller-shadow-runbook.md) before mirroring a live
repository.

Use `scripts/github-controller-shadow.sh compare` to build and sign the wrapper
without placing the secret in a process argument. Use its `summary` command to
read the authenticated aggregate gate.

## Configuration

| Variable | Default | Description |
|----------|---------|-------------|
| `GITHUB_CONTROLLER_ADDR` | `0.0.0.0:8091` | Listen address |
| `GITHUB_CONTROLLER_DB` | `github-controller.db` | Controller-owned SQLite database |
| `GITHUB_CONTROLLER_MODE` | `plan_only` | `plan_only` or `external_canary` |
| `GITHUB_CONTROLLER_WEBHOOK_SECRET` | _(missing)_ | GitHub webhook HMAC secret; missing is not-ready and fail-closed |
| `GITHUB_CONTROLLER_SHADOW_SECRET` | _(disabled)_ | HMAC secret for trusted shadow comparison wrappers; not an OCP action credential |
| `GITHUB_CONTROLLER_OBSERVER_SECRET` | _(disabled)_ | Separate HMAC secret for the aggregate canary summary; required in `external_canary` |
| `GITHUB_CONTROLLER_CANARY_REPOSITORY` | _(disabled)_ | Exact `owner/repo` whose raw ingress is owned in `external_canary` |
| `GITHUB_CONTROLLER_ALLOWED_REPOS` | _(allow all)_ | Comma-separated `owner/repo` allowlist |
| `GITHUB_CONTROLLER_BOT_HANDLE` | _(none)_ | Bot handle without `@`, used for mention commands |
| `GITHUB_CONTROLLER_ROSTER` | `chair,rev1,rev2` | Planned council roster; first entry is chair |
| `GITHUB_CONTROLLER_COUNCIL_PRESET` | `lite` | Default `lite`, `quick`, `standard`, or `full` plan preset; PR label wins |
| `GITHUB_CONTROLLER_REVIEW_MODE` | `approve` | Proposed write parity: `status`, `approve`, or `enforce` |
| `GITHUB_CONTROLLER_OCP_URL` | _(disabled)_ | HTTPS OCP origin; required in `external_canary` |
| `GITHUB_CONTROLLER_OCP_ACTION_TOKEN` | _(disabled)_ | Installation token granted only `open_session` for the exact canary scope |
| `GITHUB_CONTROLLER_OCP_SCOPE` | _(disabled)_ | Exact controller scope sent with every action |
| `GITHUB_CONTROLLER_ID` | _(disabled)_ | Installed controller id; must match signed runtime events |
| `GITHUB_CONTROLLER_EVENT_SIGNING_SECRET` | _(disabled)_ | Base64url per-controller event secret issued by OCP; minimum 32 decoded bytes |
| `GITHUB_CONTROLLER_GITHUB_APP_ID` | _(must be absent)_ | Future GitHub App client configuration; setting any App credential makes readiness fail |
| `GITHUB_CONTROLLER_GITHUB_APP_INSTALLATION_ID` | _(disabled)_ | Future GitHub App installation |
| `GITHUB_CONTROLLER_GITHUB_APP_PRIVATE_KEY` | _(must be absent)_ | Future GitHub App key; forbidden in the P7 shadow deployment |

The controller deliberately ignores all `OABCP_*` variables. Run OCP and this
controller with separate databases, environment groups, images, and health
checks. Follow the [external canary runbook](github-controller-canary-runbook.md)
before changing raw webhook ownership.
