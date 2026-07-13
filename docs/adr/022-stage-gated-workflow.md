# ADR 022 — Stage-gated SDD workflow: durable controller outside, OCP sessions inside

Status: proposed · 2026-07-13

## Context

The proposal under evaluation is a quality-driven staged development workflow
("SDD"): nine sequential stages — Discuss → Explore → Prototype → Spec → Usage →
Ticket → Dev → Review → Wrap — where every stage is wrapped in its own quality
loop (Review → numeric Rating → Fix) that exits only when `quality_score > 9`.
This ADR decides which parts of that belong inside OCP and which must not.

What the plane already provides (verified against current code):

- **Arbitrary-content sessions.** `POST /v1/sessions` opens a session with any
  prompt text — a spec document or a prototype description reviews as readily
  as a diff (`src/api.rs`). The S16 second-consumer proof
  (`tests/second_consumer.rs`) drives a complete loop over the north surface
  alone: open → deliver → verdict → close.
- **A sequential coordinator.** Mode `pipeline` hands off stage0 → stageN
  inside one session, roster order = stage order (`src/coordinator.rs`). It is
  forward-only and single-pass: no loop-back, no per-stage gate, one hardcoded
  handoff prompt.
- **An external fix → re-review loop, already in production.** A push or
  `/review` supersedes the active round and opens a fresh session with
  delta-since-SHA context, bounded by `OABCP_REVIEW_ROUND_BUDGET` (default 10,
  all paths); pushes are additionally rate-limited by `OABCP_REVIEW_HOURLY_CAP`
  (default 3, `synchronize` only — explicit commands bypass it).
- **Consumable outcomes.** The ADR 012 close webhook, `GET /v1/sessions`
  polling, and per-session SSE all carry the structured verdict columns.

What it deliberately lacks:

- **No numeric score.** The structured verdict is `approve|request_changes`
  plus red/yellow/green counts (ADR 013); no 0–10 rating exists in the schema
  or the trailer grammar.
- **The plane never branches on verdict content.** The `run_actions` close arm
  parses the trailer into the ADR 013 columns, emits, and cleans up — it
  records verdict content but never branches on it ("side-effects are the
  app's job"), and design.md pins B4: verdict content remains a
  steering/controller responsibility, not a kernel guarantee. (ADR 013 §5 pins
  the adjacent GitHub-I/O half: the plane never calls GitHub.) A
  "score ≤ 9 → loop back" branch has no sanctioned home in the kernel.
- **No loop primitive.** Coordinator actions are Relay/Prompt/Transition/Close;
  the `Goal` enum (`Rounds(k)`, `AllStages`) was proposed and cut
  (`docs/coordinators.md`). Pipeline cannot move backward.
- **No session chaining.** Close fires one fire-and-forget webhook; nothing in
  the plane opens a successor session.
- **No content-writing token today.** ADR 019: the chair's current write ceiling
  is `pull_requests:write`; OCP does not mint `contents:write`.
- **A 600 s watchdog anchored to `created_at`.** Any long-running multi-stage
  session is force-closed regardless of progress
  (`OABCP_SESSION_TIMEOUT_SECS`).

## Prior-art findings

The companion
[prior-art study](../sdd-quality-workflow-prior-art.md) compares durable
workflow engines, agent orchestration frameworks, spec-driven development
methods, and evaluator reliability research. Its conclusions constrain this
decision:

- Temporal, Restate, DBOS, and Step Functions all separate durable workflow
  progress from at-least-once external activities. A custom controller must
  prove equivalent replay, idempotency, signal, and recovery behavior; naming a
  table `action_outbox` is not sufficient.
- Anthropic's evaluator–optimizer, LangGraph, Google ADK, AutoGen, CrewAI, and
  the OpenAI Agents SDK validate bounded refinement loops and resumable state,
  but none makes an agent's free-form approval or score a trustworthy durable
  product gate.
- Spec Kit, Kiro, and OpenSpec treat versioned artifacts and cross-artifact
  consistency as the work product. Kiro contributes explicit human review
  points; OpenSpec contributes a sync/archive step that makes accepted changes
  canonical.
- Evaluator research finds self-grading and LLM-judge bias. A numeric score can
  participate in routing only when bound to the exact artifact and combined
  with independent review, deterministic evidence, and configured human
  approval.

The study does **not** select a controller runtime from documentation claims.
W0 must run the same failure-injection fixture against Restate and the minimal
database/reconciler baseline. DBOS remains conditional on accepting a
non-Rust controller; Temporal is the semantic reference and later-scale option.

## Decision

**The SDD loop is an application built ON the plane, not a workflow engine built
IN its kernel.** A durable external `sdd-controller` owns the parent run, stage
sequencing, quality attempts, score gate, artifacts, and recovery. OCP executes
the agent phases as child sessions and continues to own identity, delivery,
liveness, session CAS, and in-session coordination.

This is the controller/coordinator split already implied by ADR 007 and ADR 008:
cross-session product policy belongs to a controller; `solo`, council, and
`pipeline` coordinators remain session-local building blocks.

### D1 — One child session per agent phase, never one long workflow session

Every `produce`, `quality_review`, `rating`, and `fix` phase is a fresh child
session opened through the north API. The whole workflow is a sequence of linked
sessions, not one `pipeline` session spanning nine stages.

Typical local modes:

| Phase | OCP child-session mode | Purpose |
|---|---|---|
| `produce` | `solo` or `pipeline` | Create the first candidate artifact |
| `quality_review` | `council` | Produce evidence and defects for one candidate-manifest digest |
| `rating` | `solo` with an independent rater | Emit the typed quality assessment |
| `fix` | `solo` or `pipeline` | Produce a new artifact revision from the failed attempt |

Four existing contracts force the child-session shape:

- the watchdog force-closes on a `created_at` anchor — a workflow that runs
  longer than the timeout dies mid-stage;
- "fresh session after close" is the pinned consumer contract (ADR 018
  residue); only Solo reopens, and reopened sessions inherit the stale
  watchdog anchor;
- session-global monotonic done votes cannot represent fresh review rounds; and
- roster membership is unique by bot, while a workflow must reuse workers and
  reviewers across stages.

Each phase has a stable logical identity, while each intentional infrastructure
redrive has a distinct dispatch identity:

```text
logical_phase_id = sdd:<workflow_id>:<stage>:<attempt>:<phase>
dispatch_try_id  = sdd:<workflow_id>:<stage>:<attempt>:<phase>:try:<infra_try>
trigger_ref      = <dispatch_try_id>
trigger_fingerprint = sha256(phase_definition || input_manifest_digest || assignment_snapshot)
```

Attempt 1 starts with `produce`. After a valid failed gate, attempt N+1 starts
with `fix`, then gets a fresh `quality_review` and `rating`. Redelivery of the
same infrastructure try reuses its `dispatch_try_id` and `trigger_ref`; a
confirmed retriable terminal failure may create the next `infra_try` of the same
logical phase, never a new quality attempt. An ambiguous try never creates a
replacement child. If read-after-write
reconciliation cannot identify its outcome, the phase enters `needs_attention`.
The controller increments `infra_try` only after the prior child is positively
identified as terminal with a retriable infrastructure failure — a confirmed
pod failure, a watchdog timeout, or a settled child whose typed result is
missing or malformed (a phase-output failure redrives the same logical phase;
it is not a new quality attempt). This prevents
an intentional redrive from being mistaken for an idempotent replay of the
original call or duplicating successfully completed but temporarily unobserved
work.

This identity scheme holds only on the generic `POST /v1/sessions` path, where
the caller owns `trigger_ref`. If a stage instead delegates to the PR-review
plugin (its artifact is a PR), identity is plane-owned and per-PR
(`github:pr/<repo>#<num>`): at most one SDD stage may drive reviews of a given
PR at a time, and the plane's per-PR round budget is an outer bound the D6 caps
must sit inside.

### D2 — Sequencing and the gate live in `sdd-controller`; durability is selected by evidence

The controller is a service backed by a durable execution substrate, not a
one-shot shell script and not a close-webhook-only consumer. Its logical
workflow is:

```
for stage in [Discuss .. Wrap]:
  candidate = run(produce, prior accepted manifest)
  for attempt in 1..=max_attempts:
    evidence = run(quality_review, candidate.manifest_digest)
    rating = run(rating, candidate.manifest_digest, evidence)
    if !machine_quality_predicate_passes(rating):
      if attempt == max_attempts:
        enter needs_attention
        stop stage
      candidate = run(fix, candidate, evidence, rating)   # opens attempt N+1
      continue
    if human_gate_required(stage):
      approval = durable_wait_for_human(candidate.manifest_digest)
      if approval.rejected:
        if attempt == max_attempts:
          enter needs_attention
          stop stage
        candidate = run(fix, candidate, evidence, rating, approval.feedback)   # opens attempt N+1
        continue
    accept(candidate)
    break
```

A pending human gate suspends the same attempt; it never falls through to
`fix` and never consumes an attempt. Only an explicit rejection with feedback
may create the next quality attempt. Approval timeout or delivery failure enters
an operator-visible wait/escalation state rather than manufacturing a quality
failure.

The controller durably records an immutable workflow definition, current
cursor, stages, attempts, phase/session links, artifact refs/digests, ratings,
and audit events. It must durably couple a state transition with the next
logical OCP action:

- in the explicit database design, the materialized state, append-only event,
  and action-outbox row commit in one transaction; or
- in a selected durable runtime, its journal/checkpoint and durable-call
  primitive must provide the equivalent recovery contract.

Do not duplicate a runtime journal with a second custom workflow outbox unless
a named business-level external effect requires it. In either implementation,
dispatch uses the deterministic dispatch-try id and ambiguous outcomes
reconcile against OCP before any state transition. If a durable runtime's
journal is authoritative for execution, the queryable domain/audit projection
beside it uses deterministic `(workflow_id, sequence)` event ids, an explicit
authority/watermark, and a tested replay/rebuild path (prior-art R17): the
projection is disposable, the events are not.

Whichever substrate wins, the controller's own code keeps an internal
engine/profile seam — the ADR 018 kernel/plugin discipline applied one layer
up. The generic half (run/stage/attempt/phase records, events, dispatch,
recovery) must contain no SDD vocabulary; stage names, rubric ids, and score
semantics enter only through the profile half. The seam costs nothing now and
is what makes a reusable engine extractable later. Extraction itself is
deferred until a second real workflow product needs it (work-management
automation flows are the nearest candidate); that extraction re-runs
build-vs-buy against accumulated W-slice crash/cost evidence rather than
settling it today.

The W0 selection spike compares a Restate-backed controller with the explicit
database/CAS/event/outbox baseline using identical crash tests. Restate is the
lead engine candidate because its HTTP-facing workflow model is close to OCP,
but it still deploys a Restate Server, persistent storage, and a separate
controller handler service. Its actively developing Rust SDK, retention,
independent crashes/upgrades, backup/restore, signals, queries, and operational
behavior must pass the experiment. W0's recommendation remains provisional
until W1 confirms current OCP boundary behavior against a real instance. This
ADR does not pre-approve a workflow dependency.

The plane's boundary does not move: it still never decides whether a product
artifact is good, and no stage names or backward transitions enter the generic
session state machine. The close webhook may wake recovery, but the selected
durable state/journal plus OCP session queries are the correctness source.
Because the webhook is best-effort (ADR 012), the controller MUST query/reconcile
`GET /v1/sessions?trigger_ref=` and must remain correct if every webhook is
lost. Note the polling surface: session list rows carry only state plus the
structured columns — the typed quality result (D3) is read from the child
transcript via `GET /v1/sessions/:id`, never from a list row.

### D3 — Use a typed, digest-bound quality result from the first slice

Do not parse a free-form `score: N/10` line and do not add an SDD `q=` field to
the PR-review verdict schema. The rater emits a versioned machine block inside
its otherwise human-readable final message. The controller reads the child
session transcript/verdict and accepts exactly one block matching the expected
workflow, stage, attempt, candidate-manifest digest, rubric, and assigned rater.

Representative payload:

```json
{
  "schema": "openab.sdd.quality/v1",
  "workflow_id": "wf_...",
  "stage": "spec",
  "attempt": 2,
  "artifact_manifest_digest": "sha256:...",
  "rubric": { "id": "sdd-spec", "version": 3 },
  "score_x100": 950,
  "hard_check_refs": ["check_acceptance_criteria_..."],
  "evidence": ["msg_..."],
  "summary": "..."
}
```

`score_x100` is an integer in `0..=1000`; `950` means 9.50/10. The controller
computes the default composite stage-advance predicate itself:

```text
all(required hard checks pass)
AND score_x100 > 900
AND (human approval not required OR a valid approval is recorded)
```

The strict `>` is part of the profile contract: 9.00 fails and 9.01 passes.
Missing/malformed fields, a wrong identity, stale manifest digest, or mismatched
stage/attempt fail closed. A reaction-only done-signal cannot complete a rating
phase; the rating session must contain a settled typed result.

The gate threshold and rubric are immutable controller configuration snapshotted
into each run, never OCP session columns. Rater identity comes from the
authenticated OCP message author and must equal the snapshotted assignment; a
self-declared JSON identity is not authoritative. The default production policy
requires the rater to differ from the producer and every quality reviewer.

Executable tests, lint, security, and policy checks join the subjective score as
hard gates where a stage supports them. Those results come from
controller-trusted executors or verified CI/policy attestations bound to the
candidate-manifest digest. The rater may reference their immutable ids but
cannot assert their result. Likewise, a trusted artifact adapter resolves each
immutable revision and computes its digest; producer- or rater-reported hashes
are only claims until verified.

The score is a routing input rather than ground truth: self-evaluation tends to
be overly positive, and independent LLM judges still require calibration
against human judgment. Human checkpoints remain configurable by stage.

Per ADR 021, a score is an exit condition and evidence, not an optimization
target. The workflow never edits its own threshold, rubric, prompt, or evaluator
assignment in response to observed scores.

### D4 — Chain versioned artifact manifests, not one mutable artifact

Canonical artifact bodies remain in Git, an issue tracker, object storage, or
another profile-declared system. The controller stores a versioned manifest of
their kinds, URIs/revisions, cryptographic digests, relationships, and short OCP
message/evidence references.

One digest cannot represent a real SDD handoff: a stage may depend on the
problem statement, requirements, design, acceptance scenarios, task plan, code,
and findings at once. Each stage therefore accepts an `input_manifest_digest`
and produces an immutable `output_manifest_digest`. The accepted output
manifest of stage N is exactly the input manifest of stage N+1. Review and
rating bind to the complete candidate-manifest digest, so stale evidence for
any member cannot unlock a newer bundle.

A profile may declare a **forward supersession** inside the currently active
stage. For example, Usage validation may supersede a flawed Spec revision, and
the Review-stage fix phase may supersede the Dev code revision before another
review. The new manifest names each replaced digest with `supersedes`; earlier
acceptance events remain immutable. This is not arbitrary reopening of an old
stage: only the active stage's declared fix policy can create the replacement,
and the entire new manifest must be reviewed and rated again.

### D5 — Fix and Dev stages stay outside the plane's identity model

Stages that prepare fixes or code do not get plane-minted write tokens.
ADR 019's governing principle holds: the current plane does not issue
`contents:write`, and the named unlock — a second GitHub-writing plugin with
requested-scope tokens validated against coordinator policy (ADR 018 residue) —
is the exit trigger, not this ADR.

Before the side-effect-safety slice, Fix and Dev agents may write only to a
workflow-owned draft workspace/branch or fake adapter. An irreversible
Ticket/Dev/Wrap effect is a separate, journaled
`prepare → verify → approve → publish → read-after-write verify` controller
action bound to the exact candidate-manifest and effect-plan digests: the
manifest, hard checks, and rating are verified before approval is requested,
and the provider result is verified after publish. Its trusted adapter credential is
provisioned and audited outside the plane; producer, reviewer, and rater pods do
not receive it. Moving that credential into OCP still requires the ADR 018/019
requested-scope design and a real second writing consumer.

### D6 — Every loop is bounded, and cap-exhaustion escalates

Each stage's quality and infrastructure loop carries independent attempt,
wall-clock, token/cost, and side-effect limits (prior-art R9; the attempt cap
mirrors the `OABCP_REVIEW_ROUND_BUDGET` pattern), and the workflow carries a
total budget. Per-phase wall-clock stays bounded by the OCP session watchdog;
stage- and run-level deadlines are controller counters, not new OCP state.
Hitting a cap escalates to a human with the last verdict attached; it never
silently passes the gate or silently retries forever. Score-gated loops
without bounds are runaway-cost machines — the round budget exists today for
exactly this reason.

The controller exposes only bounded, audited operations: `pause`, `resume`,
`retry_current`, `approve_current`, `force_pass_current`, and `cancel`. Every
operation carries an idempotency key, expected workflow version, authenticated
actor, and reason. `approve_current` must name whether it targets the current
stage gate or an effect plan and bind the exact attempt, candidate-manifest
digest, and, for an effect, effect-plan digest; it releases only that gate.
`force_pass_current` preserves the failed assessment and adds an immutable
override instead of rewriting history. It requires a schema-valid,
digest-bound rating and every required hard check to pass; it overrides only
the configured numeric score threshold. It cannot manufacture a human
approval, authorize an effect plan, or bypass any hard check. `retry_current`
may create a new infrastructure try only after the prior child is confirmed
terminal; an outcome that remains ambiguous requires reconciliation or
operator resolution, not a blind retry.

### D7 — Human checkpoints and canonical Wrap are profile contracts

The immutable workflow definition states which stages require explicit human
approval. Approval is requested only after the machine evidence, hard checks,
and score predicate succeed; stage advance waits for both. The first production
profile requires approval after Spec and before enabling any irreversible
Ticket, Dev, or Wrap side effect. Other checkpoints are profile policy, not new
OCP states.

Every stage has a typed canonical output. In particular, Prototype stores an
inspectable revision plus learnings, and Usage stores executable acceptance
criteria; neither may disappear into conversation history merely because common
SDD tools do not model it as a first-class artifact.

The Review stage gates the release-candidate manifest, not the prose quality of
a verdict in isolation. Its required hard checks include an approve decision,
no unresolved blocker, and an exact match between the reviewed code digest and
the candidate manifest. A well-written `request_changes` report is useful
evidence but cannot pass; its findings drive the Review stage's fix phase, which
supersedes the code revision and re-reviews it.

Wrap is complete only after accepted deltas are synchronized into the declared
canonical sources (specs, docs, release/deploy record, runbook, known
limitations, and follow-up tracker)
and their resulting revisions/digests are stored. A generated summary alone
does not close the parent workflow.

## Non-goals

- **A plane-internal workflow engine.** ADR 007 rule 5 pins "do not build a
  plugin platform before the current dogfood path is stable" (extraction only
  when hardcoded assumptions block packaging, installation clarity, or another
  real plugin); that caution applies doubly to a workflow platform.
- **A bundled `sdd_workflow` control plugin.** ADR 007 does sanction bundled
  control plugins, so rule 5 alone does not exclude that alternative. What
  does: the design.md plane-vs-steering test. The controller's loop is
  deterministic control flow with no judgment in it — it must hold even when
  bots misbehave only in the trivial sense that it never trusts bot output for
  anything but the typed result it validates itself. Nothing in it needs the
  kernel's delivery/liveness machinery, and pulling it in-process would make
  the plane's availability and upgrade cadence hostage to product-workflow
  churn. If a second workflow product appears and the controller's OCP-facing
  half proves generic, revisit — that is the exit trigger.
- **Reviving `Goal`/Debate.** A score-gated loop inside one session would need
  round state and backward transitions in the coordinator seam; nothing here
  justifies reopening what `docs/coordinators.md` deliberately cut.
- **An agent graph runtime in the first bake-off.** LangGraph could implement
  the external parent controller, but its Python/JS graph plus production
  checkpointer adds unused graph/agent surface without removing external-effect
  idempotency or storage operations for this fixed profile. AutoGen, ADK,
  CrewAI, and Agents SDK patterns still inform termination and manager
  ownership. Revisit LangGraph only for a real graph-shaped second consumer.
- **Auto-tuning the gate.** The threshold is edited by humans on evidence
  (ADR 021 stance). No component adjusts its own exit criteria from metrics.
- **Score as a KPI.** `quality_score` gates progression; it is not a target to
  maximize. ADR 021's Goodhart guardrails apply unchanged: a stage that
  "scores 10" by producing timid artifacts is the failure mode, and the
  severity-weighted, adoption-first lens stays primary.

## Consequences

### Positive

- The nine-stage workflow is expressible with zero kernel changes: rubric and
  steering text, plus the controller — no new mode, column, or wire field.
- Kernel purity holds: no verdict-content branching, no chaining verb, no new
  mode enters `src/` for this feature.
- Each phase is a normal session — queryable and auditable with existing OCP
  tooling, and bounded by controller limits plus OCP liveness/watchdog policy.

### Negative

- The controller is new durable state outside the plane. Whether W0 chooses a
  workflow runtime or explicit database machinery, it must survive restarts,
  dedupe wakeups, reconcile ambiguous OCP calls, and own workflow position.
  That is real code and operations in an app/ops repo, not here.
- Selecting Restate would add a server and SDK/version compatibility surface;
  selecting the custom baseline would make its CAS, journal/outbox, timers,
  commands, backup/restore, and observability our responsibility.
- Quality results live in the controller's database and child transcripts;
  they are invisible to `sessions` list queries by design. A first-class OCP
  result envelope is considered only if another consumer needs the same generic
  contract — never as SDD columns or PR-trailer keys.
- Two trust domains (plane-minted read tokens for councils; the Fix agent's
  own credentials) mean workflow-level attribution spans two audit trails.

### Neutral

- `pipeline` mode remains useful inside a `produce` or `fix` child session. It
  is not the parent stage engine, and no deprecation is implied.
- The SDD stage list (nine stages, threshold 9) is controller config; this
  ADR fixes the architecture, not the stage taxonomy.

## Migration / build order

Detailed slices live in `docs/sdd-quality-workflow-plan.md`; the order is:

1. **W0 — contracts and substrate selection.** Freeze schemas and fixtures,
   then run the same crash-recovery fixture against Restate and the explicit
   database/reconciler/outbox baseline. Record the provisional winner,
   measured behavior, and rejected alternatives before building the
   production controller.
2. **W1 — disposable probe (no plane change).** A two-stage probe drives one
   stage's full quality loop plus one stage-to-stage handoff over
   `POST /v1/sessions` + polling, with the typed `openab.sdd.quality/v1`
   block from day one; it confirms the W0 recommendation against real OCP
   boundary failures. The probe may additionally run a real, low-risk
   workload — the discipline is that the probe code is disposable, not that
   its content is fake; mock bots remain mandatory for the deterministic
   boundary cases.
3. **W2 — durable one-stage controller.** Implement the selected durable
   substrate, close-webhook wake with query/replay recovery, per-stage caps,
   durable human commands, and `needs_attention` escalation. The W0
   failure-injection suite reruns here as the production controller's
   acceptance gate (prior-art R12).
4. **W3 — nine-stage runs** with artifact-manifest chaining (D4), configured
   approval gates (D7), and canonical Wrap delta planning (dry-run; the first
   real publish adapter arrives in W4).
5. **W4 — operator and side-effect safety.** Budgets/deadlines, audited
   operator controls, effect-plan/approval digest binding, and the first real
   publish adapter with idempotency plus read-after-write reconciliation.
6. **W5 — generic OCP hardening only where the slices prove a gap.**
   Candidates are idempotent session open/opening prompt, durable
   action-result replay, exact result-message identity, scoped controller
   auth, and ADR 017 durable events.
7. **W6 — dev-lane dogfood** and the explicit production go/no-go decision.
8. **Write-capable Fix stage last** — deliberately beyond W0–W6, gated on the
   ADR 018/019 requested-scope token work and a real second writing consumer.

## Residue (honest ledger)

- **Self-grading.** If the same agent produces a stage artifact and rates it,
  the score is theater. The controller MUST reject that assignment under every
  profile; a development-only relaxation may overlap reviewer and rater roles,
  never producer and rater.
- **Score inflation over time.** A numeric gate invites drift toward
  10/10-by-default steering. The ADR 021 feedback loop (adoption-first,
  human-gated) is the counterweight; no additional mechanism added here.
- **Webhook fragility.** Best-effort close webhook is only a wakeup. Durable
  controller state/journal plus OCP query/replay is the correctness path. ADR
  017 events may later improve wakeups and audit, but are not a prerequisite.
- **Watchdog vs long deliberation.** A single phase that legitimately needs
  longer than `OABCP_SESSION_TIMEOUT_SECS` still force-closes; the controller
  sees a TIMEOUT verdict and redrives as an `infra_try` or escalates.
  OCP-enforced deadlines stay per-session (the watchdog); stage- and
  run-level deadlines are D6 controller counters. No per-stage watchdog
  enters OCP.
- **Open-plus-prompt crash window.** The current controller interpreter creates
  a session and posts its opening prompt as separate operations. The first slice
  reconciles by deterministic `trigger_ref`; a generic idempotent/atomic
  contract is a likely OCP hardening item before production scale.
- **Controller authentication.** The north API currently has one bearer key.
  Production deployment needs a scoped machine identity before operator controls
  or multiple workflow controllers share the surface.

## References

- `docs/sdd-quality-workflow-prior-art.md` — primary-source comparison and
  derived requirements
- ADR 007 — control plugins; rule 5 (no plugin platform before the dogfood
  path is stable)
- ADR 008 — external controller protocol (dormant transport)
- ADR 012 — session close webhook (best-effort contract)
- ADR 013 — decision/review state; §5 "the plane never calls GitHub"
- ADR 018 — Stage 3 extraction seam; rulings 4/6 (no new `sessions` columns,
  frozen wire surfaces); residue: requested-scope token route
- ADR 019 — untrusted-PR input boundary; token scope ceilings
- ADR 020 — review audit/effectiveness ledger; plugin-owned tables, trailer
  demoted to legacy compatibility
- ADR 021 — review effectiveness feedback loop; Goodhart guardrails
- `docs/coordinators.md` — Pipeline; `Goal` enum proposed-and-cut
- `docs/design.md` — plane-vs-steering test; B4 (verdict content is not a
  kernel guarantee)
- `docs/sdd-quality-workflow-plan.md` — implementation slices
- `tests/second_consumer.rs` — S16 north-surface consumer proof
