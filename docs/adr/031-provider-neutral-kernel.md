# ADR 031 — Provider-neutral OCP kernel and controller-owned ingress

Status: accepted · 2026-07-22

Amends:
[ADR 007](007-control-plugins-and-oab-father.md),
[ADR 008](008-external-controller-protocol.md), and
[ADR 018](018-stage3-extraction.md).

## Context

OCP began with GitHub PR review as its first product profile. That proof point
exercised the hard runtime problems: webhook admission, deterministic session
creation, roster fanout, durable delivery, quorum, liveness, identity, and a
visible external side effect.

The proof point still shapes the deployed binary. Even after Stage 3 moved
review behavior into `src/plugins/pr_review`, one process and database retain:

- raw GitHub webhook ingress and validation;
- PR-review and triage trigger construction;
- GitHub App credential custody and installation-token lifecycle;
- review-specific prompts, presets, verdict projections, and findings;
- GitHub-specific API, metrics, configuration, and release concerns.

ADR 007 already defines PR review as a control plugin rather than the kernel's
product model. ADR 018 completed an in-process module extraction, but explicitly
left external transport dormant and several GitHub residues in the kernel. ADR
008's original external-controller direction also sends raw provider events into
OCP first, then asks OCP to normalize and deliver them to a controller.

The forum north client and generic `solo`/`pipeline` flows now provide a second
consumer of the runtime. They make the durable boundary clearer: OCP's product
is provider-independent coordination, not GitHub event processing. Keeping raw
provider ingress, credentials, and product storage in the runtime unnecessarily
couples trust domains, deployment cadence, schema evolution, and failure modes.

## Decision

### 1. OCP core is provider-neutral

The target OCP kernel owns only coordination mechanism and its guarantees:

- bot registry, bot admission, and gateway identity;
- session lifecycle and compare-and-swap state transitions;
- roster, replacement, fanout, backfill, isolation, and membership health;
- message delivery, outbox, replay, and generic runtime audit;
- the coordinator seam and generic coordinator implementations;
- liveness watchdogs, deadlines, and terminal-state enforcement;
- generic north APIs, SSE/runtime events, controller authentication, action
  authorization, idempotency, and quotas.

The target kernel does not understand repositories, pull requests, issues,
comments, provider labels, review presets, finding severities, provider
webhook schemas, provider credentials, or provider side effects.

### 2. Provider controllers own raw ingress

Raw provider events terminate at a provider controller, not at OCP:

```text
provider -- raw event --> provider controller
                              |
                              | generic controller action
                              v
                           OCP core
                              |
                              v
                        OpenAB bot pods
```

The provider controller owns:

- webhook or event-source authentication;
- provider permission and admission checks;
- provider delivery deduplication and object mapping;
- trigger interpretation, prompt construction, roster selection, and requested
  coordinator policy;
- provider-specific state and projections;
- provider API calls and side effects.

This amends ADR 008's provider-event direction. OCP may deliver generic runtime
events such as session progress, close, timeout, or action failure to a
controller, but it does not normalize raw GitHub, Slack, forum, or other
provider payloads on the target path.

### 3. Controllers mutate OCP only through the action contract

A controller never receives an OCP database handle and never writes runtime
state directly. It requests declarative actions through the versioned
controller boundary. The v1 action vocabulary remains:

- `open_session`;
- `post_message`;
- `add_roster`;
- `close_session`;
- `emit_status`.

Every action is authenticated as a controller installation, authorized against
its grants, idempotent under a controller-scoped action id, validated by OCP,
and executed through the same state-transition and liveness machinery used by
first-party north clients.

The action schema may grow generic fields needed by real controllers, such as
per-recipient input or an opaque result reference. It must not gain provider
vocabulary.

### 4. Trigger references are opaque correlation keys

OCP may store values such as:

```text
github:pr/openabdev/openab#1435
forum:post/12345
incident:alert/abc
custom:task/xyz
```

The kernel uses `trigger_ref` for idempotency, lookup, audit correlation, and
operator diagnostics only. The controller owns its syntax and interpretation.
OCP does not parse provider-specific segments or fetch the referenced object.

### 5. Provider credentials and capabilities live outside OCP

OCP retains only credentials required for its own north and south boundaries,
such as controller action credentials, operator API credentials, and per-bot
gateway tokens.

Provider integrations own provider webhook secrets, App identities, private
keys, installation selection, API authentication, and side-effect authority.
Agent-side provider capability may be delivered by the controller, by bot-local
deployment configuration, or by an external capability broker. OCP may record
an opaque grant or capability reference, but it does not mint or persist a
provider bearer token.

For the GitHub profile, the target state removes GitHub App private keys, PAT
settings, installation-token mint/cache/revoke behavior, token-vending routes,
and the `installation_tokens` table from OCP.

This ADR deliberately does not select a GitHub capability broker. The ownership
boundary is the decision; the concrete credential path is migration work.

### 6. Product state lives with the provider controller

The OCP store owns generic runtime state:

- bots and controller installations;
- sessions and participants;
- messages, reactions, delivery state, and outbox rows;
- generic action idempotency, runtime events, and audit records.

A GitHub PR-review controller owns product state such as review rounds,
findings, finding events, PR head/fingerprint mappings, provider delivery ids,
and comment/status mappings. It may use its own store, but it never shares or
directly queries OCP's database.

Provider-specific terminal projections do not justify new session columns.
The kernel records generic terminal state, reason, final messages, and an
optional opaque controller-owned result reference. A controller derives and
stores its product projection from those generic records and events.

### 7. The bundled GitHub plugin is a compatibility stage, not the final boundary

Stage 3's `src/plugins/pr_review` extraction remains valid history and remains
the rollback-compatible implementation during migration. It is not the target
deployment boundary.

The target GitHub profile is an independently deployable controller with its
own ingress, credentials, product state, release cadence, health, and rollback.
The OCP binary must be able to build, start, and serve a non-GitHub workload
without GitHub configuration or secrets.

Provider-specific coordinator modes, prompt rewriting, verdict parsing, and
trigger handlers remain available only in the compatibility implementation.
New external-controller sessions use generic coordinator modes and declarative
inputs. Persisted compatibility sessions continue to dispatch safely until the
migration proves that no live row or caller needs the old mode.

### 8. Existing v1 contracts remain compatible during migration

ADR 018 froze review-shaped v1 surfaces because they have live consumers. This
ADR does not authorize an in-place breaking removal.

The embedded GitHub routes, review-shaped SSE/close-webhook fields, statistics,
and legacy token paths are deprecated compatibility surfaces. They remain
snapshot-pinned until an external GitHub controller has behavior parity, the
owned lanes have completed a compatibility window, and removal evidence is
recorded.

No new consumer may depend on a deprecated review-shaped kernel field. New
controllers use provider-neutral actions and runtime events.

### 9. The OpenAB south boundary does not change

This decision does not change the stock OpenAB gateway protocol, OCP bot
registration, direct participant roster model, bot session pools, bot-side
steering, or agent tool execution. It extracts provider product concerns from
the north and storage boundaries; it does not redesign the south data plane.

## Ownership after extraction

| Concern | OCP core | Provider controller | OpenAB bot/deployer |
|---|---|---|---|
| Raw provider event | No | Validate and interpret | No |
| Session state and liveness | Enforce | Request actions, observe events | Participate |
| Roster and delivery | Enforce | Select/request | Receive and reply |
| Prompt/product policy | Generic transport only | Construct | Execute |
| Provider credentials | No | Own or bind external broker | Consume only if explicitly provisioned |
| Provider side effects | No | Perform or authorize | Perform only with explicit capability |
| Product projections/findings | No | Store and expose | Produce source material |
| Runtime audit/outbox | Own | Correlate | No direct access |

## Relation to prior ADRs

- **ADR 007 remains the product boundary** but its extraction trigger is now
  fired: the PR-review control plugin must leave the OCP process, not merely
  occupy a module inside it.
- **ADR 008 keeps the external action/auth/idempotency model** but is amended so
  provider controllers own raw ingress. OCP emits only generic runtime events.
- **ADR 018 remains the historical Stage 3 ruling**. Its same-crate layout and
  frozen compatibility contracts describe the migration starting point, not
  the final provider-neutral kernel. Its GitHub residue exit triggers are now
  active.

## Migration invariants

The cutover sequence, PR breakdown, compatibility duration, schema cleanup,
and rollback commands live in the separate
[provider-neutral kernel migration plan](../provider-neutral-kernel-migration.md).
That plan must preserve these ADR-level invariants:

1. Pin the existing GitHub flow with replayable fixtures and end-to-end tests
   before changing ingress.
2. Build the external controller against the generic action interpreter; it
   must not call internal store helpers.
3. Run plan-only shadow comparison before any external controller side effect.
4. A provider object has exactly one active side-effect owner at a time.
5. Canary the external path on an explicit repository before broader routing.
6. Move provider credentials and product state only after behavior parity.
7. Keep existing v1 wire snapshots and persisted-mode dispatch valid through
   the compatibility window.
8. Make every stage rollbackable by image and ingress-route changes; schema
   cleanup happens last.
9. Remove the embedded plugin only after at least one release of zero-use
   evidence on owned lanes.
10. Prove the resulting kernel with a non-GitHub end-to-end flow.

## Consequences

### Positive

- OCP can run without any GitHub installation, secret, or schema concern.
- Provider compromise no longer directly exposes the coordination runtime's
  credential domain.
- Provider controllers and OCP can deploy, scale, fail, and roll back
  independently.
- New providers reuse deterministic session and delivery guarantees without
  adding provider code to the kernel.
- Product schemas can evolve without widening OCP's session schema or frozen
  runtime contracts.

### Costs and residual risks

- Operators deploy and observe an additional service per provider profile.
- The controller action/runtime-event contract becomes a maintained network
  API rather than an in-process call.
- Controller delivery, idempotency, and product storage need explicit
  operational ownership.
- The compatibility period temporarily maintains embedded and external code
  paths, increasing test and release cost.
- Moving agent-side GitHub capability out of OCP requires a separately reviewed
  credential-delivery design before the current token path can be deleted.

## Non-goals

This ADR does not decide or implement:

- OAB-to-OAB delegation or any change derived from its proposed protocol;
- a physical split of OCP's data, control, and membership planes;
- an OAB Father UI, plugin registry, or marketplace;
- dynamic third-party code loading or an in-process Rust plugin ABI;
- a specific GitHub capability broker or OctoBroker deployment;
- new coordinator semantics or a redesign of roster membership;
- immediate deletion of legacy routes, fields, modes, tables, or secrets;
- a generic schema for normalizing every provider's raw events.

## Alternatives considered

### Keep the GitHub plugin bundled permanently

Rejected. A module boundary improves code organization but leaves provider
credentials, product storage, release cadence, and failure scope inside the
runtime process.

### Receive raw provider events in OCP and forward normalized events

Rejected as the target architecture. It retains provider webhook parsing,
authentication, delivery semantics, and schema churn in OCP. Generic runtime
events from OCP to controllers remain valid; raw provider ingress does not.

### Load provider plugins through an in-process ABI

Rejected. It preserves a shared process and trust domain, couples plugin and
kernel releases, and creates ABI/versioning work before an external contract is
proven.

### Fork one OCP deployment per provider

Rejected. It duplicates the session, delivery, identity, and liveness kernel
and lets provider-specific forks drift from its guarantees.

### Move session orchestration into every provider controller

Rejected. Controllers own product policy, not durable runtime mechanism.
Reimplementing session state, outbox/replay, roster admission, and watchdogs in
each controller discards the shared guarantees OCP exists to provide.

## Acceptance criteria

This decision is realized only when:

1. OCP builds and starts with no GitHub environment variables, credentials, or
   configuration.
2. A non-GitHub end-to-end flow exercises session creation, delivery,
   coordination, terminal state, and timeout.
3. Raw GitHub webhooks terminate at an external controller rather than OCP.
4. The GitHub controller uses only the versioned controller action/runtime-event
   boundary and has no OCP database access.
5. No provider-specific field is added to a new kernel schema or generic wire
   contract.
6. OCP no longer stores GitHub tokens, PR findings, or provider side-effect
   mappings.
7. Existing GitHub PR-review behavior and side effects retain parity through
   the compatibility window.
8. The embedded GitHub plugin and deprecated compatibility surfaces can be
   removed without invalidating live sessions or persisted rows.
9. OAB-to-OAB delegation remains outside this decision.

## Deferred

- The external controller's deployment repository and packaging shape.
- The provider-neutral v2 runtime-event schema and compatibility duration.
- The exact migration PR sequence and lane rollout schedule.
- The agent-side GitHub capability-delivery mechanism.
- Removal migrations for legacy review columns and token tables.
