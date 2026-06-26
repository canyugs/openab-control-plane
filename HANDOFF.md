# Handoff

State as of this session. Read `docs/coordinators.md` (spec, source of truth) and
`docs/design.md` ("Policy vs mechanism vs substrate") before changing coordination.

## Done + verified

- **Phase 1 milestone PROVEN.** Fresh template deploy → council verdict in
  **3m34s** (< 5 min), live on `canyugs/council-demo#1`; chair posted the verdict
  comment. Full loop: start → deliberate → reply → close. (Test project torn down.)
- **Coordinator refactor — Option 2 + Option 3 increments 1–2 done.**
  - Option 2: `output.rs` (verdict → gh comment) deleted from core — side-effects
    are the app's job; close path only emits `verdict`/`state:closed` events.
  - Increment 1: `Ctx` + `Action` + `Coordinator` seam (`src/coordinator.rs`);
    orchestrator builds `OrchCtx`, calls `on_done`, executes via `run_actions`.
    `QuorumCouncil` is the parity port.
  - Increment 2: `session.mode` column (default `council`, additive `ALTER`
    migration) + `for_session(mode)` dispatch + `Solo` impl. **Fixes the
    live-found 1-bot hang** — proven over the wire by
    `tests/spike.rs::solo_single_bot_closes`. `open-council.sh` auto-picks `solo`
    for a 1-entry roster. `goal` column deferred (no second condition yet).
  - Increment 3: `Pipeline` (sequential handoff, non-fan-in). Needed only a
    `starters(roster)` kickoff hook (mention stage 0 only) + ordered roster
    (`ORDER BY rowid`) — no `on_reply`/`Goal`. `run_actions` untouched.
    `tests/spike.rs::pipeline_three_stages_closes_in_order` proves in-order close.
- **Liveness watchdog — OCP is now a *complete* guarantee layer.**
  `orchestrator::force_close_timeout` (CAS once-only `close_if_active` + verdict
  naming absentees) + a 30s background scan in `main.rs` over
  `active_sessions_before`. Deadline: `OABCP_SESSION_TIMEOUT_SECS` (default 900s).
  A silent/dead reviewer can no longer hang `QuorumCouncil` forever. By the
  safety∧liveness decomposition theorem, this is what made the control plane whole
  (was safety-only). 23 unit + solo/pipeline/1/3/5-bot spike tests green.
- Release CI: `v*` tag → test → build+push `docker.io/canyu/openab-control-plane`
  (`.github/workflows/release.yml`, SHA-pinned actions + Dependabot). Verified on
  `v0.1.0`/`v0.1.1`.

## Next — priority order

1. **Security (do soon):** rotate the GitHub PAT pasted in plaintext; rotate the
   leaked `CLAUDE_CODE_OAUTH_TOKEN` (see memory). Needs your account access.
2. **Membership plane (ADR 001):** dynamic registry (join/leave first-class) +
   bot self-recruitment behind **admission control** (quota/authz/guaranteed
   backfill) — a guarantee problem, not a feature. The user's stated direction.
   Comes after the watchdog (now done): make what exists trustworthy before
   letting the population grow. ROADMAP Phase 4.
3. **`Debate` mode — only when multi-round is a real product need.** This is the
   mode that *does* force `on_reply` + round state + per-coordinator config
   (generalizes `quorum_n`); `Pipeline` proved the seam without any of it. Don't
   build speculatively.
4. **Other Phase 1 ROADMAP items (TODO):** shared steering via `pre_seed`
   (R2/`endpoint_url`), GitHub App identity (approve/request-changes on own PRs),
   per-role scoped tokens, preset-driven roster, application-shim formalization.
5. **Optional:** pin template image by `@sha256` digest (supply-chain) — deferred.

## Constraints (decided — don't relitigate)

- **OCP = pluggable coordination *policy* over a fixed OAB-gateway communication
  *substrate*.** Quorum is policy (`QuorumCouncil`), not core. The substrate
  (broadcast thread + `🆗` done-signal) is the floor, forced by riding stock OAB.
- **OCP's role = the guarantee layer, not "more coordination."** Steering already
  coordinates (a real Discord council does roster+quorum+verdict in prose); the
  plane's job is the invariants prose can't hold — *safety* (once-only, ordering,
  auth, post-close drop — all ✅) and *liveness* (always terminates — ✅, the
  `force_close_timeout` watchdog). Test: must it hold even if a bot is slow/dead/buggy/malicious/
  hallucinating? → plane. `pipeline` (ordering guarantee) is the strongest mode;
  `council`/`solo` are weak (steering can do them). See `docs/design.md`.
- **Three planes, split by guarantee (ADR 001).** gateway = delivery, policy =
  safety+liveness, membership = admission. **Stay one binary for now**; the
  `Coordinator` trait (→ `WebhookCoordinator`) is the policy split's escape hatch.
  Membership (dynamic join, bot self-recruitment) is the real future seam but is a
  guarantee/admission problem, not a feature — and it comes *after* the watchdog.
- **OAB-contract invariant:** plane-internal only — no new gateway wire types, no
  OAB change. `tests/spike.rs` (mock bots over the real wire) is the guardrail.
- **Disposition discipline:** speculative policy → cut; privileged config
  (`quorum_n`) → defer-extract until Debate needs its own config; substrate → accept.
- Repo `github.com/canyugs/openab-control-plane` (branch `main`). Image namespace
  `canyu` on Docker Hub.
