# Prior art — LLM multi-agent collaboration mechanisms

A read of Tran et al. 2025, *Multi-Agent Collaboration Mechanisms: A Survey of
LLMs* (arXiv 2501.06322), mapped onto OCP's council architecture. Companion to
[prior-art.md](prior-art.md) (review-quality landscape). Where that doc scans how
the field *measures* review quality, this one scans how the field *frames
collaboration* — and where OCP sits in, and departs from, that frame.

## The survey's frame

The paper's contribution is a taxonomy: every multi-agent collaboration is a
**channel** `c = {actors, type, structure, strategy}` plus a **coordination**
layer. Five dimensions:

| Dimension | Options the paper enumerates |
|-----------|------------------------------|
| **Actors** | which agents are in the channel |
| **Type** | cooperation · competition · coopetition |
| **Structure** | centralized · decentralized/P2P · hierarchical |
| **Strategy** | rule-based · role-based · model-based |
| **Coordination** | static (predefined channels) · dynamic (a manager agent assigns roles/channels at runtime) |

It is descriptive — it catalogues MetaGPT, AgentVerse, CAMEL, AutoGen, OpenAI
Swarm, etc. under one grid. It does **not** ask what a collaboration *guarantees*.

## OCP mapped onto the five dimensions

| Dimension | OCP | Notes |
|-----------|-----|-------|
| **Actors** | Council = **chair + N reviewers**; chair is a *position* (`roster[0]`), not an identity; only slot with `pull_requests:write` | ADR 024 binds write scope to the active chair slot, not the `role` label |
| **Type** | **Cooperation only** (fan-in). Competition/debate was *proposed and cut* as speculative policy | `design.md` §"residual leaks": `AllAngles`/`Rounds` had no consumer → deleted |
| **Structure** | **Centralized + hierarchical** — the kernel is the only hub; bots never talk P2P; the chair reads a shared broadcast thread | substrate invariant: bot messages are stored + emitted north, never fanned to peers by the mechanism |
| **Strategy** | **Role-based** (chair/reviewer + angles) + **rule-based** (quorum count) | `assign_angles` round-robins preset angles; quorum = participating reviewers |
| **Coordination** | **Two layers**: in-session `Coordinator` (policy) + cross-session `Controller`. Mostly **static**; dynamic roster replacement exists | ADR 022 SDD stage-gate lives in an *external durable controller*; kernel never branches on verdict content |

Closest cousins in the survey: OCP's council ≈ MetaGPT/AgentVerse role-based
cooperation (§4.2.1); OCP's `Pipeline` ≈ OpenAI Swarm routines & handoffs (§5.2).

## Where OCP is sharper than the survey

The survey treats collaboration as an **LLM-layer emergent phenomenon** (agents
"debate", "negotiate"). OCP's central thesis cuts orthogonally to the whole grid:

> Steering *proposes* (probabilistic — the LLM may miscount quorum, skip a step,
> race, or die). The plane *guarantees* (deterministic — the invariant holds
> regardless). — [design.md](../design.md)

That forces a three-layer split the survey has no vocabulary for:

| Layer | OCP | In the survey? |
|-------|-----|----------------|
| **Mechanism** (fixed) | state machine, CAS once-only close, fanout, durable delivery | ✗ — the survey never models deterministic guarantees |
| **Policy** (pluggable) | `Coordinator`; **quorum is not privileged**, just the v1 reference impl | ~ the survey's type/strategy, but it treats quorum as an inherent capability |
| **Substrate** (accepted, not owned) | OAB gateway wire protocol, `🆗` done-signal | ✗ — no substrate concept at all |

The engineering question the survey's five dimensions *cannot* answer, and OCP's
split can: **must this hold even if a bot is slow, dead, buggy, malicious, or
hallucinating?** → plane. **Only when bots behave?** → steering.

## The survey's open problems → OCP's answers

The paper's §6 "open problems" are, for several items, already shipped in OCP as
safety/liveness guarantees — because OCP organizes by the Alpern & Schneider
safety∧liveness decomposition ([ADR 001](../adr/001-three-planes.md)), not by the
survey's collaboration taxonomy.

| Survey open problem (§6) | OCP mechanism |
|--------------------------|---------------|
| Unified governance (role assignment, failure recovery, fallback agents) | ADR 023 liveness (passive `-32603` → `degraded`), ADR 024 blue-green chair, roster failover to a different-provider standby |
| Cascading hallucination (one agent's error amplified) | `force_close_timeout` watchdog guarantees the session *terminates*, so a dead/hallucinating reviewer can't hang the council forever |
| **Shared decision-making** (dictatorial/majority voting is too thin) | **Open in OCP too** — council is count-based quorum + chair synthesis, no handling of *content* disagreement. This is the survey's critique landing on OCP. See sketch below. |
| Scalability / MAS scaling laws | `scale-knee.md`: provider quota binds at **N≈5**, an order of magnitude before the plane's fanout/state machinery. The real scaling law is provider-key throughput, not mechanism. |
| Comprehensive evaluation & benchmarking | ADR 015 eval-harness, ADR 020 effectiveness-ledger, ADR 021 feedback-loop — the "collaboration-level, fine-grained" eval the survey asks for |
| Ethical risk / compromised agents | ADR 019 untrusted-PR-input boundary, roster gate (only authorized members act), post-close drop, chair write-scope bound to slot not role |

## The gap worth closing — a debate/coopetition Coordinator

> Formalized in [ADR 026](../adr/026-debate-coordinator.md). The sketch below is
> its motivation; the ADR carries the decision, the `Action::Broadcast` mechanism,
> and the "detect conflict by structured trailer, never by prose" guarantee line.


OCP is most conservative on the **type** dimension: cooperation only. The survey
argues (§1, §4.2.3) that **diversity and coopetition** are precisely what let
collective intelligence exceed the sum of individuals. OCP's council today has no
handling for *conflicting* findings — if reviewer A says 🔴 and reviewer B says
🟢, the chair just synthesizes over the disagreement.

Sketch (kept inside OCP's guarantees, not smuggled into the mechanism):

- **Trigger**: a Coordinator (`DebateCouncil` / `CoopetitionCouncil`) detects a
  finding conflict at quorum (e.g. same file/line, opposite severity).
- **Action**: instead of closing, it emits an explicit coordinator `Action` —
  `Broadcast` the conflicting pair + `Relay-on-message` for one rebuttal round.
  `design.md` already reserves this: pre-done peer visibility must be an explicit
  coordinator `Action`, **never** reintroduced mechanism-side fanout.
- **Convergence**: bounded rounds (config lives per-coordinator, the deferred
  `Debate.rounds` column), then chair synthesizes the *resolved* view. Still
  terminates under the same watchdog — liveness preserved.
- **Diversity as design goal, not accident**: today mixed-provider councils exist
  to dodge rate limits (`scale-knee.md`). Repurpose that heterogeneity for
  **adversarial verification** — have a Gemini reviewer try to refute a Claude
  finding — catching single-model blind spots (the survey's competition-drives-
  robustness claim, §4.2.2). This also trades diversity for count, easing the N≈5
  quota knee.

This is the one move that most advances OCP toward the survey's "collective
intelligence beyond the sum of parts" **without** giving up the determinism that
distinguishes a plane from steering.

## Takeaway for OCP

The survey is a **map of collaboration shapes**; OCP is a **guarantee layer** for
one shape (centralized, role-based cooperation). OCP has independently solved
most of the survey's governance/liveness/evaluation open problems by treating
collaboration as a distributed-systems problem, not an emergent-LLM one. The
survey's live critique of OCP is on **type**: cooperation-only leaves
coopetition's robustness on the table. First concrete step — a `DebateCouncil`
Coordinator that resolves finding conflicts via one explicit `Broadcast`/`Relay`
round before synthesis, using existing mixed-provider rosters as adversarial
diversity.

## Sources

- Multi-Agent Collaboration Mechanisms: A Survey of LLMs — Tran, Dao, Nguyen,
  Pham, O'Sullivan, Nguyen (UCC / Pusan / TCD), arXiv 2501.06322v1, Jan 2025
- Cross-refs: [design.md](../design.md), [ADR 001](../adr/001-three-planes.md),
  [ADR 022](../adr/022-stage-gated-workflow.md), [ADR 023](../adr/023-bot-agent-liveness.md),
  [ADR 024](../adr/024-blue-green-chair.md), [scale-knee.md](scale-knee.md)
</content>
</invoke>
