# ADR 005 — Cost governance: model selection via roster-swap, not a session model wire

Status: **proposed** · 2026-06-28

## Context

The governance layer around the model (identity, permission, **cost**) is what the
plane exists to own — and the winning instinct is to make the cheap/safe path the
**default**, dialed by *"is the mistake reversible?"*. Identity already follows this
(reviewer read-only by default, short-lived per-role tokens; see
[ADR 004](004-bot-identity-shared-app-pod-local.md)). **Cost is the other face and is
unbuilt** — it has no named owner. This ADR fixes where it lives and how a model is
chosen/changed. Execution is tracked in #18; this ADR is the decision half.

Two facts constrain the design:

- **The plane cannot set a model today.** `OpenSession` (`src/api.rs:76`) carries
  `roster / quorum / chair_bot / mode` — no model field. `inherit_env`
  (`src/api.rs:245`) only passes API keys (credentials, not selection). Model is
  **pod-boot config**: `OPENAB_AGENT_COMMAND` / `[agent]` in OpenAB — one agent CLI
  per pod = one provider identity per deployment.
- **Two ways to make model plane-governed surfaced**, differing by swap type:
  - **Route A — session-scoped tier switch.** Add a per-role model field to the
    convene wire and drive OpenAB's ACP `session/set_config_option` → `/model set`,
    which switches model *within the live CLI process*. Same-provider only
    (opus↔sonnet↔haiku); requires the agent to advertise `configOptions`. New wire
    field + capability dependency.
  - **Route B — model as pod identity.** A price tier *is* a pod; "swap" = swap the
    roster. No wire change; reuses the Phase 4 membership seam (`orchestrator::admit`
    / `maybe_recruit` / `provision_requested` / fleet provisioner — all ✅/seam-✅).

## Decision

1. **Cost governance is a plane responsibility — but as policy/orchestration, not
   model execution.** Consistent with [ADR 001](001-three-planes.md): the plane
   guarantees coordination and decides *who joins / how long they run / when to spend
   more*; the actual model run stays a pod concern. The plane stays thin.

2. **Primary mechanism = roster-swap (Route B).** Model selection is governed by which
   pods are in the roster, not by a model field on the wire. Cheap default = a roster
   of cheap-tier pods; escalation = recruit a higher-tier pod via the existing
   membership seam. Chosen over Route A because it needs no wire change, doesn't depend
   on agent `configOptions`, and keeps model out of the control-plane contract.

3. **The plane-unique capability = escalate-on-irreversibility.** The plane is the only
   party that sees the council outcome, so it is the only one that can decide: run a
   cheap first pass, and **recruit a higher-tier reviewer (or bump the chair's model)
   when findings exceed a severity threshold or touch an irreversible area.** This —
   not "can set a model" — is what makes cost governance belong in OCP: deciding *when
   spending more is worth it*.

4. **Tier-1 knobs are the cheap-by-default lever** (config defaults, not new
   mechanism): default preset `quick` (3 bots, not standard/full), a tight
   `OABCP_SESSION_TIMEOUT_SECS` spend ceiling enforced by the existing watchdog, plus
   quorum/angle assignment already trimming redundant work.

5. **Route A (session-scoped `/model set`) is deferred, not rejected.** Cleaner UX
   (same bot, change tier) and the only way to do same-provider tier changes without
   spinning a second pod. Revisit once roster-swap is live and a same-provider
   fine-grained dial is actually wanted; it then adds one per-role wire field driving
   `set_config_option`.

## Consequences

- **Needs a price-tiered pod catalog** for the roster to pick from (e.g. a cheap-tier
  and an expensive-tier reviewer pod). This ties cost governance to the **fleet
  provisioner** (ADR 001 / `docs/provisioner.md`) — recruiting a tier that has no pod
  emits `provision_requested`.
- **BYOK interaction** (ROADMAP Phase 1): user-provided keys feed pod cost; the
  cheap-default policy operates on whatever keys the pods carry.
- **No control-plane wire change for the primary path** — the model never enters the
  `OpenSession` contract; this is deliberate (keeps the plane thin, per Decision 1).
- **Deferred risk:** if same-provider tier dialing turns out to be the common case
  (not cross-provider/identity swaps), Route A's wire field becomes worth building and
  this ADR should be amended.
- **Status → accepted** when the escalate-on-irreversibility roster-swap path lands
  (attach the PR). Until then this records the chosen direction; #18 tracks the work.
