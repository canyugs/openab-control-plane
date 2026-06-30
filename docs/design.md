# Design

OCP sits between clients (north) and OpenAB pods (south). The boundaries are
deliberate — anything that OpenAB or the agent CLI already handles stays there.
"Pipe, not container" — the plane coordinates but never reasons.

## Design discipline

The scope tables below are maintained by deletion, in this order (Musk's
"algorithm", applied to scope):

1. **Question every requirement** — each feature names *who* needs it and *why*.
   A requirement that can't name an owner is the first to go.
2. **Delete what another layer owns** — if OpenAB, the agent CLI, or the bot pod
   already does it, it leaves OCP. The "does NOT own" table is this step's output.
   Deleted too much? Add it back — but expect to add back less than you cut.
3. **Simplify what survives** — only after deletion. Never polish a feature that
   shouldn't exist (we don't `[agent.steering]`-inject, don't serve `CLAUDE.md`).
4. **Then accelerate, then automate** — auto-trigger and structured output come
   *after* the manual flow is proven correct, never before (see ROADMAP phases).

The bias is toward a smaller plane. A boundary that feels too aggressive and gets
walked back (e.g. act-as-user, re-scoped not deleted) means it was drawn tight
enough to test.

## OCP owns

| Concern | Why OCP |
|---------|---------|
| Session lifecycle + state machine (open → … → closed) | Multi-agent coordination doesn't exist in OAB; the engine owns the CAS state transitions, mode-agnostically |
| Roster, fanout, isolation | OAB is single-bot; multi-bot routing is the plane's job |
| The **coordination seam** (`Coordinator` trait) | The engine owns the *seam*; the *policy* plugged into it (quorum, solo, …) is not core — see "Policy vs mechanism vs substrate" below |
| Durable delivery (outbox, replay) | Cross-bot message reliability |
| Bot identity + per-bot tokens | The plane manages the registry; credentials stay in each pod (`inherit_env`) |
| `/bot-config/:id` — OAB config.toml assembly | Bots fetch gateway/agent/pool config; store is a trait seam (SQLite default, Postgres/libSQL swap) |
| North API (sessions, messages, SSE) | The client-facing interface |

## OCP does NOT own

| Concern | Who owns it | Why |
|---------|-------------|-----|
| Agent steering (`CLAUDE.md`, `AGENTS.md`, `.kiro/steering/`) | Bot deployer via OAB `pre_seed` / `pre_boot` | Agent-agnostic — any CLI OAB supports works without plane changes |
| LLM reasoning / verdict content | The agent (chair bot) | Plane never calls an LLM |
| Agent credentials (`CLAUDE_CODE_OAUTH_TOKEN`, API keys) | Each bot pod via `inherit_env` | Plane never touches model keys |
| Bot-side credential consumption — fetching its scoped GitHub token, configuring `gh`, not holding a static write PAT | OAB bot / pod | OCP **mints, offers (`/v1/sessions/:id/github-token`), and purges-on-close** per-role scoped tokens (purge = drop from the store so they're never served again, not a GitHub-side revoke); how the pod *consumes* a credential (and which static tokens it carries) is the pod's job — same boundary as `inherit_env` above |
| PR-specific logic (gh pr diff, gh pr comment, label) | Application shim or chair bot | Code review is an app on top of OCP, not part of it |
| Agent lifecycle (spawn, pool, session TTL) | OpenAB (`[agent]` + `[pool]` config) | OAB's existing session pool management |
| Platform adapters (Discord, Slack, Telegram) | OpenAB gateway | OCP speaks the gateway wire protocol, not platform APIs |
| File/knowledge seeding (S3, git clone) | OAB `pre_seed` / `pre_boot` hooks | Boot-time setup is the bot image's responsibility |
| Coordination **policy** (quorum, completion condition, who synthesizes) | A `Coordinator` impl | The engine owns the seam, not the policy — see below |

## Policy vs mechanism vs substrate

"Generic coordination engine" is precise about *which* layer is generic. Three
layers, three different dispositions:

| Layer | What it is | Disposition |
|-------|-----------|-------------|
| **Mechanism** (core) | session state machine, CAS transitions, fanout, durable delivery, identity, north/south interfaces, **and the `Coordinator` seam itself** | Fixed. This is OCP. |
| **Policy** (pluggable) | what a done-signal means, when the group has converged, who synthesizes, what prompts whom | A `Coordinator` impl. **Quorum is not privileged** — it's the v1 reference impl (`QuorumCouncil`), one policy beside solo/debate/pipeline. The engine does not "have quorum" any more than it "has debate". |
| **Substrate** (the floor) | the OAB gateway wire protocol: a shared broadcast thread, `react`/`reply`/`edit`, the `🆗` done-signal | Accepted, not owned. Not OCP's choice — OCP rides **stock** OAB pods, so it speaks what they speak. Being substrate-neutral = a different project. |

Consequence: OCP makes coordination **policy** swappable; it is deliberately
**opinionated about the communication substrate** (broadcast-thread over the
gateway), because that substrate is what stock OAB gives you. The seam can't push
past the wire protocol without an OAB change — and that's an upstream change,
never smuggled into a Coordinator (see `docs/coordinators.md`, OAB-contract
invariant).

### What this implies for residual leaks

The discipline isn't "delete every mode-specific thing now" — it's the right
*disposition* per layer:

- **Speculative policy → cut.** `Goal`/`AllAngles`/`Rounds` motivate completion
  conditions for features (presets, debate) that have no consumer yet. Today the
  only real condition is quorum, and `QuorumCouncil` reads `quorum_n` directly —
  no `Goal` enum needed. Add it when a second condition actually lands.
- **Privileged config → defer-extract.** `session.quorum_n` is a quorum-shaped
  column in a supposedly mode-agnostic schema. Correct, but only worth
  generalizing (opaque per-coordinator config) when a second coordinator needs
  its own config — i.e. `Debate`'s round count. Not before.
- **Substrate → accept.** The broadcast-thread + `🆗` convention is the floor,
  forced by riding stock OAB. Not a leak to fix.

## Steering vs policy — what OCP actually guarantees

A running PR-review council (`multi-agent-review`) proves coordination needs **no
plane**: one shared `AGENTS.md` carries the roster, the quorum rule ("synthesize
at ≥5 `[done]`"), the handoff signal, and the side-effect (`gh pr comment`). The
aggregator *LLM* counts quorum; GitHub *is* the north contract. So the honest
question is not "can a plane coordinate" — steering already does — but **what does
a plane add that prose cannot.**

The answer is not *more* coordination. It is **guarantee**:

> Steering proposes (probabilistic — the LLM might miscount, skip a step, race, or
> die). The plane guarantees (deterministic — the invariant holds regardless).

OCP is the layer that holds the invariants the LLMs can't be trusted to hold.
This guarantee responsibility is also the axis OCP decomposes along (gateway =
delivery, policy = safety+liveness, membership = admission) — see
[ADR 001](adr/001-three-planes.md). Two classes, the standard distributed-systems
split:

| Guarantee | Class | Status |
|-----------|-------|--------|
| once-only + ordered (close once, transition once) | safety | ✅ CAS `advance_state` |
| only authorized members act | safety | ✅ roster gate |
| nothing acts after close | safety | ✅ post-close drop |
| no message lost across disconnect | safety | ✅ outbox + replay |
| **the session always reaches a terminal verdict** | **liveness** | ✅ `force_close_timeout` watchdog (deadline → forced close) |

**Test for "is it OCP's job":** must it hold even if a bot is slow, dead, buggy,
malicious, or hallucinating? → plane. Only when bots behave? → steering.
(`aggregator-output.md` shouting "You MUST post to GitHub" is the tell: a prose
step is skippable; a plane makes the outcome a structural consequence of reaching
`Closed`, not a step a model might forget.)

Consequence — an honest self-assessment of the modes:

- **`pipeline` is the strongest case for a plane.** "B must not start before A" is
  exactly what prose enforces unreliably (LLMs jump the gun); mention-gating makes
  the ordering a *delivery guarantee*.
- **`council`/`solo` are weak.** Council is what `multi-agent-review` already does
  in steering; `QuorumCouncil` only earns its place if you need determinism, a
  non-Discord/programmatic verdict, or the liveness guarantee below. (`solo` exists
  only to patch a 1-bot hang the plane *itself* introduced by taking over quorum.)
- **Both halves are real now.** Safety was always there; liveness landed as the
  `force_close_timeout` watchdog — a session-level deadline (`OABCP_SESSION_TIMEOUT_SECS`,
  default 10 min) forces `Close` with a `TIMEOUT` verdict and reviews-in-hand,
  naming absentees, so a silent reviewer can't hang `QuorumCouncil` forever (the
  steering version's flaky-attendance failure). It also emits a structured north
  `timeout` event so app shims/controllers can turn the close into a product-specific
  notification without moving side effects into the kernel. This is structurally
  impossible in pure prose — a dead bot can't run its
  own "wait 30 min then proceed" — which is exactly why it's the plane's job. By the
  decomposition theorem (every property = safety ∧ liveness), only now is OCP a
  *complete* guarantee layer rather than half of one.
