# Handoff

State as of this session. Read `docs/coordinators.md` (spec, source of truth) and
`docs/design.md` ("Policy vs mechanism vs substrate") before changing coordination.

## Done + verified

- **Phase 1 milestone PROVEN.** Fresh template deploy → council verdict in
  **3m34s** (< 5 min), live on `canyugs/council-demo#1`; chair posted the verdict
  comment. Full loop: start → deliberate → reply → close. (Test project torn down.)
- **Coordinator refactor — Option 2 + Option 3 increment 1 done.**
  - Option 2: `output.rs` (verdict → gh comment) deleted from core — side-effects
    are the app's job; close path only emits `verdict`/`state:closed` events.
  - Increment 1: `Ctx` + `Action` + `Coordinator` seam (`src/coordinator.rs`);
    orchestrator builds `OrchCtx`, calls `on_done`, executes via `run_actions`.
    `QuorumCouncil` is the parity port. 18 unit + 1/3/5-bot spike tests green.
- Release CI: `v*` tag → test → build+push `docker.io/canyu/openab-control-plane`
  (`.github/workflows/release.yml`, SHA-pinned actions + Dependabot). Verified on
  `v0.1.0`/`v0.1.1`.

## Next — priority order

1. **Coordinator increment 2: `Solo` + `mode` column.**
   - `Solo` fixes the **live-found 1-bot bug** (a lone chair never closes: quorum
     counts roster-minus-chair = ∅). `Solo::on_done` = `[Close { from: Deliberating,
     verdict: cx.latest_settled(bot) }]`, no quorum gate.
   - Add `session.mode` (default `"council"`), dispatch in `coordinator::for_session`.
   - **DEFER `Goal`/`AllAngles`** — no consumer; `QuorumCouncil` reads `quorum_n`
     directly. (design.md disposition: speculative policy → cut.)
   - Keep 1/3/5-bot spike green (OAB-contract guardrail).
2. **Coordinator increment 3: Debate or Pipeline — only when a real second
   structural mode is an actual product need.** Forces `on_reply`/`on_join` trait
   widening + opaque per-coordinator config (generalizes `quorum_n`). Don't build
   speculatively.
3. **Security (do soon):** rotate the GitHub PAT pasted in plaintext this session;
   rotate the leaked `CLAUDE_CODE_OAUTH_TOKEN` (see memory).
4. **Other Phase 1 ROADMAP items (TODO):** shared steering via `pre_seed`
   (R2/`endpoint_url`), GitHub App identity (approve/request-changes on own PRs),
   per-role scoped tokens, preset-driven roster, application-shim formalization.
5. **Optional:** pin template image by `@sha256` digest (supply-chain) — deferred.

## Constraints (decided — don't relitigate)

- **OCP = pluggable coordination *policy* over a fixed OAB-gateway communication
  *substrate*.** Quorum is policy (`QuorumCouncil`), not core. The substrate
  (broadcast thread + `🆗` done-signal) is the floor, forced by riding stock OAB.
- **OAB-contract invariant:** plane-internal only — no new gateway wire types, no
  OAB change. `tests/spike.rs` (mock bots over the real wire) is the guardrail.
- **Disposition discipline:** speculative policy → cut; privileged config
  (`quorum_n`) → defer-extract until Debate needs its own config; substrate → accept.
- Repo `github.com/canyugs/openab-control-plane` (branch `main`). Image namespace
  `canyu` on Docker Hub.
