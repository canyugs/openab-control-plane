# Provider-neutral kernel migration plan

Status: **proposed** · 2026-07-22 · planning-only companion to
[ADR 031](adr/031-provider-neutral-kernel.md). This document owns the mutable
PR sequence, lane gates, cutover ledger, and deletion checklist. ADR 031 remains
the stable architecture record.

## Summary

The migration is an **external-path-first, deletion-last** extraction. OCP first
turns the existing in-process `ControllerAction` interpreter into a versioned,
authenticated network boundary and emits provider-neutral runtime events. A
GitHub PR-review controller is then built as an independently deployable service,
run in plan-only shadow mode, and canaried on an explicit repository before it
becomes the owner of GitHub ingress and product state.

The embedded `src/plugins/pr_review` path remains a compatibility implementation
and rollback target during that window. It is not deleted until the external
path has behavior parity, no owned lane has used the embedded path for a full
release, persisted compatibility sessions have drained, and provider credentials
and findings have moved out of OCP.

The first controller implementation may live as a second binary/crate in this
repository so protocol and fixture changes can land atomically. It must still be
an independent deployable with its own configuration, health, storage, and
rollback. Moving it to another repository is a later operational choice, not a
precondition for proving the boundary.

This plan does **not** select OctoBroker or any other GitHub capability broker.
Agent-side GitHub capability delivery has a separate design gate before OCP's
current token-vending path can be removed.

## 1. Starting point and residue ledger

The migration starts from the merged ADR 031 baseline (`7e6cd43`). The current
binary still contains all of these GitHub-specific seams:

| Concern | Current location | Compatibility disposition | Target owner |
|---|---|---|---|
| Raw webhook route and HMAC/admission | `src/api.rs`, `src/plugins/pr_review/webhook.rs` | Keep routable but disabled per canary scope | GitHub controller |
| Trigger parsing, presets, prompts, task rewriting | `src/plugins/pr_review/{council,tasks}.rs` | Replay-pinned; shadow against controller plans | GitHub controller |
| `review_council` coordinator and verdict grammar | `src/coordinator.rs`, `src/orchestrator.rs`, `src/plugins/pr_review/{mod,verdict}.rs` | Persisted-mode compatibility until drained | Generic coordinator inputs in OCP; product interpretation in controller |
| GitHub App identity and token lifecycle | `src/github_app.rs`, `src/identity.rs`, token handlers in `src/api.rs` | Remains until the capability gate is implemented | Controller, bot-local deploy config, or a separately selected broker |
| Installation-token cache | `installation_tokens` store methods/table | Stop new writes, then soft-remove after a full release | Outside OCP |
| Review findings and mappings | `src/plugins/pr_review/findings.rs`, review tables/store methods | Shadow, then controller-primary | GitHub controller store |
| GitHub-specific process config | `OABCP_ALLOWED_REPOS`, `OABCP_BOT_HANDLE`, `GITHUB_APP_*`, `GH_TOKEN`, review caps | Inject first; remove only with owning feature | Controller/deployer |
| GitHub dependencies and operator docs | `Cargo.toml`, `README.md`, `docs/config-reference.md`, install/runbook docs | Remove at final kernel cleanup | Controller documentation/package |

The existing frozen review-shaped v1 surfaces remain compatibility contracts.
No new controller may start depending on them.

## 2. Target deployment boundary

```text
GitHub
  │ raw webhook
  ▼
GitHub PR-review controller ── generic actions ──► OCP kernel ──► OpenAB bots
  │        ▲                                          │
  │        └──── signed generic runtime events ───────┘
  │
  ├─ GitHub credentials and side effects
  └─ review rounds, findings, delivery/object mappings
```

The target OCP process contains no GitHub webhook handler, App key, token mint,
PR schema, prompt, verdict parser, or provider side-effect code. It receives only
generic actions, stores generic runtime state, and sends generic runtime events.

### Initial repository shape

After configuration injection, introduce a Cargo workspace with these logical
units (exact directory names may be adjusted by the extraction PR without
changing ownership):

```text
crates/
├── controller-protocol/       # versioned action/event DTOs + conformance fixtures
└── github-pr-controller/      # independent binary, ingress, policy, product store
src/                           # OCP kernel package during compatibility
```

`controller-protocol` contains only provider-neutral serialized types and test
fixtures. `github-pr-controller` may depend on it, but must not import OCP
`AppState`, `Store`, coordinator implementations, or internal migration code.
The controller and OCP are always built into separate images and deployable
independently, even while they share a repository.

## 3. Invariants for every implementation PR

1. Existing v1 wire snapshots and persisted mode dispatch stay valid throughout
   the compatibility window.
2. Raw GitHub payloads never cross the new controller action boundary.
3. Provider terms are forbidden in new generic action, event, audit, and schema
   fields. `trigger_ref` and `result_ref` remain opaque strings.
4. Controllers mutate OCP only through authenticated actions. They never receive
   a store handle, SQLite path, or root operator credential.
5. `(controller_id, action_id)` replay is idempotent, and controller-scoped
   trigger dedupe is deterministic.
6. One provider object has exactly one active ingress owner and one active
   side-effect owner. Shadow mode performs no OCP mutation and no GitHub write.
7. Schema work before cleanup is additive. An older image must tolerate newly
   added generic tables; destructive/soft-removal work happens last.
8. Every rollout records the OCP image, controller image, route ownership, and
   rollback action before traffic changes.
9. OAB-to-OAB delegation, a physical OCP plane split, and capability-broker
   selection remain outside this plan.

## 4. Compatibility and cutover model

Each repository moves through the following states. The state is explicit
configuration and audit data, not inferred from whether a service happens to be
reachable.

| State | Raw ingress owner | OCP actions | GitHub writes | Findings owner | Allowed purpose |
|---|---|---|---|---|---|
| `embedded` | OCP | In-process interpreter | Existing bot/controller path | OCP | Current production and instant rollback |
| `shadow` | OCP; controller receives replay/copy | None from controller | None from controller | OCP | Compare plans and projections only |
| `external_canary` | Controller | Versioned HTTP action API | Exactly one explicitly named external-path actor | Dual-observed, OCP primary | One allowlisted repository |
| `external_primary` | Controller | Versioned HTTP action API | External path only | Controller primary | Broader owned-lane traffic |
| `external_only` | Controller | Versioned HTTP action API | External path only | Controller | Embedded route unavailable |

Promotion is per repository. A cutover ledger must record at least:

```text
repository
state
ingress_route_revision
ocp_image
controller_image
side_effect_owner
findings_owner
credential_path
promoted_at / promoted_by
rollback_route / rollback_images
```

Rollback never enables both ingress paths. The operator first stops or drains
the external route, confirms no accepted action is in flight for the target
delivery id, then atomically restores embedded routing.

## 5. Implementation sequence

Every row is one reviewable PR unless the scope proves smaller. A PR is complete
only when its listed tests pass and its rollback note is recorded. IDs are stable
references for issues and release notes; PR numbers are filled in as work lands.

| ID | Deliverable | Scope | Depends on | Acceptance |
|---|---|---|---|---|
| P0 | Baseline fixtures and zero-use telemetry | Capture replayable webhook fixtures for opened/reopened/ready, `/review`, `/ask`, mention, rerereview/supersede, ignored admission, and invalid HMAC. Snapshot the resulting session plan, prompts, roster, chair, quorum, trigger/fingerprint, terminal verdict, findings, and expected GitHub writes. Add counters for embedded ingress, legacy mode dispatch, token vending, and OCP findings writes. | — | Fixtures contain no live secret; replay is deterministic; current path passes; each future deletion has a measurable zero-use signal. |
| P1 | Inject PR-review configuration | Replace process-global reads of `OABCP_ALLOWED_REPOS`, `OABCP_BOT_HANDLE`, review caps, and webhook settings with an injected compatibility `PrReviewConfig`. Preserve env loading only in composition root code. | P0 | Parallel tests require no global env lock; identical fixture plans before/after; current deployment env remains accepted. This closes ADR 018's crate-split precondition. |
| P2 | Protocol crate and conformance corpus | Extract provider-neutral action DTOs and serialized fixtures into `controller-protocol`; define version negotiation and stable error envelopes. Keep in-process execution as an adapter. Add generic `recipient_inputs` and optional opaque `result_ref` only if fixture parity proves they are required. | P1 | Protocol crate has no OCP/GitHub dependency; JSON round trips are golden-pinned; provider-vocabulary grep gate passes. |
| P3 | Complete the generic action interpreter | Implement reserved `add_roster`, `close_session`, and `emit_status` actions through existing state-transition, backfill, and audit mechanisms. Make `open_session` per-recipient input atomic if P2 selects it. | P2 | Unknown bot/session, invalid quorum, unauthorized close, replay, supersede, backfill, and partial-failure tests pass; no caller bypasses interpreter cleanup. |
| P4 | Controller installations and HTTP action API | Add additive generic tables for controller installations, hashed/versioned action tokens, grants, quotas, and `(controller_id, action_id)` results. Expose the versioned action endpoint with constant-time auth checks and controller-scoped trigger dedupe. | P3 | Conformance tests cover invalid/revoked/rotated token, grant denial, scope denial, quota, duplicate action id, duplicate trigger ref, concurrent replay, and stable error bodies. Older OCP image starts against the additive schema. |
| P5 | Generic runtime-event delivery | Persist and sign provider-neutral `session.opened`, progress, terminal, timeout, supersede, and action-failure events. Implement bounded retry, dead letter, replay protection, and operator audit. Do not send raw provider events. | P4 | Controller fixture verifies canonical signature bytes, timestamp/replay rejection, retry schedule, dead letter, duplicate delivery, and recovery. Delivery failure cannot roll back an already valid session transition. |
| P6 | Independent GitHub controller skeleton | Add the second deployable. It owns webhook HMAC, delivery dedupe, repository/admission policy, trigger parsing, GitHub App configuration, health, and a separate product database. Initially it may only produce a `SessionPlan`; its OCP action and GitHub write clients are disabled. | P2 | Binary builds/starts without OCP internals or DB access; OCP builds/starts without controller config; health distinguishes ingress, OCP, GitHub, and product-store readiness. |
| P7 | Plan-only shadow parity | Replay P0 fixtures and mirror selected live events to the controller. Compare controller `SessionPlan` with embedded behavior: identity keys, roster, chair, quorum, mode, recipient inputs, prompt bytes, dedupe/supersede, terminal projection, and proposed writes. | P6 | Shadow has zero action credentials and zero GitHub write credentials; required fixture parity is 100%; live mismatch budget is zero for identity/ownership fields and explicitly reviewed for presentation-only text. |
| P8 | External canary ingress | Give the controller scoped action auth and route one explicit repository's raw webhooks to it. Use the compatibility credential/findings paths temporarily if needed, but never duplicate ingress or side effects. | P4, P5, P7 | Full dev-lane council completes; duplicate GitHub delivery creates one session; supersede works; timeout and controller outage are visible; atomic route rollback succeeds; embedded counters remain zero for the canary repo. |
| P9 | Controller-owned product projection in shadow | Store review rounds, findings/events, head/fingerprint mapping, provider delivery ids, and comment/status mapping in the controller DB. Derive them from generic runtime events/final messages; compare against OCP's legacy projection. | P5, P8 | Historical replay and live canary produce equivalent finding identity, severity, resolution, and terminal result; controller rebuild from event replay is tested; no direct OCP DB read. |
| P10 | Product state primary cutover | Make the controller product store/API primary for external repositories. Stop new OCP review finding writes for those sessions while retaining compatibility reads/export for old data. Set only an opaque controller-owned `result_ref` in generic runtime state. | P9 | Dual-read comparison passes for a full canary window; old sessions remain readable; new external sessions add no review-specific OCP rows; restore-to-OCP-primary runbook is exercised before promotion. |
| P11 | Agent-side GitHub capability ADR gate | Write and approve a separate ADR comparing controller-delivered short-lived capability, bot-local deployment configuration, and an external capability broker. Define read/write separation, refresh, revocation, audit, failure behavior, and migration from both App and PAT modes. | P8 | Security and operations review accepted. The ADR may select OctoBroker, another broker, or no broker; this plan supplies no default. No token-vending deletion may proceed without this gate. |
| P12 | Capability path implementation and canary | Implement the selected mechanism outside OCP; migrate reviewer read and side-effect write capabilities on the canary. Stop calling OCP token routes for migrated sessions and stop creating installation-token rows. | P11 | Least-privilege read/write tests, expiry/refresh/revoke tests, controller/broker outage test, secret-leak scan, and live canary pass; OCP token-vending telemetry is zero for external sessions. |
| P13 | Generic coordinator migration | Express external PR review through generic `council` behavior plus declarative recipient inputs/completion policy. Keep `review_council` dispatch only for persisted compatibility sessions. | P3, P8 | P0 behavior parity passes without selecting `review_council`; restart dispatches old rows safely; new external sessions never persist the legacy mode. |
| P14 | Provider-neutral v2 north/runtime contracts | Introduce generic terminal/runtime/result shapes where frozen v1 contracts still expose review vocabulary. Keep v1 snapshots and adapters for the declared compatibility window; migrate controller and non-GitHub consumer first. | P5, P10, P13 | v1 remains byte-pinned; v2 contains no provider fields; mixed-version controller tests pass; compatibility duration and removal telemetry are documented. |
| P15 | Disable and remove embedded GitHub ingress/plugin | After one full released version with zero embedded use on owned lanes, remove `/api/v1/github_webhooks`, review routes, webhook config, `src/plugins/pr_review`, and static `review_council` lookup for new work. Retain only the minimum read/dispatch adapter required by undrained rows, if any. | P10, P12, P13, P14 | Route returns the documented removal response; persisted-session inventory is zero or an explicit adapter test covers every remaining row; external ingress rollback uses the prior image, not a hidden dual route. |
| P16 | Remove OCP GitHub credentials and product storage | Remove token-vending handlers, GitHub App mint/cache/revoke code, new writes to legacy review/token tables, and provider-specific stats/store APIs. Export or retain old product data per an approved retention note; use the repository's soft-drop pattern for legacy SQLite columns/tables. | P12, P15 | OCP starts with no `GITHUB_APP_*`/`GH_TOKEN`; credential/finding grep and schema-write gates pass; old DB upgrades and fresh DB creation pass; prior image rollback procedure is bounded and documented. |
| P17 | Final kernel cleanup and proof release | Remove GitHub-only dependencies, config, templates, golden files, install docs, and remaining provider vocabulary from kernel paths. Move still-valid GitHub operator docs to the controller package. Run the non-GitHub E2E proof and publish independently versioned OCP/controller images. | P16 | All ADR 031 acceptance criteria pass; OCP binary starts with no provider secret/config; a non-GitHub flow proves create → delivery → coordination → terminal → timeout; final purity allowlist is empty except historical ADR/archive text and opaque fixture values. |

### Why the sequence is ordered this way

- Configuration injection precedes a workspace split so tests do not depend on
  process-global env mutation across crates.
- Action auth and runtime events precede external mutation so the controller
  never needs an OCP root key, DB, or polling shortcut.
- Shadow parity precedes canary, and canary precedes product-state/credential
  removal.
- Raw ingress may move before capability delivery because the current token path
  can remain as an explicit compatibility dependency. That dependency is
  measurable and cannot be deleted until P11/P12 pass.
- Schema and dependency cleanup are last so every earlier rollback is an image
  and route change rather than data restoration.

## 6. Test and conformance matrix

| Layer | Permanent proof |
|---|---|
| Protocol | Golden action/event/error JSON; version negotiation; provider-vocabulary grep |
| Action authorization | Rotation overlap, revocation, grants, scope, quota, constant-time token verification |
| Idempotency | Concurrent duplicate action, duplicate provider delivery, duplicate trigger ref, retry after response loss |
| Runtime events | Signature canonicalization, replay window, retry/dead letter, ordering and duplicate tolerance |
| Session mechanism | Atomic open/input, fanout, replacement/backfill, supersede, quorum, timeout, terminal enforcement |
| GitHub parity | P0 fixture corpus plus live shadow comparison of plans, projections, and proposed side effects |
| Ownership | Test/config assertion that only one ingress and side-effect owner is enabled per repository |
| Compatibility | Frozen v1 wire snapshots; old persisted-mode restart; old DB → new image; additive-schema new DB → old image |
| Provider neutrality | OCP compile/start without GitHub env/deps; non-GitHub E2E imports no provider package |

No live GitHub fixture may contain a token, webhook secret, private key, private
repository content, or personally identifying comment body. Fixtures use
synthetic payloads or reviewed redaction with deterministic hashes.

## 7. Publish and lane cadence

There are six mandatory publish points. Each publish is followed by the listed
gate before the next destructive or ownership-changing group starts.

| Publish point | After | Required lane proof | Rollback |
|---|---|---|---|
| A — generic boundary | P5 | Non-GitHub conformance controller opens/posts/adds/closes and receives signed terminal/timeout events | OCP image revert; additive tables remain unused |
| B — shadow | P7 | Fixture parity + live plan-only report; controller lacks mutation/write credentials | Stop shadow copy; controller image revert |
| C — canary ingress | P8 | One allowlisted repo completes open, supersede, verdict, timeout, duplicate delivery, and controller-outage drills | Atomic ingress route restore + controller stop |
| D — product primary | P10 | Full comparison window; new controller rows rebuild from runtime events; no new OCP findings | Flip findings read owner; keep external ingress only if healthy |
| E — capability external | P12/P14 | GitHub read/write capability and mixed v1/v2 compatibility proven; OCP vending zero for external sessions | Restore compatibility capability path and previous images |
| F — provider-neutral kernel | P17 | External-only GitHub E2E plus non-GitHub E2E; zero-use evidence archived | Prior compatible OCP/controller image pair; no automatic schema downgrade |

Promotion beyond the canary repository requires one complete release window
without a severity-1 parity, ownership, credential, or product-state mismatch.
Presentation-only prompt drift must still be reviewed and recorded; it is not an
automatic waiver.

## 8. Operational failure and rollback rules

- **Controller unavailable before action acceptance:** return provider-appropriate
  retry status and rely on provider redelivery; do not fall through to embedded
  ingress.
- **Action accepted but controller loses the response:** retry the same
  `action_id`; OCP returns the stored result.
- **Runtime event delivery unavailable:** OCP session mechanism continues; event
  remains durable and eventually dead-letters with operator visibility.
- **GitHub side effect uncertain:** reconcile by provider delivery/object mapping
  before retry. Never post a second verdict merely because the HTTP response was
  lost.
- **Parity or ownership violation:** freeze promotion, disable the external route
  for that repository, drain in-flight accepted actions, then restore embedded
  routing. Preserve both audit trails for comparison.
- **Product-store issue:** do not write findings back into OCP for external
  sessions ad hoc. Use the tested P10 ownership rollback or replay runtime events
  into the repaired controller store.
- **Capability outage:** stop provider writes and surface degraded status. Do not
  mint a broad emergency token in OCP.

Every runbook names the maximum safe compatibility image pair. Rolling OCP back
across an action/event protocol change without its matching controller is not an
approved rollback.

## 9. Deletion gates

The following checklist is intentionally evidence-based. “Code appears unused”
is not deletion evidence.

- [ ] Embedded raw-ingress count is zero on all owned lanes for one full released
      version.
- [ ] `review_council` has no active or restartable persisted session, or a
      minimal compatibility adapter has a separately approved expiry.
- [ ] OCP token-vending count and new `installation_tokens` writes are zero for
      every external repository for one full released version.
- [ ] New OCP review finding/product rows are zero; controller state rebuild has
      been exercised from retained runtime events.
- [ ] v2 consumers are inventoried and the v1 compatibility window has expired.
- [ ] Provider controller rollback no longer depends on re-enabling an endpoint
      removed from the current image; the approved prior image pair is retained.
- [ ] Legacy data export/retention and SQLite soft-removal behavior are approved.
- [ ] OCP fresh-install and legacy-upgrade tests pass with no GitHub config.
- [ ] Non-GitHub E2E and external GitHub E2E both pass on the release candidate.

## 10. Exit criteria

This migration is complete only when all of the following are true:

1. GitHub sends raw events to the independently deployed controller, not OCP.
2. The controller uses only the versioned action/runtime-event boundary and has
   no OCP DB or internal-code dependency.
3. OCP owns no GitHub credential, installation token, PR finding, product
   mapping, or provider side-effect implementation.
4. New sessions use provider-neutral coordinator/action/event contracts; old
   persisted compatibility rows are drained or covered by a bounded adapter.
5. OCP builds, starts, and operates without GitHub dependencies, variables,
   secrets, or configuration.
6. Existing PR-review behavior retained parity through shadow, canary, and a
   full external-primary release window.
7. A non-GitHub E2E proves session creation, delivery, coordination, terminal
   state, and timeout on the final kernel.
8. OAB-to-OAB delegation remains outside the implementation and test contract.

## 11. Explicitly deferred decisions

- Whether the proven GitHub controller later moves to a separate repository.
- Which agent-side GitHub capability mechanism P11 selects, including whether
  OctoBroker participates at all.
- OAB-to-OAB delegation and any contract proposed by OpenAB PR 1435.
- A physical split of OCP data/control/membership planes.
- A plugin marketplace, dynamic code loading, or an in-process ABI.
- Normalizing arbitrary provider webhook schemas inside OCP.
