# Prior art for the stage-gated SDD quality workflow

Status: researched · 2026-07-13

Related decision: [ADR 022 — Stage-gated SDD workflow](adr/022-stage-gated-workflow.md)

Implementation: [SDD quality workflow implementation plan](sdd-quality-workflow-plan.md)

## Research question

The proposed workflow has nine ordered software-development stages:

```text
Discuss → Explore → Prototype → Spec → Usage → Ticket → Dev → Review → Wrap
```

Each stage produces an artifact, reviews and rates that exact revision, and
either accepts it or sends it through a bounded fix loop. This research asks:

1. Which existing systems already solve durable sequencing, retries, human
   waits, and crash recovery?
2. Which agent frameworks validate the review → evaluate → fix pattern?
3. Which spec-driven development systems provide useful artifact and approval
   conventions?
4. What evidence shows that an LLM-reported `quality_score > 9` is not, by
   itself, a trustworthy production gate?
5. Which parts should OCP adopt as contracts, and which runtimes or abstractions
   should it avoid?

Only official documentation, official repositories, and original papers are
used below. Product marketing claims are treated as capabilities to verify, not
as proof that a runtime meets OCP's failure model.

## Executive conclusion

No single prior-art system is the design.

- **Spec Kit** contributes versioned specs/plans/tasks and cross-artifact
  analysis/convergence; **Kiro** contributes explicit approval points; and
  **OpenSpec** contributes delta specs plus sync/archive into canonical truth.
- **Anthropic's evaluator–optimizer pattern, MetaGPT, and ChatDev** validate
  structured producer/reviewer separation and iterative refinement, but do not
  provide a durable parent workflow.
- **Temporal, Restate, DBOS, and Step Functions** define the durability
  semantics the controller must match: durable cursor/history, replay,
  at-least-once activities that require idempotency, signals, bounded retries,
  and explicit human waits.
- **LangGraph, Google ADK, AutoGen, CrewAI, and the OpenAI Agents SDK** provide
  useful checkpoint, resume, loop, and manager patterns. LangGraph can implement
  an external controller; the others still need application-owned parent policy.
  Their broader agent-runtime abstractions are not required for this fixed
  profile.
- **Agent evaluation research** shows that self-evaluation and LLM judges are
  useful but biased and incomplete. The numeric score may remain a routing
  predicate, but passage requires digest-bound evidence, deterministic hard
  checks, an independent rater, and configured human approval.

The architecture boundary in ADR 022 remains correct: an external
`sdd-controller` owns the parent workflow; OCP executes bounded child
sessions. The prior-art research changes one implementation assumption:
**the controller's durability mechanism is a W0 selection, not a pre-decided
custom SQLite workflow engine.** A time-boxed spike must compare a
Restate-backed controller with the minimal database/reconciler/outbox design.
DBOS is retained if a non-Rust controller runtime is acceptable; Temporal is
the semantic reference and a later-scale option.

## Evaluation criteria

The comparison uses these criteria:

| Criterion | Required behavior |
|---|---|
| Durable progress | Resume after process or host restart without losing the workflow cursor |
| Replay safety | Completed logical work is not repeated; ambiguous external effects reconcile safely |
| Stable identity | Workflow, stage, attempt, phase, artifact, and commands have deterministic ids |
| External events | Webhooks and human approvals can arrive late, duplicate, reorder, or disappear |
| Versioning | A running workflow pins its definition, rubric, schemas, and role assignments |
| Bounded execution | Quality, infrastructure, time, token/cost, and external-side-effect retries have limits |
| Auditability | Decisions and overrides are reconstructable without scraping free-form chat |
| Deployment fit | Operable as a small Zeabur service; no Kubernetes client or provider lock-in |
| OCP fit | OCP remains a generic session plane rather than acquiring product stage semantics |

## 1. Durable workflow engines

| Prior art | What it establishes | Important limits | OCP disposition |
|---|---|---|---|
| [Temporal](https://docs.temporal.io/workflow-execution) | Event History is the durable source; deterministic workflow code replays while completed Activity results come from history. Signals, Updates, Queries, timers, child workflows, and retry policies are first-class. | Activities are at-least-once and still need business idempotency. Workflow code has determinism/versioning constraints. The official [Rust SDK](https://github.com/temporalio/sdk-rust) is still Public Preview. A Temporal service is substantial new operations for this first profile. | **Semantic reference; defer adoption.** Borrow event/activity separation, signals, version pinning, and distinct transport versus business retries. Revisit when multi-node failover, many workflow profiles, or cross-service durable RPC justify it. |
| [Restate](https://docs.restate.dev/foundations/key-concepts) | Each durable action/result is journaled and replay skips completed work. Workflow keys provide once-per-id execution; durable promises/signals and query handlers support long waits and human input. The Restate Server is a single binary with built-in persistence. | Deployment still has two services: the Restate Server and the controller's HTTP workflow handler, plus persistent storage, registration, and backup. Restate lists a Rust SDK, but its [Rust SDK README](https://github.com/restatedev/sdk-rust) says it is under active development and may break across releases; workflow APIs, retention, and upgrade behavior must be proven. | **Lead engine candidate.** Its HTTP-facing server model is close to OCP, but it is not a one-binary controller deployment. Do not adopt until independent service crashes/upgrades, persistent-volume restore, query, signal, retention, and Rust API gates pass. |
| [DBOS](https://docs.dbos.dev/python/tutorials/workflow-tutorial) | Workflow ids are idempotency keys; step outputs are checkpointed, interrupted workflows resume from the last completed step, and durable events/messages support human-in-the-loop flows. Its [system database](https://docs.dbos.dev/python/tutorials/database-connection) can use SQLite or Postgres. | Official SDKs are [Python, TypeScript, Go, and Java](https://docs.dbos.dev/explanations/portable-workflows), not Rust. SQLite is for prototype/test and Postgres is recommended for production. DBOS recommends [Conductor](https://docs.dbos.dev/production/conductor) for production recovery/operations; commercial self-hosting uses a [paid proprietary license](https://docs.dbos.dev/production/hosting-conductor). External effects still need idempotency. | **Conditional candidate.** Keep it only if a supported controller language and the Postgres/Conductor operational and licensing model are acceptable. Its checkpoint and workflow-id semantics remain useful if rejected. |
| [AWS Step Functions Standard](https://docs.aws.amazon.com/step-functions/latest/dg/concepts-error-handling.html) | Explicit Retry/Catch policies, attempt limits, backoff, and [task-token callbacks](https://docs.aws.amazon.com/step-functions/latest/dg/connect-to-resource.html) make failure and human approval first-class state-machine concepts. | AWS/ASL coupling, account-bound callback tokens, service quotas, and a deployment model unlike Zeabur/OCP. | **Design reference; reject for this profile.** Borrow durable command tokens and bounded retry semantics, not the service. |
| Minimal DB + reconciler + outbox | A small controller can atomically persist materialized state, append an audit event, and enqueue the next OCP action; a level-driven reconciler can recover from lost wakeups. It matches this workflow's simple ordered stages and bounded loops. | This is real workflow-engine correctness code: leases/CAS, ambiguous HTTP outcomes, idempotency, migrations, backup/restore, timers, signals, and observability all become our responsibility. A table called `action_outbox` is not evidence that these guarantees hold. | **Viable baseline/fallback, not an assumed winner.** It must run the same failure-injection suite as Restate and win on simplicity without weakening guarantees. |

OpenAI's Agents SDK now documents
[Temporal, Restate, and DBOS integrations](https://openai.github.io/openai-agents-python/running_agents/)
specifically for runs spanning long waits, retries, or process restarts. This is
useful independent confirmation that conversation/session persistence is not a
substitute for durable workflow execution.

### Durability lessons adopted

1. Treat every OCP child session as an at-least-once external activity.
2. Separate **transport retry** of one logical phase from **quality iteration**
   that creates a new candidate attempt.
3. Use deterministic business identity:

   ```text
   sdd:<workflow_id>:<stage>:<attempt>:<phase>
   ```

   This scheme applies to the generic `POST /v1/sessions` path, where the
   caller owns `trigger_ref`. A stage that delegates to the PR-review plugin
   inherits plane-owned per-PR identity and the plane's round budget as an
   outer bound (ADR 022 D1); the §7 fixture deliberately models only the
   generic path.

4. After an ambiguous OCP response, query by stored session id or
   `trigger_ref` before retrying. If the effect cannot be observed or
   deduplicated, enter `needs_attention` with an ambiguous-effect reason.
5. Pin workflow and rubric versions for the lifetime of a run.
6. Persist human controls as authenticated, idempotent commands rather than
   waiting on an in-memory future.
7. Store large artifacts outside the workflow history. Persist URI, revision,
   digest, and compact evidence references.

## 2. Agent orchestration and refinement loops

| Prior art | Useful semantics | Limit or rejection |
|---|---|---|
| [Anthropic — Building effective agents](https://www.anthropic.com/engineering/building-effective-agents) | Distinguishes predefined workflows from model-directed agents. Its evaluator–optimizer pattern is exactly generator → evaluator feedback → revision, and is appropriate only when evaluation criteria are clear and iteration measurably improves the result. | It is an architecture pattern, not persistence. The controller supplies durable state, stopping conditions, budgets, and evidence validation. |
| [LangGraph persistence](https://docs.langchain.com/oss/python/langgraph/persistence), [interrupts](https://docs.langchain.com/oss/python/langgraph/interrupts), and [functional API](https://docs.langchain.com/oss/python/langgraph/functional-api) | Durable checkpoints, a stable thread identity, pause/resume, replay, and explicit idempotency for side effects. A deterministic graph whose nodes call OCP could be the external controller. | Resume can re-execute a node/task, so external effects remain at-least-once. The first bake-off rejects it because a Python/JS graph plus production checkpointer adds agent/graph abstractions this fixed linear profile does not use, without removing effect-idempotency or storage operations. Reconsider if a non-Rust graph product becomes a real second consumer. |
| [Google ADK LoopAgent](https://adk.dev/agents/workflow-agents/loop-agents/) and [resume](https://adk.dev/runtime/resume/) | A loop agent does not decide its own quality; it needs `max_iterations` or explicit escalation. Resumed tool calls have at-least-once behavior and therefore require idempotency. | ADK session/events are agent-runtime state, not the SDD artifact and gate ledger. |
| [AutoGen GraphFlow](https://microsoft.github.io/autogen/dev/user-guide/agentchat-user-guide/graph-flow.html) | Sequential, parallel, conditional, and cyclic edges with explicit termination provide a useful graph-validation model. | GraphFlow is documented as experimental. Free-text conditions such as an `APPROVE` substring and application-managed state snapshots are not production gate or durability contracts. |
| [CrewAI Flows](https://docs.crewai.com/en/concepts/flows) | Typed flow state, routing, SQLite-backed persistence, resume, and fork show that workflow state is distinct from conversation state. | The documented persistence model does not establish safe external-effect replay at every crash boundary. It is lower-weight prior art than the durable engines. |
| [OpenAI Agents SDK orchestration](https://openai.github.io/openai-agents-python/multi_agent/) and [human-in-the-loop](https://openai.github.io/openai-agents-python/human_in_the_loop/) | Application-code orchestration is deterministic and predictable; evaluator loops and serializable approval state are first-class patterns. | A [handoff](https://openai.github.io/openai-agents-python/handoffs/) transfers control within an agent run. It is not a durable nine-stage workflow cursor. |

### Agent-loop lessons adopted

- Parent control flow is code-controlled. Agents choose how to produce, review,
  rate, or fix within one bounded phase; they do not select the next workflow
  stage.
- Producer, reviewer, and rater are distinct roles. Under the default
  production policy, the rater identity differs from the producer and every
  quality reviewer.
- Each loop has independent attempt, wall-clock, token/cost, and side-effect
  limits, plus an explicit escalation destination.
- Conversation history, workflow state, artifacts, and approval state are
  separate records with separate retention and access rules.
- A free-form `APPROVE`, `[done]`, emoji, or regex score never advances the
  parent workflow.

## 3. Spec-driven development systems

| Prior art | Artifact/gate model | What OCP should adopt | What it does not solve |
|---|---|---|---|
| [GitHub Spec Kit](https://github.com/github/spec-kit) and its [SDD method](https://github.com/github/spec-kit/blob/main/spec-driven.md) | Constitution → specification → plan → tasks → implementation, with clarify, checklist, pre-implementation `analyze`, and post-implementation `converge` checks. Specifications are treated as the source of truth. | Versioned artifacts, explicit dependencies, pre-code consistency analysis, and post-code convergence back to artifacts. | It is a development method and agent steering package, not a durable runtime or an independently calibrated quality gate. |
| [Kiro feature specs](https://kiro.dev/docs/specs/feature-specs/) | Requirements → design → tasks with user review between phases and task-level execution/verification. | Configurable human stage approval, traceable requirements, and verification after each implementation task. | Quick-plan modes can intentionally skip approvals; Prototype, Ticket projection, and Wrap/archive need an OCP-specific profile. |
| [OpenSpec](https://github.com/Fission-AI/OpenSpec/blob/main/docs/overview.md) | Explore → proposal/design/tasks plus spec deltas → apply → verify → sync/archive into canonical specs. | Brownfield-friendly current-truth model, delta specs, explicit verification, and Wrap as sync/archive rather than a prose summary only. | It is deliberately lightweight/advisory; artifacts do not themselves enforce durable gates or external-effect safety. |
| [MetaGPT](https://arxiv.org/abs/2308.00352) | Encodes software SOPs into role-based handoffs that produce PRD, design, tasks, code, and tests. Intermediate verification reduces errors from naive agent chaining. | Typed role handoffs, prerequisite scheduling, and structured intermediate artifacts. | A fixed assembly line can become waterfall; the paper does not establish production durability or trustworthy autonomous approval. |
| [ChatDev 1.0](https://arxiv.org/abs/2307.07924) and its [1.0 branch](https://github.com/OpenBMB/ChatDev/tree/chatdev1.0) | Specialized design, coding, testing, and documentation roles use multi-turn review and bounded repair loops. | Two-role propose/validate separation and bounded code-review/test repair. | Natural-language consensus is not quality evidence, and the current repository defaults to ChatDev 2.0; the surveyed four-phase claims are specifically from the 1.0 paper/branch. |

### Mapping the nine stages to established artifacts

| SDD stage | Prior-art anchor | Required canonical output |
|---|---|---|
| Discuss | OCP profile synthesis, informed by Kiro approval and Spec Kit governance/clarification rather than a source-equivalent stage | problem statement, goals, non-goals, constraints, success measures, unresolved decisions |
| Explore | OpenSpec explore; Spec Kit research | sourced alternatives, assumptions, risks, recommendation |
| Prototype | weakly covered by the surveyed systems | inspectable prototype reference plus findings, shortcuts, and disposal/adoption decision |
| Spec | Spec Kit specification; Kiro requirements/design; OpenSpec deltas | versioned requirements, architecture, interfaces, state/failure/security/test design |
| Usage | OCP post-Spec validation using Spec Kit scenarios/quickstart and Kiro-style testable requirements | user journeys, permissions, failure UX, executable acceptance criteria, plus a controlled superseding Spec revision if validation finds inconsistency |
| Ticket | Spec Kit/OpenSpec/Kiro tasks | dependency-ordered work items mapped to acceptance criteria; tracker issues are projections |
| Dev | apply/implement phases; MetaGPT/ChatDev coding | exact code/PR revision plus deterministic test, lint, security, and policy evidence |
| Review | Spec Kit `converge`; OpenSpec verify; ChatDev review/test | reviewed release-candidate manifest, digest-bound findings, approve decision, no unresolved required blocker |
| Wrap | OpenSpec sync/archive for canonical specs; additional OCP profile policy for delivery closeout | canonical spec/docs update plus OCP-defined release/deploy record, runbook, limitations, and follow-up ownership |

Prototype and Usage are not consistently first-class in prior art. They must not
be silently folded into chat history: the OCP profile gives each an explicit
artifact schema and digest.

## 4. Evaluator reliability and the numeric gate

The screenshot's `quality_score > 9` is a routing rule, not proof of quality.

- Anthropic's [guide to agent evaluations](https://www.anthropic.com/engineering/demystifying-evals-for-ai-agents)
  recommends combining deterministic, model-based, and human graders because
  no single layer catches every failure. Model graders need calibration against
  human judgment.
- Anthropic's [long-running agent harness research](https://www.anthropic.com/engineering/harness-design-long-running-apps)
  reports that self-evaluation tends to be overly positive; separating producer
  and evaluator helps, but the evaluator can still be lenient.
- [G-Eval](https://arxiv.org/abs/2303.16634) found stronger human correlation
  from LLM-based evaluation than older automatic metrics, while also observing
  bias toward LLM-generated text.
- [Judging LLM-as-a-Judge](https://proceedings.neurips.cc/paper_files/paper/2023/file/91f18a1287b398d378ef22505bf41832-Paper-Datasets_and_Benchmarks.pdf)
  documents position, verbosity, self-enhancement, and reasoning limitations
  in model judges.
Therefore the controller computes a composite gate:

```text
schema valid
AND workflow/stage/attempt/rater/rubric bindings valid
AND reviewed candidate-manifest digest equals the current candidate manifest
AND all required deterministic hard checks pass
AND independent rating score_x100 > 900
AND any profile-required human approval is recorded
```

The rater cannot emit an authoritative `passed` boolean. A score of 900 fails;
901 can pass only if every other predicate passes. Rubric changes require human
review and are versioned for future runs; the workflow never tunes its own
threshold, prompt, or evaluator from observed scores.

## 5. OCP-specific disposition

| Option | Decision | Reason |
|---|---|---|
| Put the nine stages and backward edges in OCP `SessionState` or `pipeline` | Reject | Mixes product policy into the generic plane; conflicts with the session watchdog, monotonic votes, roster uniqueness, and verdict-content boundary. |
| Use LangGraph as the parent runtime | Reject from the first bake-off | It is capable of being the external controller, but a Python/JS graph and production checkpointer add unused graph/agent surface without removing effect-idempotency or storage operations. Reconsider for a real graph-shaped second consumer. |
| Use AutoGen, ADK, CrewAI, or Agents SDK as the parent runtime | Reject for v1 | Their agent/session abstractions overlap OCP and still leave application-owned durable product state/effects. Borrow patterns only. |
| Use one OCP session for the entire workflow | Reject | Long-lived parent state and repeated reviewer/fixer rounds do not fit the current session lifecycle. |
| External controller + one OCP child session per phase | Accept | Keeps deterministic product policy and durable cursor outside the plane while reusing OCP identity, delivery, liveness, CAS, and coordination. |
| Restate-backed external controller | W0 lead candidate | Closest deployment and durability fit, but Rust workflow API maturity, upgrades, backup/restore, and operational behavior must be proven. |
| Custom DB/reconciler/outbox controller | W0 baseline/fallback | Smallest dependency footprint, but only acceptable if it meets the same crash/replay test suite without quietly rebuilding an unsafe workflow engine. |
| DBOS-backed controller | Conditional candidate | Strong lightweight checkpoint model; consider only if the controller may use one of DBOS's supported languages. |
| Temporal-backed controller | Defer | Strongest reference semantics; current operational weight and Public Preview Rust SDK are disproportionate to one linear profile. |
| Step Functions | Reject | Provider and DSL coupling do not fit the intended Zeabur-native deployment. |

## 6. Derived implementation requirements

These requirements are normative inputs to ADR 022 and the implementation plan.

1. **R1 — External parent ownership.** The parent run, stage cursor, attempts,
   gate, artifacts, and operator commands live in `sdd-controller`.
2. **R2 — Phase isolation.** Produce, review, rating, and fix are fresh OCP
   child sessions with deterministic identities.
3. **R3 — Durable substrate selection.** W0 records a provisional
   evidence-backed choice between Restate and the custom DB baseline; DBOS is
   optional if the runtime constraint changes. W1 confirms the choice against
   real OCP boundary failures. No production controller is built before the
   final record.
4. **R4 — Equivalent guarantees.** Whichever substrate wins must preserve
   durable progress, idempotent dispatch, long waits, versioning, audit, and
   backup/restore. A runtime journal and a custom outbox must not be duplicated
   without a named business-level need.
5. **R5 — Retry separation.** Redelivery reuses the same dispatch-try id. An
   intentional infrastructure redrive keeps the logical stage/attempt/phase id
   but increments `infra_try` and its dispatch id; a quality failure creates a
   new quality attempt.
6. **R6 — Composite gate.** Numeric score is necessary for the screenshot
   profile but insufficient. Digest binding, schemas, evidence, hard checks,
   role independence, and configured approval are mandatory.
7. **R7 — Artifact truth.** Canonical bodies stay in Git, a tracker, or object
   storage. A cumulative immutable manifest stores all required references,
   revisions, digests, and explicit forward supersessions; the controller or a
   trusted adapter verifies the hashes.
8. **R8 — Definition pinning.** Stage list, schemas, rubric, threshold, budgets,
   roles, and human-gate policy are snapshotted into every run.
9. **R9 — Bounded loops.** Every quality and infrastructure loop has attempt,
   time, token/cost, and side-effect limits with a durable escalation state.
10. **R10 — Human control.** Pause, resume, approve, retry, override, and cancel
    are authenticated idempotent commands with actor, reason, target, and event.
    Approval binds the exact stage/effect target and digests; a score override
    cannot create approval or bypass a required hard check.
11. **R11 — Canonical wrap.** Completion includes sync/archive of the accepted
    artifacts into current truth; it is not merely a final chat summary.
12. **R12 — Failure injection.** Selection and acceptance tests kill the
    controller at every transition/effect boundary and lose/duplicate/reorder
    wakeups.
13. **R13 — No self-tuning.** Metrics and scores may inform a human-reviewed
    future version; a running or future workflow never rewrites its own gate.
14. **R14 — Kernel vocabulary stays generic.** No SDD stage, attempt, rubric, or
    score enters OCP's generic session state or gateway protocol.
15. **R15 — Trusted provenance.** Artifact adapters verify immutable provider
    revisions and compute manifest digests; controller-owned executors or
    verified attestations produce hard-check results. Agent claims are not
    authoritative hashes or checks.
16. **R16 — Effect safety.** Irreversible work follows
    prepare → verify → approve → publish → read-after-write verify. Approval is
    bound to exact manifest and effect-plan digests.
17. **R17 — Dispatch and projection safety.** Logical phases and intentional
    dispatch retries have distinct ids. A runtime-backed audit projection uses
    deterministic event ids, an explicit authority/watermark, and a tested
    replay/rebuild path.
18. **R18 — Controller engine/profile seam** (adopted from ADR 022 D2 rather
    than derived from a surveyed system). The controller's generic
    run/stage/attempt/phase, event, dispatch, and recovery code contains no
    SDD vocabulary; stage names, rubric ids, and score semantics enter only
    through the versioned profile. A grep gate enforces the seam from W2
    onward, mirroring OCP's kernel-purity CI gate.

## 7. W0 runtime selection experiment

The spike uses the same one-stage `Spec` control-flow fixture, an
OCP-conformance mock, and a deterministic fake publish adapter for each
candidate. The mock deliberately reproduces current OCP limitations:
active-only create deduplication, closed rows sharing a logical trigger,
matching/conflicting fingerprints, and the create-before-opening-prompt crash
window. The fake adapter exercises effect-plan approval and ambiguous provider
outcomes without making canonical external changes. W0's result is provisional
until W1 repeats OCP boundary cases against a real instance.

| Test | Required result |
|---|---|
| Duplicate create while active, after completion, and after configured retention | One logical workflow; idempotency horizon is explicit and expiry cannot silently create a conflicting run |
| Crash after durable claim, before send | Existing pending action is dispatched once logically |
| OCP commits, response is lost, child closes before reconciliation | Controller finds/attaches or enters `needs_attention`; it never blindly creates a second business phase |
| Multiple closed children share a logical phase | Distinct dispatch-try ids make the intended child unambiguous |
| Response received, crash before journal/outbox acknowledgement | Replay uses the same dispatch id and reconciles the existing child |
| Session created, opening prompt missing | Controller repairs through an independently idempotent prompt or escalates; it does not assume the child ran |
| Result stored, crash before event/projection/cursor | One immutable result/event and eventual single guarded transition |
| Crash before/after domain-event write and projection update | Deterministic event ids prevent duplication; watermark/rebuild restores the projection |
| Two reconcilers race; lease expires; stale worker returns | Epoch/fencing prevents the stale worker from acknowledging or advancing |
| Controller handler, Restate Server, storage, or custom reconciler crashes independently | Each process/storage boundary resumes from the same durable logical state |
| Approval/pause/resume/cancel arrives early, late, duplicate, or reordered | Authenticated command ids and target versions yield one deterministic outcome |
| Fake provider commits, response is lost, controller crashes before storing the effect result | The same provider idempotency key or read-after-write reconciliation yields one verified logical effect |
| Approval names an old candidate-manifest or effect-plan digest | It cannot authorize a changed candidate, payload, target, adapter version, or precondition |
| Human wait | Virtual-clock long wait plus a short real restart soak prove no in-memory dependency |
| Definition/rubric deployment change | In-flight run keeps its pinned version |
| Backup/restore with pending work | Workflow, event history, manifest refs, commands, and pending actions recover |
| Runtime/handler upgrade and rollback | Exact SDK/server pair is pinned; in-flight fixture survives or a tested migration/rollback handles incompatibility |
| Retention expiry | Completed-workflow and idempotency retention are configured and tested beyond their horizon |
| Observability | Operator can identify current phase, retry cause, lease/invocation owner, next wakeup, projection watermark, and linked OCP session |
| Zeabur deployment | Controller handler, Restate Server (if selected), persistent volume/database, registration, health, resource, backup, and recovery runbooks are sized separately |

Selection output is a short provisional architecture record containing
measured recovery behavior, operational components, SDK maturity,
implementation size, and the explicit reason for choosing or rejecting each
candidate. W1 must confirm it against the real north API before W2 starts. If
Restate and the custom baseline both fail a required test, selection stops
rather than weakening the durability contract.

## Sources

### Durable execution

- [Temporal workflow execution](https://docs.temporal.io/workflow-execution)
- [Temporal Event History](https://docs.temporal.io/encyclopedia/event-history)
- [Temporal retry policies](https://docs.temporal.io/encyclopedia/retry-policies)
- [Temporal Signals, Updates, and Queries](https://docs.temporal.io/encyclopedia/workflow-message-passing)
- [Temporal Rust SDK](https://github.com/temporalio/sdk-rust)
- [Restate key concepts](https://docs.restate.dev/foundations/key-concepts)
- [Restate workflows](https://docs.restate.dev/use-cases/workflows)
- [Restate service types](https://docs.restate.dev/foundations/services)
- [Restate service configuration and retention](https://docs.restate.dev/services/configuration)
- [Restate server deployment](https://docs.restate.dev/server/overview)
- [Restate Rust SDK](https://github.com/restatedev/sdk-rust)
- [DBOS workflow tutorial](https://docs.dbos.dev/python/tutorials/workflow-tutorial)
- [DBOS database connections](https://docs.dbos.dev/python/tutorials/database-connection)
- [DBOS cross-language support](https://docs.dbos.dev/explanations/portable-workflows)
- [DBOS Conductor](https://docs.dbos.dev/production/conductor)
- [DBOS self-hosted Conductor licensing](https://docs.dbos.dev/production/hosting-conductor)
- [AWS Step Functions error handling](https://docs.aws.amazon.com/step-functions/latest/dg/concepts-error-handling.html)
- [AWS Step Functions callback tasks](https://docs.aws.amazon.com/step-functions/latest/dg/connect-to-resource.html)
- [OpenAI Agents SDK durable execution integrations](https://openai.github.io/openai-agents-python/running_agents/)

### Agent orchestration and evaluation

- [Anthropic — Building effective agents](https://www.anthropic.com/engineering/building-effective-agents)
- [Anthropic — Demystifying evals for AI agents](https://www.anthropic.com/engineering/demystifying-evals-for-ai-agents)
- [Anthropic — Effective harnesses for long-running agents](https://www.anthropic.com/engineering/harness-design-long-running-apps)
- [LangGraph persistence](https://docs.langchain.com/oss/python/langgraph/persistence)
- [LangGraph interrupts](https://docs.langchain.com/oss/python/langgraph/interrupts)
- [LangGraph functional API](https://docs.langchain.com/oss/python/langgraph/functional-api)
- [Google ADK LoopAgent](https://adk.dev/agents/workflow-agents/loop-agents/)
- [Google ADK workflow resume](https://adk.dev/runtime/resume/)
- [Microsoft AutoGen GraphFlow](https://microsoft.github.io/autogen/dev/user-guide/agentchat-user-guide/graph-flow.html)
- [CrewAI Flows](https://docs.crewai.com/en/concepts/flows)
- [OpenAI Agents SDK multi-agent orchestration](https://openai.github.io/openai-agents-python/multi_agent/)
- [OpenAI Agents SDK human-in-the-loop](https://openai.github.io/openai-agents-python/human_in_the_loop/)
- [G-Eval paper](https://arxiv.org/abs/2303.16634)
- [Judging LLM-as-a-Judge paper](https://proceedings.neurips.cc/paper_files/paper/2023/file/91f18a1287b398d378ef22505bf41832-Paper-Datasets_and_Benchmarks.pdf)

### Spec-driven software development

- [GitHub Spec Kit](https://github.com/github/spec-kit)
- [Spec Kit's spec-driven development method](https://github.com/github/spec-kit/blob/main/spec-driven.md)
- [Kiro feature specs](https://kiro.dev/docs/specs/feature-specs/)
- [OpenSpec overview](https://github.com/Fission-AI/OpenSpec/blob/main/docs/overview.md)
- [OpenSpec explore mode](https://github.com/Fission-AI/OpenSpec/blob/main/docs/explore.md)
- [MetaGPT paper](https://arxiv.org/abs/2308.00352)
- [MetaGPT repository](https://github.com/FoundationAgents/MetaGPT)
- [ChatDev paper](https://arxiv.org/abs/2307.07924)
- [ChatDev 1.0 branch](https://github.com/OpenBMB/ChatDev/tree/chatdev1.0)
- [Current ChatDev repository](https://github.com/OpenBMB/ChatDev)
