# SDD quality workflow implementation plan

Status: proposed · 2026-07-13

Governing decision: [ADR 022 — Stage-gated SDD workflow](adr/022-stage-gated-workflow.md)

Research basis:
[Prior art for the stage-gated SDD quality workflow](sdd-quality-workflow-prior-art.md)

## Outcome

Deliver a durable external `sdd-controller` that uses OCP as its agent-session
runtime and runs this profile on stock OpenAB pods:

```text
Discuss → Explore → Prototype → Spec → Usage → Ticket → Dev → Review → Wrap
```

Every stage follows the same bounded quality loop:

```text
produce → quality_review → rating
                            ├─ predicate fails → fix → quality_review → rating
                            └─ hard checks pass and score > 9
                                 → required human gate (if configured) → accept
```

The capitalized **Review** stage is a product stage whose candidate is the
release manifest: exact code revision plus finding/verdict set. Its hard gate
requires approval with no unresolved blocker. The lowercase `quality_review`
phase is the generic evaluator around every stage artifact, including the
Review-stage candidate. They are distinct records and prompts.

Completion means more than a happy-path demo. A run must survive controller and
OCP restarts, reject stale or malformed ratings, avoid duplicate business
attempts under redelivery, stop on configured limits, expose audited operator
controls, and prove that the exact accepted artifact revision feeds the next
stage.

## Architecture boundary

| Component | Owns | Must not own |
|---|---|---|
| `sdd-controller` | parent workflow, cursor, attempts, quality/approval gates, artifact chain, effect plans/adapters, durable action/replay semantics, recovery, operator API | OCP delivery internals or agent reasoning |
| OCP | child sessions, roster/admission, identity, WS delivery, liveness, session CAS, logs and query API | SDD stages, scores, backward workflow transitions |
| OpenAB pods | producing, reviewing, rating, fixing, and invoking allowed draft/preparation tools | deciding whether the workflow cursor advances or holding trusted irreversible-publish credentials |
| Git/issue tracker/object store | canonical artifact bodies and revisions | workflow coordination state |

The controller is a separate deployable service. Its exact source repository and
operator are W0 decisions; it is not a new `src/plugins/sdd_workflow` module in
the OCP binary. OCP remains the execution substrate for every agent phase.

A typical stage uses fresh OCP child sessions:

```text
sdd-controller durable state/journal
  │
  ├─ open produce session ────────────────▶ OCP ─▶ producer pod(s)
  ├─ record artifact URI + digest
  ├─ open quality_review session ─────────▶ OCP ─▶ reviewer council
  ├─ open rating session ─────────────────▶ OCP ─▶ independent rater
  ├─ validate typed result + evaluate gate
  └─ accept stage, or open fix session ───▶ OCP ─▶ fixer pod(s)
```

The close webhook may wake the controller. Durable controller state/journal
plus level-driven OCP query/replay is the correctness path.

## Prior-art constraints and runtime selection

The research found three independent layers:

1. Spec Kit defines versioned artifacts and cross-artifact
   analysis/convergence; Kiro supplies explicit approval points; OpenSpec
   supplies delta specs and canonical archive/sync.
2. Anthropic's evaluator–optimizer and the surveyed agent frameworks define a
   bounded produce → evaluate → improve pattern.
3. Temporal, Restate, DBOS, and Step Functions define the durable execution
   behavior required across retries, process restarts, and human waits.

OCP is already the agent-session layer. A LangGraph graph could technically be
the external controller, but the first bake-off excludes it because this fixed
linear profile does not use its Python/JS graph and agent surface, and its
checkpointer does not remove external-effect idempotency or storage operations.
Conversely, a custom SQLite schema must not be treated as durable merely because
it has an outbox table.

W0 makes an explicit substrate decision:

| Candidate | W0 position | Selection concern |
|---|---|---|
| Restate-backed Rust controller | lead engine candidate | size two services (Restate Server + controller handler) and persistent storage; prove signals/queries, journal replay, retention, independent crashes/upgrades, backup/restore, and the actively developing Rust SDK |
| Explicit SQLite/Postgres + CAS/events/outbox/reconciler | required baseline/fallback | prove every ambiguous OCP boundary, timer, command, lease, migration, and restore path without recreating an unsafe workflow engine |
| DBOS | conditional | evaluate only if a supported non-Rust language plus production Postgres and Conductor operations/licensing are acceptable |
| Temporal | reference/deferred | semantics are the benchmark; service footprint and Public Preview Rust SDK are not justified for the first profile |
| LangGraph | explicitly rejected from W0 | capable of external orchestration, but unused graph/agent abstractions plus a production checkpointer do not reduce this profile's durability burden |
| Step Functions | rejected | AWS and ASL coupling conflict with the intended Zeabur-native service |

Restate and the explicit database baseline run the same one-stage crash fixture.
W0 produces a provisional recommendation; W1 must confirm its OCP boundary
assumptions against the real north API. No production W2 implementation starts
until the final selection record names the winner, measured failure behavior,
operational cost, rejected alternatives, and rollback plan. If a durable runtime
wins, its journal/durable-call mechanism replaces the workflow-level action
outbox; the design does not operate two competing replay systems.

## Requirements traceability

Prior-art §6 requirements map onto ADR 022 decisions and this plan as follows
(R-numbers from
[the prior-art study](sdd-quality-workflow-prior-art.md#6-derived-implementation-requirements)):

| Requirement | ADR 022 | Plan home | Verified by |
|---|---|---|---|
| R1 external parent ownership | Decision, D2 | Architecture boundary | W2 |
| R2 phase isolation | D1 | Child-session identity and attempt semantics | W1, W2 |
| R3 durable substrate selection | D2 | Prior-art constraints and runtime selection | W0, W1 |
| R4 equivalent guarantees | D2 | Logical persistence and recovery contract | W0, W2 |
| R5 retry separation | D1 | Child-session identity and attempt semantics | W0, W2 |
| R6 composite gate | D3 | Immutable result contracts | W0 fixtures |
| R7 artifact truth | D4 | Artifact manifest and forward supersession | W3 |
| R8 definition pinning | D3, D7 | Versioned profile contract | W0, W3 |
| R9 bounded loops | D6 | profile `budgets` defaults; W4 counters | W4 |
| R10 human control | D6, D7 | Controller API and operator controls | W4 |
| R11 canonical wrap | D7 | stage table Wrap row | W3, W6 |
| R12 failure injection | D2, build order | W0 fixture; W2 acceptance rerun | W0, W2 |
| R13 no self-tuning | Non-goals | W6 exit criteria | W6 |
| R14 kernel vocabulary generic | Non-goals, Consequences | W0/W5 exit criteria | W0, W5 |
| R15 trusted provenance | D3 | Immutable result contracts (adapters, executors) | W0, W3 |
| R16 effect safety | D5 | Prepare, verify, approve, publish, verify | W4 |
| R17 dispatch/projection safety | D1, D2 | Logical persistence and recovery contract | W0, W2 |
| R18 engine/profile seam | D2 | W2 seam rule and grep gate | W2 |

## Current OCP baseline

Reusable without changing the gateway wire or generic session schema:

- generic sessions, ordered rosters, and deterministic `trigger_ref` fields;
- `solo`, council, and one-pass `pipeline` session coordinators;
- durable per-bot outbox and reconnect replay;
- bot identity, admission, liveness, watchdog, and CAS transitions;
- `POST /v1/sessions`, `GET /v1/sessions/:id` with messages, session logs, SSE,
  and session listing/query APIs;
- controller `OpenSession` and `PostMessage` actions;
- best-effort session-close webhook as a low-latency wakeup.

Not provided today:

- a parent workflow, stage cursor, quality attempt, score, or artifact model;
- conditional branching or loop-back in `pipeline`;
- a typed SDD result field or exact result-message pointer;
- durable controller action/result replay after a child session has closed;
- atomic or independently idempotent session creation plus opening-prompt post;
- scoped machine auth for multiple external controllers;
- replayable persisted lifecycle events from ADR 017;
- neutral/plugin-supplied pipeline handoff text;
- content-writing (`contents:write`) GitHub tokens from OCP — the chair's
  plane-minted ceiling today is `pull_requests:write` (ADR 019).

The existing three-stage Pipeline integration test proves ordered handoff only.
It is a mechanism baseline, not acceptance evidence for this workflow.

## Versioned profile contract

The workflow definition is immutable once a run starts. A representative first
profile is:

```yaml
schema: openab.sdd.workflow/v1
name: software-development
version: 1
defaults:
  threshold: { operator: gt, score_x100: 900 }
  max_attempts: 3
  phase_timeout_seconds: 540
  budgets:                       # representative; W0 freezes the real values
    stage_wall_clock_seconds: 7200
    stage_token_budget: 500000
    run_wall_clock_seconds: 86400
    run_token_budget: 2000000
    max_side_effect_retries: 2
human_gates:
  - { after_stage: spec }
  - { before_irreversible_side_effect: ticket }
  - { before_irreversible_side_effect: dev }
  - { before_irreversible_side_effect: wrap }
stages:
  - key: discuss
    worker_role: facilitator
    reviewer_roles: [product_reviewer]
    rater_role: quality_rater
    rubric: { id: sdd-discuss, version: 1 }
  - key: explore
    worker_role: researcher
    reviewer_roles: [architecture_reviewer]
    rater_role: quality_rater
    rubric: { id: sdd-explore, version: 1 }
  - key: prototype
    worker_role: prototyper
    reviewer_roles: [ux_reviewer, technical_reviewer]
    rater_role: quality_rater
    rubric: { id: sdd-prototype, version: 1 }
  - key: spec
    worker_role: spec_author
    reviewer_roles: [architecture_reviewer, security_reviewer]
    rater_role: quality_rater
    rubric: { id: sdd-spec, version: 1 }
  - key: usage
    worker_role: product_writer
    reviewer_roles: [acceptance_reviewer]
    rater_role: quality_rater
    rubric: { id: sdd-usage, version: 1 }
  - key: ticket
    worker_role: planner
    reviewer_roles: [delivery_reviewer]
    rater_role: quality_rater
    rubric: { id: sdd-ticket, version: 1 }
  - key: dev
    worker_role: implementer
    reviewer_roles: [correctness_reviewer, security_reviewer]
    rater_role: quality_rater
    rubric: { id: sdd-dev, version: 1 }
    hard_checks: [tests, lint]
  - key: review
    worker_role: review_chair
    reviewer_roles: [correctness_reviewer, integration_reviewer]
    rater_role: quality_rater
    rubric: { id: sdd-review, version: 1 }
    hard_checks: [review_decision_approve, no_unresolved_blocker, reviewed_digest_match]
  - key: wrap
    worker_role: release_writer
    reviewer_roles: [release_reviewer]
    rater_role: quality_rater
    rubric: { id: sdd-wrap, version: 1 }
```

The controller snapshots the effective OCP watchdog for its target deployment.
It rejects a profile unless
`phase_timeout_seconds < OABCP_SESSION_TIMEOUT_SECS`. The representative
540-second deadline fits the current 600-second default with a recovery margin;
raising it requires a coordinated OCP deployment override and a new profile
version.

Role names and human gates are profile policy. Runtime assignment resolves roles
to registered bot identities and snapshots the assignment. The same bot may
serve in several child sessions; under the default production policy the
rater differs from the producer and every `quality_review` participant (a
development profile may relax reviewer/rater overlap, never producer/rater
separation). A human gate is evaluated after the machine composite
predicate succeeds, and the final stage-advance gate waits for both. Human
approval does not replace review, rating, or hard checks.

| Stage | Canonical accepted artifact | Minimum evidence |
|---|---|---|
| Discuss | problem statement, goals, non-goals, constraints | unresolved questions and measurable success criteria |
| Explore | alternatives/research memo and recommendation | compared options, sources, assumptions, risks |
| Prototype | runnable or inspectable prototype revision | demo/smoke evidence, recorded shortcuts/learnings, and an explicit disposal/adoption decision |
| Spec | versioned technical specification | interfaces, state, failure, security, migration, observability, tests |
| Usage | validated usage contract with executable acceptance criteria plus any superseding Spec revision | happy path, edges, permissions, failure UX, Spec consistency |
| Ticket | dependency-ordered tasks | acceptance-criterion mapping, owner, definition of done |
| Dev | code or PR revision | tests, lint, security/policy checks, exact digest |
| Review | reviewed release-candidate manifest (code revision + verdict/findings) | approve decision, no unresolved blocker, reviewed code digest matches candidate |
| Wrap | canonical specs/docs plus release/deploy/closeout revisions | archive/sync proof, release checks, runbook, limits, follow-up ownership |

The exact scoring dimensions and weights live in the versioned rubric. A
headline number without required evidence cannot pass. Prototype and Usage have
explicit artifacts because the surveyed SDD tools cover them inconsistently;
Wrap must update canonical truth rather than produce only a summary.

## Artifact manifest and forward supersession

Every stage consumes and produces an immutable
`openab.sdd.artifact-manifest/v1`. The manifest is the candidate that review
and rating bind to; artifact bodies remain in their canonical systems.

```json
{
  "schema": "openab.sdd.artifact-manifest/v1",
  "workflow_id": "wf_...",
  "stage": "usage",
  "attempt": 2,
  "input_manifest_digest": "sha256:...",
  "artifacts": [
    {
      "kind": "spec",
      "uri": "git:docs/spec.md",
      "revision": "abc123",
      "digest": "sha256:new",
      "supersedes": "sha256:old"
    },
    {
      "kind": "usage",
      "uri": "git:docs/usage.md",
      "revision": "abc123",
      "digest": "sha256:usage"
    }
  ]
}
```

The serialized manifest has its own `output_manifest_digest`. The accepted
output manifest of stage N becomes the exact input manifest of stage N+1. This
preserves simultaneous requirements, design, scenarios, tasks, code, and
finding dependencies instead of pretending that one artifact digest represents
the handoff.

Forward stages may revise an upstream artifact only where the versioned profile
declares it:

- Usage may supersede Spec when post-spec user/acceptance validation exposes an
  inconsistency. The Usage candidate then contains both the revised Spec and the
  usage contract.
- Review may supersede the Dev code revision when an approve/no-blocker hard
  check fails. A `request_changes` verdict drives the Review-stage `fix`
  phase; fresh code, review, and rating are required.

The superseding manifest retains the replaced digest, accepted event, and
lineage. It does not mutate or reopen the old stage. Arbitrary backward jumps
remain out of scope.

## Immutable result contracts

W0 freezes JSON Schemas and valid/invalid fixtures for:

- artifact manifest and adapter verification result;
- review result;
- rating result;
- trusted hard-check result/provenance;
- human approval, effect plan/result, and operator override event.

The rating result uses `openab.sdd.quality/v1` from ADR 022 and binds workflow,
stage, attempt, candidate-manifest digest, rubric id/version, `score_x100`,
trusted hard-check refs, and evidence. The controller, not the rater, computes:

```text
all(required hard checks pass)
AND score_x100 > 900
AND (human approval not required OR a valid approval is recorded)
```

`score_x100` is an integer from 0 through 1000. Therefore 900 (9.00) fails and
901 (9.01) can pass only if all hard checks and any configured human gate pass.
The controller ignores any agent-provided `passed` boolean.

The initial transport is a delimited, versioned machine block in the assigned
rater's settled message. After the child closes, the controller fetches the
session messages and requires exactly one valid block. Missing or duplicate
blocks, malformed/out-of-range values, wrong identity, wrong stage/attempt,
stale manifest digest, rubric mismatch, or absent evidence fail closed.
Reaction-only completion cannot advance a rating phase.

Rater identity is the authenticated OCP message author, not a JSON field. The
default production assignment requires a rater different from the producer and
all `quality_review` participants; a development profile may relax
reviewer/rater separation but never producer/rater separation.

Artifact adapters resolve immutable revisions, fetch the authoritative bytes or
provider content hash, compute/verify every artifact digest, and then compute
the manifest digest. Hard checks run through controller-owned executors or
verified CI/policy attestations and produce immutable records containing the
executor identity, command/policy version, subject manifest digest, result,
timestamps, and evidence digest. Agent messages can reference these records;
they cannot create an authoritative passing check or hash.

## Child-session identity and attempt semantics

Every agent phase has a stable logical identity and each intentional child
dispatch has a distinct identity:

```text
logical_phase_id = sdd:<workflow_id>:<stage>:<attempt>:<phase>
dispatch_try_id  = sdd:<workflow_id>:<stage>:<attempt>:<phase>:try:<infra_try>
trigger_ref      = <dispatch_try_id>
trigger_fingerprint = sha256(phase_definition || input_manifest_digest || assignment_snapshot)
```

The controller stores a fingerprint of the phase definition and expected input
manifest digest plus its assignment snapshot. Redelivery of the same dispatch
try must start-or-attach to the same child session. A conflicting fingerprint
fails and requires operator attention. Only an explicit policy transition
after the prior child is positively identified as terminal with a retriable
infrastructure failure increments `infra_try` and creates a new
`dispatch_try_id`. An outcome that remains unknown after reconciliation enters
`needs_attention`; it never creates a replacement child.

- Attempt 1 starts with `produce`.
- A failed rating closes attempt N.
- `fix` starts attempt N+1 from attempt N's candidate, immutable review, and
  rating feedback, producing a new candidate-manifest digest before fresh
  review and rating phases.
- Machine failure or explicit human rejection below the cap creates the next
  quality attempt. At the cap it enters `needs_attention` without dispatching
  a final useless `fix`.
- Machine pass with approval outstanding enters `waiting_for_approval` on the
  same attempt. Waiting, approval delivery retries, and approval timeout do not
  consume a quality attempt.
- A confirmed terminal pod failure, timeout, or malformed-output retry is an
  `infra_try` within the same business phase; it is not automatically a new
  quality attempt. A network outcome that remains ambiguous after
  reconciliation cannot be retried automatically.
- The accepted output-manifest digest of stage N is the recorded input-manifest
  digest of stage N+1.

One phase per child session avoids the current session-global monotonic done
votes, repeated-bot roster uniqueness, and workflow-length watchdog problem.

## Logical persistence and recovery contract

The records below are logical and live outside OCP's SQLite schema. W0 decides
their physical mapping:

- the explicit database baseline implements them as controller tables, with
  CAS transitions, append-only events, and an action outbox; or
- a durable runtime owns the cursor, replay journal, timers, and pending calls,
  while the controller keeps the queryable domain/audit projection not already
  provided by that runtime.

Exactly one mechanism owns workflow replay. A runtime-backed implementation
must not enqueue the same OCP call into both its durable journal and a custom
workflow outbox.

If a durable runtime wins, its journal is authoritative for execution cursor,
timers, and pending calls. The append-only domain event store is authoritative
for audit and projection rebuild. Each event has a deterministic
`(workflow_id, sequence)` id and is written through an idempotent durable call;
the read projection records a sequence watermark and is disposable/rebuildable
from those events. W0 injects crashes before and after the event write, durable
call acknowledgement, and projection update to prove that events are neither
lost nor duplicated.

### `workflow_runs`

- id, profile, immutable definition JSON/hash;
- trigger reference/fingerprint;
- state: `pending`, `running`, `paused`, `waiting_for_approval`,
  `publishing`, `verifying_effect`, `needs_attention`, `completed`, `failed`,
  or `cancelled`;
- current stage ordinal and CAS version;
- thresholds, limits, deadline/budget counters;
- input/final manifest refs and digests; timestamps.

### `stage_runs`

- workflow id, stage key/ordinal, state (including `waiting_for_approval`,
  `publishing`, and `verifying_effect` where applicable), current attempt;
- accepted attempt id;
- input/output manifest ref/digest and primary artifact/message refs;
- CAS version and timestamps.

Unique `(workflow_id, ordinal)`.

### `attempts`

- stage run id, attempt number, candidate manifest ref/digest;
- rater, rubric, rating message id, `score_x100`;
- hard checks, evidence, gate result/reason, optional override id;
- state and timestamps.

Unique `(stage_run_id, attempt_no)`. A stored valid rating is immutable.

### `phase_runs`

- attempt id, phase, `infra_try`;
- assigned bots/roster snapshot and expected input-manifest digest;
- logical phase id, dispatch-try id, child OCP session id;
- output message/artifact refs, status, error, timestamps.

Unique `(attempt_id, phase, infra_try)` and unique action id.

### `workflow_events`

Append-only sequence, event id, workflow/stage/attempt/phase/session ids, kind,
actor, payload, causation/idempotency keys, and timestamp. Events include stage
start/accept, phase dispatch/complete/fail, rating record, gate evaluation,
retry, operator control, effect prepare/approve/publish/verify, escalation, and
workflow completion.

### `effect_runs`

- workflow/stage/attempt ids, immutable effect-plan ref/digest and candidate
  manifest digest;
- provider/adapter identity and version, operation/target, payload digest,
  expected precondition, reversibility, and provider idempotency key;
- approval id/actor/reason plus the approved manifest and effect-plan digests;
- state (`prepared`, `waiting_for_approval`, `publishing`, `published`,
  `verified`, `reconciling`, or `failed`), provider result/revision,
  verification evidence, error, and timestamps.

An effect may enter `publishing` only from an exact-digest approval. A changed
candidate manifest, payload, target, adapter version, or precondition creates a
new effect plan and invalidates the old approval.

### Durable actions (`action_outbox` in the custom baseline)

Deterministic action/dispatch id, workflow id, action kind/payload and payload
fingerprint, state (`pending`, `leased`, `acknowledged`, `dead`), try
count, lease owner/epoch/expiry, next-attempt time, acknowledgement/result
identity, last error, and timestamps.

In the custom baseline, a domain state mutation, its event, and its next outbox
action commit in one controller transaction. In a runtime-backed design, a
journaled/durable call must provide the equivalent crash boundary; the domain
projection stores its invocation/action id so operators can reconcile it to the
linked OCP session.

The custom dispatcher is at-least-once. Lease takeover uses an epoch/fencing
check so a stale worker returning after expiry cannot acknowledge or advance
the action. OCP/provider idempotency or read-after-write reconciliation remains
required; the outbox alone cannot make an external effect exactly once.

## Required recovery semantics

The custom baseline implements these semantics as a level-driven reconciler. A
runtime-backed implementation may express them through deterministic replay,
durable calls, timers, and signals, but must produce the same observable result.
For every non-terminal workflow:

1. Load the immutable definition, cursor, current phase, and pending durable
   action.
2. If no phase is claimed, durably claim exactly one next phase and record its
   action before dispatch.
3. Dispatch or redrive the action using its deterministic identity.
4. If the child session is active, wait.
5. If it is closed, fetch messages and validate/store the exact phase result.
6. Durably advance phase/attempt/stage state, append the domain event, and
   record the next action without a lost-work window.
7. Redrive unacknowledged actions with bounded backoff.

| Observed state | Reconciliation |
|---|---|
| phase claimed, action not dispatched | dispatch the existing durable action |
| dispatch response lost | find/link the child by stored id or deterministic trigger |
| child active | wait; never create a second child |
| child closed, result absent | parse/store once or mark the phase failed |
| result stored, cursor not advanced | replay/rerun the guarded transition |
| gate failed below cap | start attempt N+1 with `fix` |
| gate failed at cap | enter `needs_attention` |
| machine gate passed, approval required | enter/remain `waiting_for_approval`; dispatch no `fix` |
| approval rejected below cap | start attempt N+1 with approval feedback |
| approval rejected at cap | enter `needs_attention` |
| stale controller version | discard computation and reload/replay |

Webhook/SSE events may wake a run. Correctness must be unchanged if all wakeups
are duplicated, reordered, or lost.

## Controller API and operator controls

These endpoints belong to `sdd-controller`, not OCP:

```text
POST /v1/workflows
GET  /v1/workflows
GET  /v1/workflows/:id
GET  /v1/workflows/:id/events?after_seq=...
POST /v1/workflows/:id/control
```

Create takes an idempotency key, profile, trigger identity, input artifact, and
approved limit overrides. Read returns the cursor, stages, attempts, phases,
linked OCP sessions, quality evidence, and artifact chain.

`control` accepts only `pause`, `resume`, `retry_current`, `approve_current`,
`force_pass_current`, and `cancel`. It requires an idempotency key, expected CAS
version, authenticated actor, and reason. A stale version returns `409`.
`approve_current` targets exactly one `stage_gate` or `effect_plan`. Its payload
binds the current attempt and candidate-manifest digest; an effect approval also
binds the effect-plan digest. It releases only that named gate. A stale or
different target returns `409` and cannot be reused. `force_pass_current`
requires a schema-valid, digest-bound rating and every required hard check to
pass; it preserves the failed rating and adds an immutable override of only the
numeric score threshold. It cannot create an approval, authorize a publish, or
bypass any hard check. `retry_current` is rejected until the current child is
confirmed terminal with a retriable infrastructure failure; ambiguity alone is
not retry authorization. There is no raw `advance_stage` operation. `cancel`
durably prevents new dispatch and marks later child results ignored; current
OCP has no generic session-cancel endpoint.

## Prepare, verify, approve, publish, verify

Human approval must precede the irreversible effect it authorizes. Agent
`produce` and `fix` phases therefore prepare candidates only in a
workflow-owned draft namespace: an isolated workspace/branch, draft tracker
object, or dry-run adapter. Draft writes still use deterministic ids, but cannot
merge, release, deploy, notify users, or update canonical truth.

For a stage with external effects, the controller runs this state sequence:

```text
prepare candidate
  → verify manifest + hard checks + rating
  → wait for approval bound to manifest and effect-plan digests
  → publish through a trusted adapter
  → read-after-write verify the provider result
  → record effect result and accept stage
```

The immutable `effect_plan` includes provider/adapter version, operation,
target, payload digest, candidate-manifest digest, idempotency key, expected
precondition/version, and reversibility. The approval includes the exact
`effect_plan_digest` and `candidate_manifest_digest`; changing either
invalidates it. The effect result records provider identity, returned revision,
verification evidence, and reconciliation status.

Ticket prepares task objects before creating tracker issues. Dev prepares an
immutable code revision before merge or release. Wrap prepares canonical
spec/docs/release deltas before publishing or deploying them. Irreversible
credentials belong only to trusted publish adapters, not producer/reviewer/
rater pods. W1–W3 use fake or dry-run adapters; W4 is the first slice allowed to
enable one real publish effect after its idempotency and read-after-write tests
pass.

## OCP integration and hardening gates

The first vertical slice uses the current north API and makes no OCP schema or
wire changes. The controller records every returned child session id and
reconciles against `GET /v1/sessions/:id`.

Before production automation, test these generic gaps and land only the ones the
slice demonstrates as load-bearing:

1. **Durable action-result replay.** An action id must return the original child
   session even when the response was lost and that session later closed.
2. **Idempotent opening prompt.** Today session creation can commit before the
   opening prompt is posted; retry dedupes the active session without repairing
   the missing prompt. Make create+prompt atomic or independently redrivable.
3. **Exact result-message identity.** Prefer a generic settled result pointer
   over selecting `latest_settled`; until then the controller requires exactly
   one valid typed block and fails closed.
4. **Scoped controller auth.** Replace the shared north bearer with a machine
   identity whose permitted session actions are explicit.
5. **Durable OCP lifecycle events.** ADR 017 replay improves wakeup/audit. It is
   useful but polling remains the recovery fallback.
6. **Configurable pipeline handoff.** If a phase uses `pipeline`, replace the
   hardcoded “continue the review” text with caller/profile-supplied wording
   while retaining compatibility for existing review sessions.

Writing agents initially use credentials outside OCP's trust boundary. Moving
write tokens into OCP requires the ADR 018/019 requested-scope design and a real
second writing consumer. External side effects must use provider idempotency keys
or a read-after-write reconciler before automatic retries are enabled.

## Delivery slices

Each slice is independently reviewable and leaves review/triage behavior
intact. The write-capable Fix/Dev stage is deliberately outside W0–W6: it
unlocks only with the ADR 018/019 requested-scope token work and a real
second writing consumer (ADR 022 build-order step 8).

### W0 — Freeze contracts and provisionally select the durable substrate

Deliver:

- accept ADR 022 and the prior-art disposition; select controller repo, owner,
  deployment, language, and auth model;
- versioned workflow/artifact/review/rating/override schemas and fixtures;
- state-transition table, error taxonomy, stage rubrics, and prompt ownership;
- per-stage human-calibration sample and false-pass/false-block thresholds;
- side-effect inventory and dry-run policy;
- one OCP-conformance mock and crash fixture implementing the
  [runtime selection experiment](sdd-quality-workflow-prior-art.md#7-w0-runtime-selection-experiment);
- a deterministic fake publish adapter in that fixture so effect-plan approval,
  lost provider responses, read-after-write reconciliation, and effect-result
  journaling are tested without changing canonical external state;
- conformance cases for active-only create dedupe, duplicate create after
  completion and after configured retention expiry, matching/conflicting
  fingerprints, a child closing before lost-response reconciliation, multiple
  closed children for one logical phase, and the missing-opening-prompt window;
- a time-boxed Restate implementation and explicit database/reconciler/outbox
  baseline of the same one-stage `Spec` workflow;
- a provisional selection record with exact Restate server/SDK versions,
  retention/retry policy, measured failure behavior, two-service/PV operations,
  backup/restore, upgrade/rollback, implementation size, and rejected
  alternatives.

Exit:

- fixtures prove 900 fails and 901 passes only with all hard checks and any
  configured human approval;
- stale manifest digest, wrong stage/attempt/rater, missing evidence, and
  duplicate result blocks fail closed;
- the provisional substrate passes every case in the prior-art §7 runtime
  selection experiment table — including lease/fencing races, command
  early/late/duplicate/reorder, retention expiry, and observability — or W0
  stops with no recommendation;
- exactly one journal/outbox mechanism owns replay;
- no SDD column, coordinator mode, or gateway field enters OCP.

### W1 — Disposable north-API mechanism probe

Deliver a small two-stage probe using existing OCP endpoints and deterministic
mock bots. It emits typed results from day one and polls rather than trusting the
close webhook.

The probe may additionally run one real, low-risk workload — for example a
read-only capability spec — so the POC produces real value while its code
stays disposable. Mock bots remain mandatory: they prove the deterministic
boundary cases in the exit criteria below; the real workload proves the loop
is worth automating. The addition changes none of the exit criteria.

Exit:

- stages execute in order;
- one stage follows fail → fix → rerate → pass;
- accepted output-manifest digest N becomes input-manifest digest N+1;
- observed OCP boundary gaps are recorded with crash points and evidence;
- the W0 recommendation passes the real-OCP lost-response, closed-child,
  duplicate-trigger, conflicting-fingerprint, and missing-opening-prompt cases;
- the final substrate record is accepted or selection is reopened; W2 cannot
  start on a provisional result;
- probe code is not treated as the production controller.

### W2 — Durable one-stage controller

Deliver the W0/W1-selected substrate, immutable run definition, logical
run/stage/attempt/phase records, append-only domain events, durable actions,
read/control API, operator deployment/runbook, and one `Spec` quality loop.

- If Restate wins, deliver the versioned workflow handlers, durable calls,
  signals/queries, domain query/audit projection, server persistence,
  registration, backup/restore, and upgrade/rollback procedures.
- If the explicit database baseline wins, deliver migrations/repositories, CAS
  transitions, workflow events, action outbox, leases/timers, and reconciler.

Until OCP offers durable action-result replay, an ambiguous dispatch is queried
to convergence or escalated; it is never blindly reissued merely because the
HTTP response was lost.

In either branch, keep the engine/profile seam (ADR 022 D2): generic
run/stage/attempt/dispatch/recovery modules contain no SDD vocabulary; stage
names, rubrics, and score semantics enter only through the profile package.

Exit:

- restart/crash injection at claim, dispatch, child-open, child-close,
  result-store, and cursor-advance boundaries causes no duplicate business phase;
- the full W0 failure-injection suite (including lease/fencing races, command
  reordering, and retention expiry) reruns against the production controller
  as its acceptance gate;
- pass-first, fail-fix-pass, malformed result, timeout, stale manifest digest,
  wrong rater, and attempt exhaustion are covered;
- cap exhaustion enters `needs_attention` with the last evidence;
- a grep gate proves the generic half is SDD-free (no stage name, rubric id,
  or score field outside the profile package) — same mechanism as the OCP
  kernel-purity CI gate.

### W3 — Nine-stage profile and artifact chain

Deliver all stage definitions, prompts, rubrics, assignment policy, artifact
adapters, deterministic Dev/Review hard checks, human-gate policy, budgets,
fake/dry-run Ticket/Dev/Wrap publish adapters, canonical Wrap delta planning,
and full run rendering.

Exit:

- mock-bot integration completes all nine stages in declared order;
- at least one stage uses the retry edge;
- every accepted output-manifest digest equals the next stage's input-manifest
  digest;
- the Spec checkpoint and all side-effect approvals pause/resume durably;
- Wrap's dry-run effect plan names the exact canonical updates, while no real
  external publish occurs;
- one logical parent completion is recorded once.

### W4 — Operator and side-effect safety

Deliver deadlines/cost counters, pause/resume/retry/approval/force-pass/cancel,
metrics and runbook, effect-plan/approval binding, and the first real provider
adapter with idempotency plus read-after-write reconciliation.

Exit:

- limits never spin indefinitely;
- every override preserves rejected evidence plus actor/reason;
- duplicate delivery cannot duplicate Ticket, Dev, or Wrap side effects;
- an approval for an old manifest/effect-plan digest cannot authorize a new
  publish, and every real effect has a verified provider result;
- support can reconstruct a run without scraping an agent chat.

### W5 — Evidence-driven OCP hardening

Implement only the generic OCP items proven necessary by W1–W4, starting with
durable action-result/opening-prompt idempotency. Keep stock OAB wire fixtures and
existing review, triage, solo, council, and pipeline tests green.

Exit:

- each landed primitive has a second-consumer-style test independent of SDD
  vocabulary;
- no SDD stage/attempt/score enters `SessionState` or generic store columns;
- recovery remains correct across controller and OCP redeploys.

### W6 — Dev-lane dogfood and production decision

Run the full profile manually in the dev lane, initially with Ticket/Dev/Wrap
side effects dry-run. Collect completion, override, cost, latency, false-pass,
false-block, human-versus-rater calibration, and downstream-adoption evidence by
stage/rubric.

W0 freezes the initial per-stage calibration policy. Unless a stage-specific
policy is stricter, production automation requires at least 30 independently
human-adjudicated candidate manifests for that rubric version, including at
least 10 known-fail cases; machine/human pass-decision agreement at least 85%;
false-pass rate at most 5%; false-block rate at most 20%; and zero bypasses of a
critical deterministic hard check. These are operational go/no-go thresholds,
not a claim of statistical certainty.

Exit:

- restart recovery is observed live, not only in tests;
- no skipped gate, duplicate side effect, or orphan active child session;
- each automatically enabled stage meets its frozen labeled-sample thresholds;
  insufficient samples are `unknown` and keep that stage human-gated/manual;
- humans review every rubric/threshold change; the workflow never tunes itself;
- production enablement has an explicit go/no-go, rollback owner, and credential
  decision.

## Verification matrix

### Functional and gate integrity

- nine stage keys execute in declared order;
- pass on first attempt and fail → fix → rerate → pass;
- max attempts enters `needs_attention`; cancellation prevents new dispatch;
- score 900 fails, 901 passes, and a failed hard check blocks score 1000;
- a required human gate blocks score 1000 until a valid approval is recorded;
- malformed score, wrong binding, stale manifest digest, unassigned/self rater,
  missing evidence, duplicate typed block, and reaction-only completion all
  block;
- force-pass is a distinct immutable override and never implies publish
  approval or bypasses a required hard check;
- stage and effect approvals target the exact current attempt and manifest;
  effect approval additionally targets the exact effect-plan digest.

### Durability and idempotency

- duplicate create/control/result signals have one domain effect;
- crash at every transaction/dispatch boundary resumes without a duplicate
  quality attempt or external side effect;
- webhook loss, duplication, and reordering do not change correctness;
- startup reconciliation handles active, closed, timed-out, and missing child
  sessions;
- selected-substrate migration/versioning, backup/restore, and rollback
  procedures are exercised;
- completed-workflow and idempotency-key retention are exercised beyond their
  horizon; expiry cannot silently create a conflicting duplicate run;
- runtime-backed delivery and a custom workflow outbox are never both active for
  the same logical OCP action.

### Artifact, compatibility, and security

- accepted output-manifest digest N equals input-manifest digest N+1; stale
  evidence cannot unlock it;
- canonical bodies remain in artifact systems, not OCP or controller events;
- Wrap's stored revisions prove accepted deltas reached the canonical sources;
- stock OAB wire fixtures and existing OCP profiles remain unchanged;
- operator controls require an authenticated actor and retain a reason;
- bots receive only capabilities required by their phase;
- no token, private key, environment dump, or bulk private artifact enters logs
  or events.

## Rollout and rollback

1. Run deterministic mocks locally with all side effects disabled.
2. Enable a manual `Spec`-only run in the dev lane.
3. Enable the full profile with Ticket/Dev/Wrap adapters in dry-run mode.
4. Exercise controller and OCP redeploys plus lost-webhook recovery.
5. Enable a real side effect only after its idempotency/reconciliation test.
6. Review collected evidence and explicitly approve any production trigger.

Rollback pauses new controller claims, lets active OCP child sessions finish,
and ignores their late results against the cancelled/paused parent version. It
preserves all workflow/attempt/event records. A restart resumes from the
selected substrate's durable cursor and pending-action journal/outbox; rollback
never rewrites a rating or deletes history. If live child termination later
proves necessary, it is a separately authorized generic OCP W5 primitive with
race tests, not an assumed current capability.

## Scope limits

Not in the first release:

- arbitrary DAGs, parallel joins, compensation graphs, cron, or a marketplace;
- automatic editing of prompts, rubrics, thresholds, definitions, or rosters;
- storing repository trees or document bodies in OCP/controller databases;
- arbitrary reopening of accepted old stages;
- a workflow UI or multi-tenant control plane;
- changing the OAB gateway protocol or agent session pool.

These limits do not remove a requested stage or quality loop. They keep the
first implementation focused on making this exact profile durable and auditable.
