# ADR 037 — Third panel: coding panel with a plan→implement→review loop

Status: proposed · 2026-08-02

## Context

ADR 014 surveyed the second-panel candidates and deferred the coding panel as
"third panel material" because it "immediately pulls the Phase 3 blackboard
and write-side-effect policy". Both pulls have since been resolved by events:

- **Artifact channel.** The implement stage's artifact is a git branch and a
  PR — git itself is the blackboard. The plane never carries code; sessions
  carry only pointers (plan text, branch name, PR number). No Phase 3
  primitive is needed.
- **The verification stage already exists.** The review council auto-convenes
  on every PR (dev lane `external_canary`, ADR 031) and lands a
  machine-readable verdict twice over: the `openab/council` GitHub status and
  the signed `session.terminal` event. The "check the implementation, pass or
  bounce" half of the loop is running in production today.

What does NOT exist is a loop. The kernel is deliberately loop-free:
`Pipeline` is single-pass forward-only, `Close` is terminal, and the
round-counting Debate coordinator (ADR 026) is parked. ADR 022's ruling
stands: workflow state (gates, retries, iteration) belongs to a durable
controller outside the plane; OCP sessions are the stages, not the engine.

The panel this ADR ships: a human submits a task; a planner produces a plan;
an implementor turns it into a PR; the existing review council verdicts it;
`request_changes` feeds the findings back for another iteration;
`approve` (or the iteration cap) ends the run.

## Decision

Ship a **coding-panel controller**: a small external controller that owns the
loop as an explicit state machine and drives OCP through the public north
API only. Zero kernel diff — no new mode string, no coordinator, no endpoint.

### 1. Stages are chained `solo` sessions; the loop lives in the controller

```
task (REST) ─► PLAN (solo) ─► IMPLEMENT (solo → branch + PR) ─► COUNCIL (existing)
                  ▲                    ▲                            │ verdict
                  │                    └──── request_changes ◄──────┤
                  └──── plan declared invalid by implementor        │ approve
                                                                    ▼
                                                                  DONE
```

- **PLAN**: `open_session` `mode:"solo"`, roster `[planner]`, opening input =
  the task. Terminal `final_messages` = the plan.
- **IMPLEMENT**: `mode:"solo"`, roster `[implementor]`, opening input = plan
  (+ prior-round findings, if any). The implementor pushes a branch and opens
  a PR from its pod; its final message must carry the PR pointer.
- **COUNCIL**: nothing to do — opening the PR convenes it. The controller
  waits on the verdict.
- **Feedback routing, v1**: `request_changes` → new IMPLEMENT session with
  the council findings appended. Only when the implementor's final message
  declares the plan itself unworkable does the controller fall back to a new
  PLAN session. No LLM routing decision in the controller — it reads
  declared markers, mirroring the verdict-trailer philosophy (B4).

### 2. Gate signal: poll the GitHub status in v1 — bound to the run, not the message

v1 polls the PR's `openab/council` status (approve / request_changes /
error). It is the product-facing contract, survives plane restarts, and
needs no event grant. Consuming signed `session.terminal` events instead is
a v2 refinement, not a correctness requirement.

**Binding invariant (the pointer is a hint, never an authority).** The
implementor's final message ingests partially untrusted content (plan text,
prior-round findings), so its PR pointer could be substituted for an
unrelated PR that already carries a green status. The controller therefore
resolves the gate target independently: the run owns the branch name (the
controller assigns it in the IMPLEMENT opening input), and the gate PR must
(a) live in the run's configured repository, (b) have the run-owned branch
as its head, (c) be authored by the implementor identity, and (d) carry the
`openab/council` status on the exact head SHA the controller captured when
it observed the PR — the implementor's message is used only to speed up
discovery, and any mismatch parks the run as `needs_human`.

### 3. Loop discipline: cap, journal, idempotency

- **Iteration cap** default 3; on cap or council `error`, the run parks as
  `needs_human` — the controller never retries its way through a fail-closed
  verdict.
- **Journal**: controller-owned SQLite, one row per run + one per stage
  transition (the ADR 022 pattern; github-pr-controller's store is the
  precedent). Restart-safe: state is re-derived from the journal, sessions
  are deduped by `trigger_fingerprint = hash(run_id, stage, iteration)`.
- **Human gate (optional)**: `require_plan_approval` pauses the run after
  PLAN until `POST /runs/:id/approve`. Off by default in v1; the mechanism
  is the controller's, per ADR 022 D7. Task submission and approval require
  authenticated operator authority (bearer key held by operators only), are
  scoped to the named run, and are idempotent per run — a replayed approve
  is a no-op, never a second advance.

### 4. Write-side-effect policy: the implementor writes as an author

The one-writer rule (ADR 031) governs the *review* product: council bots
must not write to GitHub. The implementor is not a council bot — it is an
**author**, a distinct trust domain, and author-vs-reviewer separation is a
feature: the council reviews code it did not write, under an identity that
cannot approve its own PR.

v1: the implementor pod gets a deploy key (or fine-grained PAT) scoped to
**one experiment repository**, dev lane only. Because the implementor
ingests partially untrusted input while holding a write credential (the
ADR 019 tension), the credential model is named here, not left to
implementation: the key is **push-only to the run-owned branch namespace**
(`panel/*`), enforced server-side (branch protection on everything else),
and the promotion path is a controller-minted short-lived token or a write
broker — the standing key never widens. Widening beyond that repo is a
promotion-time decision recorded in a future amendment — not silently.

### 5. Deployment: dev lane, raised timeout, own installation

- New controller installation (`coding-panel-dev`), action token granting
  `open_session` only; deployed beside github-pr-controller.
- Sessions run on the **dev plane** with `OABCP_SESSION_TIMEOUT_SECS` raised
  to 1800 — implement stages will not fit 600s. Accepted dev-lane cost:
  stuck *review* sessions are also force-closed later. Prod is untouched. A
  dedicated panel lane (ops `lane-bootstrap.md`) is the escape hatch if the
  shared timeout hurts.

## Alternatives considered

- **One `pipeline` session for plan+implement** — no loop-back, one timeout
  budget across both stages, and the council verdict lands after the session
  is already closed. Rejected.
- **A rounds-aware kernel coordinator** (Debate-shaped, ADR 026) — pulls a
  workflow engine into the kernel against ADR 022/031; still cannot span the
  council, which is a *separate session*. Rejected.
- **Wait for Phase 3 blackboard** — unnecessary; git is the artifact store
  and pointers fit in messages. Deferred on its own merits.

## Consequences

- The loop exists, is journaled, capped, and inspectable — and the plane
  stays a coordination kernel; every stage is an ordinary session visible in
  the existing tooling (backups, watchdog, SLOs).
- Second external controller = first real test that controller installations
  compose (tokens, grants, quotas) beyond the founding tenant.
- The implementor credential is a new standing write capability outside the
  App-installation boundary — scoped to one repo, but it must appear in the
  ops key inventory and rotation schedule from day one.
- Known debts accepted at birth: status polling (v2: terminal events),
  global dev timeout raise (escape: dedicated lane), declared-marker
  feedback routing (revisit only with evidence it misroutes).
