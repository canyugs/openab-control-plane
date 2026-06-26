# ADR 001 — Three planes, split by guarantee responsibility (not yet physically)

Status: accepted · 2026-06-27

## Context

OCP ships as one Rust binary, but it carries three distinct responsibilities.
The recurring question is whether to split them into separately-deployed
components — pushed especially by two desired capabilities:

- remove the Discord dependency so bots are added/removed freely and fast;
- let bots *recruit* more bots mid-session.

We need a principled split criterion, not "tidier code". The criterion is the
one from [design.md](../design.md) "Steering vs policy": **OCP exists to
*guarantee* things prose can't.** So decompose by *which guarantee a piece
owns* — and physically split only when the pieces differ in change rate,
scaling profile, trust domain, or lifecycle.

## Decision

Recognize three planes by guarantee responsibility. Keep them **in one binary
for now**; harden the seams so a physical split is cheap when a real driver
appears.

| Plane | Owns the guarantee | Status today | Prior art |
|-------|--------------------|--------------|-----------|
| **Data plane** (gateway) | *delivery* — at-least-once, ordering, outbox/replay | built | sidecar/Envoy, message broker |
| **Control plane** (policy engine) | *safety + liveness* — once-only close, ordering, **always terminates** | safety ✅, liveness ❌ (watchdog) | Temporal/durable execution, SDN control plane |
| **Membership plane** | *membership/identity* — who exists, who's alive, dynamic join, **admission** | weakest — static `OABCP_BOTS` + per-session roster | etcd/Consul/ZooKeeper, K8s API server + scheduler |

By Alpern & Schneider's decomposition theorem (every property = safety ∧
liveness), the control plane is **literally incomplete** until the liveness
watchdog lands — hence its Phase 1 priority.

### What this says about the two capabilities

Both land on the **membership plane**, today's weakest seam:

1. **"No Discord, free churn"** — already half-done: bots connect to OCP's `/ws`,
   not Discord. The gap is making membership *dynamic* (a registry where
   join/leave are first-class) instead of boot-time-static. `add_to_roster` +
   history backfill is the start.
2. **"Bots recruit bots"** is not a feature — it is a **guarantee problem**. A
   recruit *request* is a bot action, but **whether to admit is a control-plane
   decision**, never honored just because a bot asked (a hallucinating/malicious
   bot could spawn 1000). Admission control (quota, authz, guaranteed backfill of
   the new member) is the plane's job. If recruiting also spins up a *pod*, that
   touches infra (Zeabur) — a different trust domain that must NOT sit on the
   coordination hot path; it belongs to a fleet/provisioner, behind the
   membership plane.

## Consequences

- **Do not physically split now.** Splitting adds a network hop per bot event and
  an internal protocol to own.
- **Gateway ↔ policy**: the logical seam already exists (`Coordinator` trait). The
  pre-designed escape hatch for a physical split is `WebhookCoordinator` (policy as
  an external HTTP service) — extract *only* policy, only when needed.
- **App shim** (GitHub PR logic) is the already-planned north split; generic north
  (sessions/SSE) stays in core.
- **Membership plane** is the most-justified *future* split — registry (long-lived
  shared), sessions (ephemeral), and provisioner (infra, separate trust domain)
  have genuinely different lifecycles. Tracked in ROADMAP Phase 4 (Bot discovery).
- **Sequencing**: liveness watchdog (make what exists *trustworthy*) precedes
  dynamic membership / self-recruit (make membership *grow*). A system that
  neither guarantees termination nor bounds its own population is the wrong order.
