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