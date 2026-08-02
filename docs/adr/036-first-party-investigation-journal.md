# ADR 036 — First-party investigation journal and causal correlation

Status: proposed · 2026-08-02

Builds on: [ADR 017](017-message-observability-audit-layer.md) (durable runtime
events), [ADR 020](020-review-audit-effectiveness-ledger.md) (runtime audit vs
product evidence), [ADR 031](031-provider-neutral-kernel.md) (kernel/controller
ownership), [ADR 033](033-postgres-backing-store.md) (dual stores), and
[ADR 034](034-mutable-controller-registrations.md) (configuration audit).

This ADR refines ADR 017 after the provider-controller extraction. It supersedes
ADR 017's assumption that one OCP-local event table can describe the whole
product path. It does not change ADR 017's message-storage, data-minimization,
or bounded tool-trace decisions.

## Context

The current system retains most of the domain records needed to investigate a
review:

- the GitHub controller stores webhook delivery id, payload hash, admission
  state, result JSON, session target, review rounds/findings, runtime-event
  receipts, and the GitHub-write outbox;
- OCP stores controller action idempotency, controller/session bindings,
  sessions, roster, messages, reactions, settled result identity, runtime-event
  delivery rows, and review-shaped compatibility state;
- stable ids already cross important boundaries:
  `delivery_id -> action_id -> session_id -> runtime_event_id -> write_id`.

The happy path is therefore reconstructable. The evidence is not yet a durable
investigation trail:

1. **State is not history.** `controller_events` and `github_writes` retain the
   latest delivery state and `last_error`, not every attempt and transition.
2. **North events are transient.** `emit_north` broadcasts lifecycle, quorum,
   roster, connection, and health changes to live SSE subscribers without
   persisting a general history.
3. **Correlation is partly implicit.** A GitHub action id is deterministically
   named `github-delivery-<delivery_id>`, while several other joins require
   parsing `result_json` or traversing more than one table. That is useful
   implementation behavior, not a stable investigation contract.
4. **Provider success receipts are incomplete.** A write row records that a
   comment/review/status is done, but not every provider response identity and
   reconciliation result needed to prove what GitHub accepted.
5. **Retention is inconsistent.** Delivered controller events and controller
   runtime receipts are pruned after a short operational window while session
   and product records live longer. An old incident can retain its conclusion
   but lose the causal path that produced it.
6. **Logs carry unique evidence.** Some retry, connection, and reconciliation
   facts exist only in process logs. A restart, retention policy, or unavailable
   deployment log surface can erase them.
7. **Two trust domains now own the path.** After ADR 031, OCP must not absorb
   GitHub webhook payloads or provider-side-effect schemas merely to produce one
   physical audit table. Conversely, the provider controller must not query or
   write OCP's database.

The investigation target is a first-party record that can answer, after both
processes restart and without consulting GitHub, Discord, pod logs, or an
external telemetry product:

> What authenticated input started this work, what decisions and retries
> followed, which session and messages carried it, what external effects were
> attempted, and which effects are known to have succeeded, failed, or remained
> outcome-unknown?

This is an operational investigation record, not compliance-grade WORM audit
and not distributed tracing.

## Decision

Adopt a shared **investigation event contract** and an append-only local journal
in each owning process. Unify the evidence schema and correlation rules first;
do not centralize the physical store in this phase.

```text
GitHub                         GitHub controller                         OCP
  |                                  |                                   |
  | delivery_id                      |                                   |
  |--------------------------------->| ingress.*                         |
  |                                  | action_id                         |
  |                                  |---------------------------------->| action.*
  |                                  |                                   | session.*
  |                                  |             runtime_event_id      |
  |                                  |<----------------------------------| runtime_event.*
  |                                  | runtime_event.received            |
  |                                  | github_write.*                    |
  |<---------------------------------| provider side effect              |
  |                                  | github_write.succeeded/reconciled |
```

The two journals form one logical investigation stream through stable
correlation fields. They remain separate physical stores because each service
is authoritative for different facts.

### 1. One versioned event envelope

Both processes serialize the same provider-neutral envelope:

```json
{
  "version": 1,
  "event_id": "aud_...",
  "event_key": "github.write.attempted:42:2",
  "occurred_at": 1785600000000,
  "recorded_at": 1785600000012,
  "service": "github-pr-controller",
  "kind": "github.write.attempted",
  "outcome": "pending",
  "caused_by": "aud_...",
  "correlation": {
    "delivery_id": "d-...",
    "controller_id": "github-prod",
    "action_id": "github-delivery-d-...",
    "scope": "tenant:prod/resource:github",
    "trigger_ref": "github:pr/owner/repo#123",
    "trigger_fingerprint": "sha:...",
    "session_id": "ses_...",
    "message_id": null,
    "runtime_event_id": "cev_...",
    "write_id": "42"
  },
  "actor": {
    "kind": "github_user",
    "id": "1234",
    "display": "octocat",
    "association": "MEMBER"
  },
  "target": {
    "kind": "github_pull_request",
    "ref": "owner/repo#123",
    "revision": "head-sha"
  },
  "detail": {
    "operation": "submit_review",
    "attempt": 2
  },
  "error": null
}
```

Contract rules:

- `event_id` is globally unique and immutable.
- `event_key` is a stable semantic idempotency key. A unique constraint per
  service prevents transaction retries from duplicating the same fact. Events
  that intentionally repeat, such as delivery attempts, include the attempt
  number in the key.
- `occurred_at` is the domain time; `recorded_at` is the local durable-write
  time. Neither implies a global total order across processes.
- Each store assigns a monotonic local `seq` at insert. Queries order by
  `(recorded_at, seq)` locally; cross-service readers use causal identifiers
  rather than pretending clocks provide a total order.
- `caused_by` links to the immediate known investigation event when both facts
  are in the same journal. Cross-service causality uses the first-class ids in
  `correlation`.
- `kind` is namespaced by owner (`session.*`, `runtime_event.*`,
  `github.write.*`). `detail` may evolve within the versioned envelope, but
  identifiers and outcome semantics are stable.
- `outcome` uses the bounded vocabulary `pending | accepted | ignored | denied
  | succeeded | failed | retry_scheduled | outcome_unknown | reconciled`.

The serialized type lives in a small dependency-free shared module or crate so
OCP and first-party controllers cannot drift. Provider controllers may add
provider-specific `kind`, `actor`, `target`, and `detail` values; OCP treats
them as controller-owned records and never parses them.

### 2. First-class indexed correlation columns

Each store adds an `audit_events` table. The JSON envelope is returned on the
API, but common correlation fields are real nullable columns, not fields that
must be extracted from `detail_json`:

```text
seq, event_id, event_key, version, occurred_at, recorded_at,
service, kind, outcome, caused_by,
delivery_id, controller_id, action_id, scope,
trigger_ref, trigger_fingerprint, session_id, message_id,
runtime_event_id, write_id,
actor_kind, actor_id, target_kind, target_ref,
detail_json, error_json
```

Minimum indexes cover `session_id`, `delivery_id`, `(controller_id, action_id)`,
`runtime_event_id`, `write_id`, `trigger_ref`, and `(recorded_at, seq)`.
SQLite and Postgres implement the same Store/ProductStore methods and run the
same conformance suite.

The journal is a historical index, not a replacement for domain state:

- message bodies remain in `messages`; a journal event records message id,
  author/audience, content length, and content hash;
- controller-event bodies remain in `controller_events` for delivery;
- GitHub write payloads remain in `github_writes`;
- findings and rounds remain provider-controller product state;
- investigation queries join or link those records by id when authorized.

### 3. Local mutation and journal evidence commit together

When an event describes a durable local state mutation, the state change,
outbox work, and investigation event commit in the same database transaction.

Examples:

- session creation + `session.opened`;
- state CAS + `session.state_changed`;
- message insert + `session.message_recorded` + controller progress outbox;
- roster replacement + `session.roster_replaced`;
- terminal state/result + `session.closed` + terminal-event outbox;
- webhook admission state + `ingress.accepted|ignored|denied`;
- GitHub-write enqueue + `github.write.enqueued`;
- controller registration PATCH + `controller.installation_patched`.

The audit write is not best-effort. If authenticated input would be dispatched
or a durable state mutation would become externally visible but its required
journal row cannot be committed, the operation fails closed and remains
retryable. This does not apply to unauthenticated payload content: authentication
still happens before persistence.

`emit_north` becomes a projection of committed runtime events where applicable,
not the primary event source. Purely live connection observations that have no
database mutation still append their own journal event before/with broadcast.

### 4. External effects use intent, attempt, result, and reconciliation events

A database transaction cannot include an HTTPS call. Every external side effect
therefore uses a four-part evidence contract:

1. Persist the outbox intent and `*.enqueued` before I/O.
2. Persist `*.attempted` with the attempt number before the request.
3. Persist `*.succeeded` or `*.failed` after the response.
4. If the process can crash after provider success but before step 3, reconcile
   by provider marker/idempotency key and persist `*.reconciled`. Until that
   happens, the durable state is explicitly `outcome_unknown`, never silently
   treated as failed or retried as new work.

For GitHub writes, success evidence includes the operation, request hash, HTTP
status, attempt number, and provider receipt when one exists: comment id/URL,
review id/state, or status context plus target SHA. Tokens, Authorization
headers, installation credentials, and response bodies unrelated to the receipt
are never retained.

The same rule applies to OCP controller-runtime-event delivery. Each attempt,
scheduled retry, success, and dead letter is a journal event; the current
`controller_event_audit` view is retained for compatibility but is no longer
presented as a complete delivery history.

### 5. Evidence ownership follows ADR 031

OCP records only provider-neutral runtime facts:

- controller action received/accepted/denied/replayed/completed/failed;
- session open, state CAS, close, abort, timeout, and supersede;
- message recorded metadata, quorum, roster, thread, and result identity;
- bot connection, health, replacement, and liveness decisions;
- runtime-event enqueue, attempt, retry, delivery, and dead letter;
- operator configuration mutations and token lifecycle outcomes.

The provider controller records provider facts:

- authenticated ingress receipt, admission, ignore/deny reason, duplicate, and
  payload conflict;
- normalized actor and target identity needed to explain admission;
- action dispatch and returned `session_id`;
- runtime-event receipt, duplicate, conflict, and terminal projection;
- product rounds/findings;
- provider-side-effect enqueue, attempts, receipts, failure, and reconciliation.

OCP does not store raw GitHub webhook bodies or interpret GitHub operation
details. The controller does not receive a database handle or read OCP tables.
No cross-database transaction is introduced.

### 6. Authenticated, bounded query surfaces

Each service exposes the same filtered event query shape under its existing
operator/observer trust boundary:

```text
GET /v1/audit/events?session_id=&delivery_id=&action_id=&runtime_event_id=
    &write_id=&trigger_ref=&kind=&since=&until=&cursor=&limit=
```

Provider controllers may mount the equivalent under their existing API prefix.
Queries are cursor-paginated, newest-first by default, capped, and never return
secrets or raw credential material.

A first-party read-only investigation command composes the two APIs without
sharing databases:

```text
openabctl investigate --session ses_...
openabctl investigate --delivery d-...
openabctl investigate --trigger-ref github:pr/owner/repo#123
```

It emits one versioned investigation bundle containing the two service-local
streams and a causally ordered best-effort timeline. If one service is
unreachable or a correlation link is absent, the bundle reports the gap rather
than hiding it.

The query/API seam is part of this ADR. A dashboard is not.

### 7. Data minimization and actor honesty

- Store normalized authenticated ingress facts and a payload hash by default,
  not the full raw provider payload.
- Do not duplicate message bodies in `audit_events`; retain message id, hash,
  length, and authorization-aware link to the canonical message record.
- Error records contain a stable class, retryability, safe message, and HTTP
  status when useful. Headers, tokens, private keys, command stdout, repository
  bulk content, and arbitrary provider response bodies are excluded.
- An authenticated provider actor records stable provider id plus display name
  and association when supplied by the verified event.
- Today's shared operator key cannot identify a human. Operator actions record
  the credential class/installation identity, not a fabricated person. Per-human
  attribution begins only when OIDC or distinct operator credentials exist.
- Bot-authored messages identify the bot OCP authenticated; the journal does not
  infer which human or model internals caused the bot's reasoning.

### 8. Retention is explicit and does not redefine domain retention

The initial default journal retention is 90 days, configurable per deployment.
Security/configuration changes, terminal failures, dead letters, and
`outcome_unknown`/reconciled external effects retain for 365 days. A retention
sweep may batch-delete expired journal rows; it is the only allowed deletion
path and records a post-sweep aggregate `audit.retention_pruned` event outside
the removed range.

Domain tables keep their existing product retention. Removing an audit event
does not delete the session, message, finding, review round, or provider receipt
it references. Conversely, deleting product data must not silently claim its
audit evidence is still complete; investigation bundles mark missing referenced
domain records.

No historical backfill is claimed. Migration may create coarse `imported`
events for existing rows, but those rows must be visibly distinguished from
events recorded transactionally after adoption.

## Minimum event vocabulary

The first implementation covers this bounded set before adding more event
names:

| Owner | Required events |
|---|---|
| GitHub controller ingress | `ingress.received`, `ingress.accepted`, `ingress.ignored`, `ingress.denied`, `ingress.duplicate`, `ingress.conflict` |
| OCP actions | `action.received`, `action.accepted`, `action.denied`, `action.replayed`, `action.completed`, `action.failed`, `action.outcome_unknown` |
| OCP sessions | `session.opened`, `session.state_changed`, `session.message_recorded`, `session.roster_changed`, `session.quorum_reached`, `session.timed_out`, `session.closed`, `session.aborted`, `session.superseded` |
| Bot membership/liveness | `bot.connected`, `bot.disconnected`, `bot.health_changed`, `bot.replaced` |
| Runtime-event delivery | `runtime_event.enqueued`, `runtime_event.attempted`, `runtime_event.retry_scheduled`, `runtime_event.delivered`, `runtime_event.dead_lettered`, `runtime_event.received`, `runtime_event.duplicate`, `runtime_event.conflict` |
| Provider writes | `github.write.enqueued`, `github.write.attempted`, `github.write.succeeded`, `github.write.failed`, `github.write.retry_scheduled`, `github.write.outcome_unknown`, `github.write.reconciled` |
| Operator/configuration | `controller.installation_patched`, token rotate/revoke outcomes, `audit.retention_pruned` |

New providers replace the `github` namespace with their own. They do not add
provider event kinds to OCP.

## Implementation sequence

1. **Contract and correlation audit.** Add the shared serialized type and write
   a field-by-field map of existing ids. Promote `session_id`/`action_id` links
   currently recoverable only from JSON or naming rules into first-class
   columns without changing behavior.
2. **Dual-backend journal primitive.** Add `audit_events`, append/query Store
   methods, indexes, SQLite/Postgres migrations, idempotency tests, redaction
   tests, and conformance tests.
3. **OCP transaction points.** Journal controller actions, session/message/state
   changes, membership/liveness decisions, and every controller-event delivery
   attempt. Make north SSE a projection of the durable event where applicable.
4. **Controller transaction points.** Journal authenticated ingress admission,
   action dispatch/result, runtime-event receipt, round projection, and GitHub
   outbox lifecycle. Persist provider receipts and explicit unknown outcomes.
5. **Read surfaces.** Ship bounded APIs in both services and the first-party
   investigation bundle command. Do not wait for a dashboard.
6. **Retention and recovery drills.** Enable configured sweeps only after a
   restart/crash matrix proves the evidence chain survives every transaction/I/O
   boundary and a Postgres restore preserves the journals.

Do not turn every existing `tracing!` call into a journal event mechanically.
Only facts in the minimum vocabulary that change investigation conclusions are
durable records. Diagnostic prose remains a log concern.

## Acceptance criteria

Given any retained `delivery_id`, `action_id`, `session_id`, runtime `event_id`,
or provider `write_id`, an operator can produce an investigation bundle after
both services restart that shows, where applicable:

1. authenticated ingress identity, target, admission decision, and reason;
2. controller action request, authorization/idempotency outcome, and resulting
   session;
3. every session state/roster/quorum/liveness transition and the messages that
   supplied the evidence;
4. terminal result identity and controller-event enqueue/delivery attempts;
5. controller receipt and product projection;
6. every provider write intent and attempt, plus success receipt, terminal
   failure, or explicit unknown/reconciled outcome;
7. any missing service, pruned record, or broken correlation link as a visible
   gap.

Required tests run against SQLite and Postgres:

- crash before and after every local commit around action/session/event/write
  transitions;
- provider success followed by process death before local success recording;
- duplicate webhook, action, runtime event, and provider-write reconciliation;
- retry histories remain append-only and idempotent;
- redaction fixtures prove no bearer token, signing secret, private key, or raw
  Authorization header enters the journal;
- an investigation bundle generated without process logs or provider API access
  still satisfies items 1–7.

## Consequences

### Positive

- Incidents become reconstructable from first-party durable state rather than
  deployment logs or provider history.
- Existing ids become an explicit causal contract instead of incidental joins.
- OCP stays provider-neutral and controllers retain product-data ownership.
- Current domain tables remain authoritative; this is an additive history and
  query layer, not an event-sourcing rewrite.
- SQLite local development and Postgres production provide the same evidence.

### Negative

- Every critical mutation gains an additional write and index cost.
- Two local journals still require a read-time composition step; there is no
  globally serialized event order.
- Provider-call crash windows need reconciliation code and explicit
  `outcome_unknown` handling rather than a simple success boolean.
- Retention and redaction become product contracts that need tests and operator
  runbooks.

### Neutral

- Logs, `/v1/stats`, readiness, and canary summaries remain useful operational
  views but are not investigation sources of truth.
- Existing `controller_event_audit`, `webhook_deliveries`,
  `controller_action_idempotency`, and outbox tables remain; journal events
  explain their transitions rather than replace them.
- Full agent tool-call payloads remain outside OCP. The journal can record a
  coarse bot activity/error fact only when a trusted runtime boundary reports
  it.

## Non-goals

- Prometheus, OpenTelemetry, Grafana, log shipping, or a hosted observability
  vendor.
- A dashboard or general analytical query engine.
- One shared database or cross-service distributed transaction.
- Raw provider-payload retention by default.
- Full agent chain-of-thought, tool arguments/responses, shell output, or model
  telemetry.
- Tamper-proof/WORM/compliance certification.
- Backfilling a causally complete history for work performed before adoption.

## References

- [ADR 008 — External controller protocol](008-external-controller-protocol.md)
- [ADR 017 — Message observability / audit layer](017-message-observability-audit-layer.md)
- [ADR 020 — Review audit and effectiveness ledger](020-review-audit-effectiveness-ledger.md)
- [ADR 028 — Settled result identity](028-settled-result-identity.md)
- [ADR 031 — Provider-neutral OCP kernel](031-provider-neutral-kernel.md)
- [ADR 033 — PostgreSQL backing store](033-postgres-backing-store.md)
- [ADR 034 — Mutable controller registrations](034-mutable-controller-registrations.md)
- [Controller action API](../controller-action-api.md)
- [GitHub PR controller](../github-pr-controller.md)
