# Handoff

State as of this session. Read `docs/coordinators.md` (spec, source of truth) and
`docs/design.md` ("Policy vs mechanism vs substrate") before changing coordination.

## Done + verified

- **Phase 1 milestone PROVEN.** Fresh template deploy → council verdict in
  **3m34s** (< 5 min), live on `canyugs/council-demo#1`; chair posted the verdict
  comment. Full loop: start → deliberate → reply → close. (Test project torn down.)
- **Live re-confirmation on `openabdev/openab#1187` (2026-06-27, image `0.1.2`).**
  Full template deploy (control-plane + chair + rev1 + rev2) on the user's Zeabur
  server. The council produced a genuine expert review — verdict **Request
  Changes** with a correct **P1** (`is_multiple_of()` needs Rust 1.87 MSRV), **P2**
  HTTP-drain race + OAuth client-id impersonation; chair attempted
  `gh pr review --request-changes`. **Two live findings:**
  1. **Watchdog works in production.** The real bots stalled in multi-round
     deliberation without cleanly hitting quorum-close; `force_close_timeout`
     force-closed at session age 910s (900s deadline). The liveness guarantee
     built this session saved a real hung council. ✅
  2. **Quorum→close was fragile with real bots → FIXED.** They signal completion
     in message *text* (`[done]`, the convention the real Discord council actually
     uses), not the `add_reaction` 🆗 the quorum path counted. `is_done_signal` +
     `check_text_done` now treat a trailing `[done]` (or a bare 🆗) on send/edit as
     a done-signal (synthetic 🆗 → same coordinator path); the watchdog now also
     surfaces the chair's synthesized verdict instead of burying it. `open-council.sh`
     trigger updated to instruct `[done]`. Proven by
     `tests/spike.rs::council_closes_on_text_done_signal` (closes with zero
     reactions). (Test project torn down.)
- **`[done]` fix LIVE-PROVEN on `canyugs/openab#14` (2026-06-27, image `0.1.3`).**
  Fresh template deploy (project `oab-council-v013`, OnCloud server) against a
  purpose-built test PR (`humanize_bytes` with a planted index-OOB panic + a
  unit-label mismatch). The council **closed in 80.8s via the text-`[done]`
  quorum path** — `system: "Quorum reached…"` fired, chair/rev1/rev2 all emitted
  trailing `[done]`, *not* the watchdog (lifetime ≪ 900s). Both planted bugs
  caught; chair posted a correct **Request Changes** verdict to the PR
  (`unit + 1 < UNITS.len()` fix + regression test) via `gh`. This is the v0.1.2
  live-found fragility (#1187, closed only by the 910s watchdog) now closing
  cleanly under quorum. Milestone re-run done. **Polish nit (steering, not
  plane):** chair posted 3 "in-progress" comments + 2 verdict comments — a bit
  noisy; the trigger could tell the chair to post in-progress once. Session
  `verdict` field came back empty (verdict lives in PR comments + thread, not the
  session row) — fine for now, surface it later if the API needs it.
- **Comment-noise fix LIVE-PROVEN + standing council is up (2026-06-27).** The
  trigger now tells the chair to maintain ONE PR comment via a single
  `gh pr comment N --edit-last --create-if-none` (creates first time, edits the
  same comment every wake after — so rounds 2/3 overwrite in place). Re-ran on
  `canyugs/openab#14` (old 8 comments deleted first): closed via quorum in 61.9s,
  PR ended with **exactly 1** well-formatted verdict (critical panic + SI/IEC
  label + thin-test findings). **Council is now a PERSISTENT deploy — do NOT tear
  down** (server is always on, idle pods cost ~nothing). Coords: project
  `oab-council` (`6a3f3afc22d1fdaf7eb045fe`) on OnCloud server, plane
  `https://oab-council.zeabur.app`, API key = control-plane `PASSWORD` var. Reuse
  for any review: `PLANE=… KEY=… scripts/open-council.sh owner/repo#N`.
- **Chair-closing-authority fix LIVE-PROVEN on `canyugs/openab-control-plane#4`
  (2026-06-27, image `0.1.4`).** Dogfooding the council on its *own* repo (a real
  Dependabot `rusqlite` 0.31→0.40 bump, not a planted-bug test) surfaced a fresh
  `QuorumCouncil` fragility: the reviewers deliberated but never emitted a
  done-signal, so the 2-of-2 reviewer quorum was never reached. The chair
  synthesized a correct verdict and signalled `[done]`, but the close was gated on
  `from: Quorum` → the CAS failed → the session hung in Deliberating until the
  900s watchdog, with a duplicate-ack chatter storm (chair/rev1 looping
  "Duplicate — session closed"). **Fix (PR #6, `caa32d3`):** `QuorumCouncil::on_done`
  closes from whichever active state the chair is in — `Quorum` (the designed
  path) OR `Deliberating` (the chair finished before a formal quorum). The chair
  is the coordination authority + only writer, so its done is authoritative;
  closing immediately also drops the post-close chatter (`handle_reply`'s
  closed-drop). Proven over the wire by
  `tests/spike.rs::chair_done_closes_without_full_quorum` (neither reviewer
  signals → still closes; hangs to timeout pre-fix) + 2 coordinator unit tests;
  31 unit + 8 spike green. **Live:** re-ran #4 on the redeployed `0.1.4` council —
  **closed in 51s via the chair `[done]` path** (reviewers never reached quorum),
  5 clean messages (was 13 with chatter), one edit-last verdict comment. Extends
  the v0.1.3 #1187/#14 done-signal work from "text `[done]` *counts*" to "chair
  `[done]` *closes* without a formal reviewer quorum." Council now runs `0.1.4`.
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

1. ~~Re-run the live milestone to confirm the `[done]` fix end-to-end.~~ **DONE**
   — `v0.1.3` cut + image published; live on `canyugs/openab#14`, closed in 80.8s
   via the `[done]` quorum path (not the watchdog). See "Done + verified" above.
   Open follow-on (optional, low pri): trim duplicate chair "in-progress" comments
   via the trigger; consider surfacing the verdict on the session row.
2. **Security (do soon):** rotate the GitHub PAT + `CLAUDE_CODE_OAUTH_TOKEN` —
   both were dumped in plaintext by `zeabur variable list` this session (already on
   the list). Needs your account access.
2. **Membership plane (ADR 001), continued.** inc1+inc2 done:
   - inc1 — **admission gate** (`admit` + `add_to_roster` → `Admission`; bounded +
     registered-bot roster, `409` on reject, `OABCP_MAX_ROSTER`).
   - inc2 — **bot self-recruitment**: `[[recruit:<id>]]` in a message →
     `maybe_recruit` → the same gate; authz `may_recruit` (chair-only v1). No new
     wire command (text convention). `GET /v1/sessions/:id` now returns `roster`.
   - inc3 — **fleet provisioner seam**: a recruit of an unregistered bot emits
     `provision_requested` (`recruit_event`) instead of failing. The actual
     pod-spinning is an **external** provisioner holding the Zeabur token —
     deliberately *not* in core (separate trust domain, off the hot path). Contract
     in `docs/provisioner.md`. Building that external service (with real Zeabur
     creds + live test) is the remaining work, outside this repo's core.
   Next (when a real need appears):
   - dynamic registry (join/leave as first-class events); widen recruit authz
     (role/allow-list); the external reference provisioner. ROADMAP Phase 4.
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
