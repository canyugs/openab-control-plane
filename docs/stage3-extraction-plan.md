# Stage 3 — PR-review extraction plan

Status: **proposed** · 2026-07-08 · executes the boundary-review-2026-07 Stage 3
item ([ADR 007](adr/007-control-plugins-and-oab-father.md) bundled plugins,
[ADR 008](adr/008-external-controller-protocol.md) controller vocabulary) and
finishes [ADR 016](adr/016-gateway-token-externalization.md) (D1) and the
ADR 010 B2 freeze. Trigger: **fired** — the forum-support north client
(docs/forum-north-client-plan.md) is the second consumer the boundary review
named. Planning only — no implementation in this document. References to
**ADR 018** are forward references: that ADR is this plan's own first
deliverable (S1) and does not exist yet.

## Summary

Stage 3 is a **hooks-first, move-second** extraction: first grow the two seams
that already exist — the `Coordinator` trait gains four defaulted policy
methods, and the `controller.rs` interpreter takes ownership of supersede
cleanup and gains `PostMessage` + atomic open-with-prompt — each as a
behavior-preserving PR verified by the *unmodified* existing integration tests;
only then `git mv` the PR-review application (council.rs, github_webhook.rs,
the orchestrator's trigger/task/verdict string code, ReviewCouncil,
TriageCouncil) into `src/plugins/` as strictly mechanical moves.

The whole stage makes **zero SQLite schema changes**. The ADR 013 verdict
columns and `set_session_verdict` stay in the kernel store, deprecated-in-place;
templates stay at `scripts/*.tmpl` (shared with `open-council.sh` and pinned by
the prompt-identity contract, council.rs:17-19); mode strings never change, so
persisted rows keep dispatching across upgrades. Consequence: every PR in the
sequence is independently live-deployable onto the dev/prod lanes' legacy DBs,
and rollback is always a plain image revert — the PR #145 hazard class
(schema/index-ordering breakage) is structurally excluded from this stage.

Guards land **before** the risky work, not after it: the /bot-config
golden-snapshot freeze (B2) and the wire-shape snapshot suite are S2/S3, the
ADR ratification is S1, and a permanent CI grep gate replaces "review-checklist
purity" once the moves complete. Exit is a live-verified dev-lane loop plus an
executable proof that the seam is generic: a forum-shaped end-to-end test that
exercises **only trait defaults and zero plugin code**.

Three honesty corrections this plan makes against the designs it synthesizes:

1. **"Kernel never names app code" is achieved by no design and not by this
   one.** `coordinator::lookup`'s review/triage arms reference
   `crate::plugins::*` structs directly — a compile-time static match, chosen
   deliberately over a dynamic registry because persisted-mode dispatch across
   restarts must never depend on registration order (a missing arm silently
   downgrades a `review_council` row to `QuorumCouncil`, which hangs a 1-bot
   roster — the C4 bug class). The dependency direction is kernel→plugins **by
   design**; the enforced boundary is "no review vocabulary in kernel *logic*",
   proven by a permanent CI grep gate (S12), not by cargo features.
2. **The ADR 013 verdict-column ambiguity is resolved here, before code
   lands.** boundary-review:691 says the verdict columns "move with the Stage 3
   plugin"; boundary-review:541 ties them to the M4 findings ledger; route.md
   puts M4 after Stage 3. Ruling: Stage 3 moves the columns'
   **interpretation** (the `[[verdict:]]` grammar becomes plugin code behind a
   `structured_verdict` hook); their **storage** moves with M4's plugin-owned
   findings table, per route.md ordering. ADR 018 (S1) records this as an
   amendment to the boundary review's :691 phrasing.
3. **Kernel wire surfaces keep ADR 013 vocabulary, on purpose.** The
   `/v1/sessions` JSON, the north `verdict` SSE event, the
   [ADR 012](adr/012-session-close-webhook.md) close-webhook payload, and the
   fused `/v1/stats` review aggregates are external contracts with live
   consumers (metering, forum plan). Stage 3 declares them **frozen v1 kernel
   contracts**, pins them with golden snapshots (S3), and assigns their future
   evolution to the M4 ADR — internal-signature purity is not bought by
   breaking external receivers.

## 1. Target module layout

Single crate, module split only — no cargo workspace (CI and the release
pipeline stay unchanged; crate split has a named precondition, §7).

```
src/
├── lib.rs             — adds `pub mod plugins;`
├── main.rs            — unchanged
├── api.rs             — kernel north REST/SSE + /bot-config (frozen, B2).
│                        The two PR-review route lines remain in router() but
│                        delegate to plugins::pr_review::webhook handlers.
│                        POST /v1/sessions gains optional `prompt` (S5);
│                        POST /v1/sessions/:id/messages routes through
│                        controller::execute (S5). check_auth stays kernel.
├── controller.rs      — ControllerAction { OpenSession, PostMessage };
│                        interpreter owns supersede cleanup (S4). Mode
│                        whitelist still derives from coordinator::lookup
│                        (controller.rs:132) — no hand-maintained list (B3).
├── coordinator.rs     — Ctx, Action, StructuredVerdict, Coordinator trait
│                        (+4 defaulted methods §2, +reopen gate S13), generic
│                        impls QuorumCouncil/Solo/Pipeline, lookup()/
│                        for_session(). review/triage arms reference plugin
│                        structs. Unknown-mode handling becomes fail-loud (S9).
├── orchestrator.rs    — pure mechanism (~1,000 non-test lines after
│                        extraction): fanout, watchdog, supersede after-effects,
│                        liveness/backfill/recruit, run_actions, close webhook
│                        transport. All review-string code (today lines 16-205,
│                        572-628) deleted-by-move. unfenced_lines stays
│                        pub(crate) (shared with the recruit parser).
├── github_app.rs      — unchanged kernel credential custody. Role→scope map
│                        is accepted residue with a sketched exit (§7).
├── identity.rs        — D1 edits only (S14/S15).
├── store.rs et al.    — untouched. Verdict columns + set_session_verdict
│                        doc-commented "deprecated-in-place; interpretation
│                        owned by plugins/pr_review; storage moves at M4".
└── plugins/
    ├── mod.rs         — `pub mod pr_review; pub mod triage;` + the bundled-
    │                    plugin rules doc: all writes via controller::execute,
    │                    reads via the Store trait, kernel logic never names
    │                    plugin items except the lookup arms.
    ├── pr_review/
    │   ├── mod.rs     — ReviewCouncil (coordinator + the four hook overrides)
    │   ├── council.rs — git-mv of src/council.rs: convene_for_pr/convene_ask,
    │   │                admission valves, presets/angles, trigger refs,
    │   │                REREVIEW markers, fingerprint construct+parse
    │   ├── tasks.rs   — chair/reviewer task rendering + trigger parse-back
    │   │                (render and parse co-located: the round-trip becomes
    │   │                one module's invariant); include_str! of
    │   │                ../../../scripts/pr-review-*.tmpl
    │   ├── verdict.rs — VerdictTrailer + parse_verdict_trailer (+ tests)
    │   └── webhook.rs — git-mv of src/github_webhook.rs (HMAC verify moves
    │                    with the handler; secret custody stays on AppState)
    │                    + the /v1/review handler from api.rs:1338-1385
    └── triage/
        └── mod.rs     — TriageCouncil + TRIAGE_QUORUM_PROMPT + TRIAGE-prefix
                         gate ([ADR 014](adr/014-triage-panel.md): coordinator-
                         only plugin; trigger surface stays scripts/)
```

**Not moved, deliberately:** `scripts/*.tmpl` stay in `scripts/` — they are a
cross-artifact contract shared with `open-council.sh`/`open-triage.sh` and
`docs/steering/pr-review.md`'s task-prefix role resolution; only `include_str!`
relative paths change. `tests/steering_sync.rs` keeps pinning template↔steering
drift and is **extended** (S10) to pin the task-prefix ↔ role-resolution
contract. The empty `src/north`/`src/south` dirs are deleted in S1 (they imply
a plugin story this plan does not build). `/bot-config` stays in kernel api.rs:
it is compatibility surface slated for demotion (§5), not app logic — moving
review-flavored dead code into the plugin would be churn.

## 2. Controller / Coordinator surface

### Coordinator trait (src/coordinator.rs) — four defaulted methods + one struct

Defaults encode the native OAB contract, so Solo/Pipeline/QuorumCouncil — and
forum, the consumer that carries no code — override **nothing**.

```rust
pub struct StructuredVerdict {
    pub decision: String,        // plugin vocabulary; kernel treats as opaque
    pub red: Option<i64>,
    pub yellow: Option<i64>,
    pub green: Option<i64>,
}

pub trait Coordinator: Send + Sync {
    // existing: kind(), on_done(), starters(), on_roster_change()

    /// B1: does `bot`'s 🆗 reaction count as its done-signal?
    /// Default: yes (native OAB set_done contract).
    fn reaction_counts_as_done(&self, cx: &dyn Ctx, bot: &str) -> bool { true }

    /// B1: is a textual done-signal from `bot` accepted, given the message?
    /// Default: yes.
    fn accepts_text_done(&self, cx: &dyn Ctx, bot: &str, text: &str) -> bool { true }

    /// Render the opening client trigger for delivery to `target`. Pure
    /// (chair, target, text) → text; MUST be applied by mechanism at ALL
    /// trigger-delivery sites: initial fanout, backfill replay, non-starter
    /// chair redelivery. Default: verbatim passthrough (the forum contract).
    fn recipient_trigger_text(&self, chair: Option<&str>, target: &str,
                              trigger_text: &str) -> String {
        trigger_text.to_string()
    }

    /// Interpret the closing verdict text into structured fields (ADR 013),
    /// called in the Close arm BEFORE the close webhook fires (the webhook
    /// re-reads the row). Default: None — text-only close, columns stay NULL,
    /// no warn log.
    fn structured_verdict(&self, verdict_text: &str) -> Option<StructuredVerdict> { None }

    /// S13 (follow-up PR, not bundled mid-extraction): may a client message
    /// reopen a terminal session (Closed|Aborted → Deliberating)?
    /// Default false; Solo overrides true (ADR 011 follow-up pattern).
    /// Removes the chat-policy leak at orchestrator.rs:229-234.
    fn reopen_on_client_message(&self) -> bool { false }
}
```

Overrides — **ReviewCouncil**: `reaction_counts_as_done` →
`Some(bot) != cx.chair()` (chair auto-🆗 must not close; the live
premature-close fix, today orchestrator.rs:1238-1247);
`recipient_trigger_text` → chair/reviewer task fabrication (tasks.rs);
`structured_verdict` → `parse_verdict_trailer`. **TriageCouncil**: same
reaction rule; `accepts_text_done` → non-chair, or chair text starting with
`TRIAGE` (today orchestrator.rs:1290-1298).

After S6, coordinator.rs:266's claim — "the only place a mode maps to policy"
— becomes true (**closes B1**, governed by ADR 007's kernel/policy split).

**Honest kernel residue:** `StructuredVerdict{red,yellow,green}` keeps
review-flavored vocabulary in a kernel trait signature, because the ADR 013
columns it feeds stay kernel this stage (Summary ruling 2). It moves with the
columns at M4.

### ControllerAction (src/controller.rs)

```rust
pub enum ControllerAction {
    OpenSession(OpenSessionAction),   // unchanged shape; `prompt` now actually
                                      // fed by POST /v1/sessions (S5)
    PostMessage(PostMessageAction),   // NEW (S5)
}
pub struct PostMessageAction { pub session_id: String, pub content: String }

pub enum ControllerActionResult {
    SessionOpened { session_id: String, deduped: bool },
    Superseded   { session_id: String, old_id: String }, // cleanup already done
    MessagePosted { session_id: String, message_id: String },
}
```

Semantics:

1. **Interpreter-owned supersede cleanup (S4).** `open_session`'s Superseded
   arm calls `orchestrator::handle_superseded_session(state, &old_id)` itself
   before returning; the three caller-remembered invocations (api.rs:508,
   api.rs:1375, github_webhook.rs:522) are deleted. A forgotten call site can
   no longer leak outbox rows and unrevoked GitHub tokens. **Closes B3's
   "caller must remember" class**; per ADR 008, side effects belong to the
   interpreter.
2. **Atomic open-with-prompt (S5).** `POST /v1/sessions` forwards an optional
   `prompt` into `OpenSessionAction` — today api.rs:498 hardcodes
   `String::new()`, forcing forum into a two-call open-then-message with a
   crash window between the calls. Forum is this stage's firing consumer; the
   gap closes before its Phase 1 starts.
3. **PostMessage (S5).** Validates session exists + non-empty content, then
   delegates to `orchestrator::post_client_message`;
   `POST /v1/sessions/:id/messages` routes through it. Forum's entire write
   surface (open + follow-up) is interpreter-mediated from day one — "never a
   second grandfathered exception" (**B3**, ADR 008).
4. **Reserved, not implemented** (declared in ADR 018 to pin ADR 008 v1
   vocabulary): `AddRoster`, `CloseSession`, `EmitStatus`. Roster routes keep
   calling the orchestrator directly until M3/M4 need them. The in-process
   action structs are the de-facto ADR 008 v1 schema; ADR 018 includes their
   **serialized JSON shapes** (unversioned) so "externalization is a transport
   change" is checkable, not aspirational.

### Fail-loud mode dispatch (S9)

`for_session`'s unknown-mode → `QuorumCouncil` fallback (coordinator.rs:282-284)
silently hangs a 1-bot roster (the C4 bug class). S9 replaces silent adoption:

- New opens for unknown modes are already rejected by controller validation
  (controller.rs:132 derives from `lookup`) — unchanged.
- A **persisted** row whose mode no longer resolves: `for_session` logs at
  error level, emits a north `dispatch_error` event, and the session is
  force-closed with `reason:"unknown_mode"` via the existing close CAS —
  refusal, never silent quorum adoption. Because dispatch is a compile-time
  static match, this path is unreachable in a correctly built binary; the
  fail-loud arm is insurance against future registry-style refactors.
- Test: `validate_accepts_every_dispatchable_mode` enumerates every kernel and
  plugin mode against controller validation and against `for_session`, pinning
  the B3 whitelist-drift class mechanically.

## 3. Migration sequence

Rules that hold for every row: each PR lands independently `cargo test` +
clippy green; each is deployable onto a legacy DB (zero schema change all
stage) and rolls back with a plain image revert; mechanical-move PRs (S10-S12)
allow **no logic edits** (review rule recorded in ADR 018); the pinned
integration tests (orchestrator.rs:2598-2724 chair-auto-🆗/TRIAGE-gate,
:3361-3412 trigger round-trip, :3577-3600 forum-shaped reopen) stay
**unmodified** through S4-S9 as the regression harness and move only with
their subjects. S7/S8's dependency on S6 is scheduling, not logic — the hook
PRs share that pinned harness, and landing them serially keeps each diff's
regression signal attributable to one hook; S7 may land in parallel with S6
if scheduling needs it (both depend only on S3 logically).

| ID | Title | Scope | Depends on | Acceptance test |
|---|---|---|---|---|
| S1 | ADR 018 + up-front ratification + doc sync | Write ADR 018 (this plan's rulings: layout, hook surface, reserved verbs + serialized shapes, verdict-column ruling, kernel→plugins direction, residues + exit triggers, M4 seam design, crate-split precondition). Flip ADR 007/008/016 proposed→**accepted-as-amended** — *before* any code PR, so a mid-stage council amendment cannot invalidate landed work. Revise docs/forum-north-client-plan.md against the S5 surface (dedupe exists, prompt field, reopen semantics, **hard rule: fresh session after close** — watchdog trap §7). Fix design.md:126-133 stale supersede text. Delete empty src/north, src/south. | — | Docs-only; council review of ADR 018 is the gate. |
| S2 | /bot-config golden-snapshot freeze (B2) | tests/bot_config_freeze.rs: byte-exact snapshots of rendered config.toml across (chair, reviewer) × (claude, codex, kiro) × (legacy, externalized) — the drift class already fired once (allow_* fields post-ADR-010). Module-doc freeze banner on bot_config(). Lands **first among code PRs** so the freeze guards drift *during* the stage, not after it. | S1 | Adding any field to the render fails CI until the golden file is deliberately regenerated citing ADR 010 B2. |
| S3 | Wire-shape snapshot suite | Golden snapshots of the four frozen v1 contracts: GET /v1/sessions JSON, north `verdict` SSE event, ADR 012 close-webhook payload, /v1/stats. Permanent CI fixtures — cheap insurance making the eventual M4 column move a snapshot-guarded change instead of archaeology. | S1 | Snapshots green on main; any byte change to the four shapes fails CI. |
| S4 | Interpreter owns supersede cleanup | Move handle_superseded_session into controller::open_session's Superseded arm; delete the three caller invocations (api.rs:508, api.rs:1375, github_webhook.rs:522). | S3 | New controller test: supersede via any ingress → old outbox purged + tokens revoked + `reason:"superseded"` event, with zero caller cooperation. Wire snapshots unchanged. |
| S5 | PostMessage + atomic open-with-prompt | ControllerAction::PostMessage; POST /v1/sessions/:id/messages via controller::execute; optional `prompt` on POST /v1/sessions through OpenSessionAction (kills the forum two-call crash window at api.rs:498). | S4 | Open-with-prompt creates session + message atomically; unknown session → Invalid; existing reopen test (:3577-3600) green unmodified. |
| S6 | Done-policy hooks (B1) | Add reaction_counts_as_done + accepts_text_done; override on ReviewCouncil **and** TriageCouncil in the same PR (landing one reopens the live chair-auto-🆗 premature-close bug); delete orchestrator.rs:1238-1247 and :1290-1298. | S3 | Pinned integration tests (:2598-2724) pass **unmodified**; new FakeCtx unit tests for both hooks on all five coordinators. |
| S7 | recipient_trigger_text hook | Add defaulted method; ReviewCouncil delegates to existing (unmoved) render helpers; rewire all three delivery sites (fanout ~:274, backfill ~:930, chair redelivery ~:1173). | S6 | NEW regression closing a named gap: a replacement chair backfilled into a review_council session receives the rewritten chair task, not the raw trigger. Verbatim-passthrough test for solo (forum contract). |
| S8 | structured_verdict hook | Add StructuredVerdict + hook; ReviewCouncil returns parse_verdict_trailer output; Close arm calls the hook. **The stage's only intentional behavior change:** non-review modes stop attempting trailer parsing — forum solo closes lose the per-close warn noise and can never write review columns. Release-noted. | S6 | (a) review close writes columns BEFORE close_webhook_payload reads the row (ordering pin, previously untested); (b) solo close leaves columns NULL, no parse; (c) north verdict-event field parity via S3 snapshots. |
| S9 | Fail-loud dispatch + mode-enumeration test | Replace the silent QuorumCouncil fallback for unknown persisted modes with error log + north `dispatch_error` + force-close `reason:"unknown_mode"` (§2). Add validate_accepts_every_dispatchable_mode. | S6 | Fixture: persisted row with a bogus mode → force-closed with the reason, never quorum-dispatched; enumeration test covers kernel + plugin modes. |
| S10 | Move A: plugins skeleton + coordinators + tasks/verdict | Create src/plugins/*; extract tasks.rs + verdict.rs from orchestrator.rs; move ReviewCouncil/TriageCouncil; lookup arms reference plugin structs; include_str! paths → ../../../scripts/. App-content tests (:2356-2596, :1556-1600, :3361-3412) move with the code. **Extend tests/steering_sync.rs** to pin the task-prefix ↔ docs/steering/pr-review.md role-resolution contract (chair/reviewer .tmpl first line is what bot-side role resolution keys on — previously unpinned). | S7, S8, S9 | Mechanical; cargo test count never drops; steering_sync extension green; trigger round-trip tests pin the string contract across the move. |
| S11 | Move B: council.rs | git mv src/council.rs → src/plugins/pr_review/council.rs; REREVIEW markers become plugin-internal (renderer and parser now one module). | S10 | Mechanical; prompt-identity re-asserted: open-council.sh posts the identical pointer template (templates never moved). |
| S12 | Move C: webhook + routes + **permanent CI grep gate** | git mv github_webhook.rs → plugins/pr_review/webhook.rs; move /v1/review handler (api.rs:1338-1385); api.rs router lines delegate. Add a **standing CI job** (not a one-time check): fail if `PR Review`, `gh pr`, `[[verdict`, or `TRIAGE` appear in api.rs / orchestrator.rs / coordinator.rs / controller.rs — the cheapest permanent approximation of the kernel-purity proof, without cargo features. | S11 | Grep-gate job green; /v1/review vs webhook admission-valve parity test; env-lock test helpers still shared (same crate). |
| S13 | Reopen gate (named follow-up, not bundled) | Add reopen_on_client_message (default false; Solo true); post_client_message's Closed→Deliberating arm (orchestrator.rs:229-234) becomes gated; non-solo reopen → 410 with explanatory body. A deliberate behavior change landing **after** the extraction, alone, release-noted. Plus: regression test **pinning the watchdog created_at reopen trap** (a reopened solo session force-closes within ~30s of the stale timeout) so the deferred fix (§7) has an executable description. | S12 | Reopen matrix test: solo reopens; review/triage/council → 410. Trap-pin test documents current (wrong) behavior with a FIXME naming the chat-mode trigger. |
| S14 | D1 steps 1+2 (ADR 016 blockers 1-2) | identity::issue() (identity.rs:18-22) consults externalize_tokens(): flag-on stores hash + empty plaintext (seed_token shape, identity.rs:54-79); token returned once in the POST /v1/bots response. RegisterBot gains optional operator-supplied `token` (is_safe_token + min length) — registration never round-trips plaintext through /bot-config; unblocks forum's Allen-bot provisioning. | S2 | Flag-on register → /bot-config serves ${OABCP_BOT_TOKEN} env-ref, token_plain empty, WS auth by hash works; flag-off bit-for-bit parity vs the S2 golden. |
| S15 | D1 step 3: graceful default flip | externalize_tokens() defaults ON when OABCP_EXTERNALIZE_TOKENS unset, **with a boot-time legacy-DB guard**: env unset + non-empty token_plain rows → run legacy with a loud deprecation warning naming the exact OABCP_BOT_TOKEN_<NAME> vars (ADR 016's "flip for new deployments" without bricking in-place upgrades). Explicit =0/=1 always wins. Both lanes already set =1 → no-op. | S14 | Fixture DB with token_plain rows + env unset → legacy fallback + warning; fresh DB + env unset → externalized; kill-switch is =0, rollback is image revert. |
| S16 | "Second consumer" proof test | End-to-end forum-flow integration test against the north surface only: open(mode=solo, prompt) → verbatim delivery asserted → bot done → close with text-only verdict event → follow-up PostMessage → reopened per S13 policy → second done → close. Exercises **only trait defaults, zero plugin code** — the executable proof the seam is generic. | S13 | The test passes using no `plugins::` import; it becomes the contract forum Phase 1 builds against. |
| S17 | B2 demotion runbook + deprecation telemetry | ADR 010 amendment appending the demotion runbook (§5): gated on S15 + template-repo configUrl work. Add a **per-serve deprecation warn log** on /bot-config, giving lane-log evidence for the "no consumer for a full release" removal trigger. No endpoint behavior change. | S15 | Warn log visible in dev-lane logs on pod boot; runbook merged; golden snapshot (S2) unchanged. |

### Deployment cadence (a stated rule, not an aspiration)

Published images historically lag main (0.1.14 shipped without #144), so
"independently deployable" must be exercised, not assumed. Rule: **four
publish points**, each followed by the smoke checklist below on the dev lane
(openab-council) before the next group starts:

1. after **S5** (controller surface complete),
2. after **S9** (all hooks + fail-loud dispatch — the riskiest behavior
   window),
3. after **S12** (mechanical moves, batched into one publish — no behavior
   delta expected),
4. after **S15** (D1 flip; also the prod-lane upgrade point).

**Per-lane smoke checklist** (the cross-artifact contracts —
scripts/*.tmpl ↔ steering doc ↔ open-council.sh — have no full automated
coverage; this list is the compensating control): auto-convene on a test PR →
fanout shows rewritten chair/reviewer tasks → push mid-council supersedes with
`reason:"superseded"` → reviewer done → chair verdict → ADR 013 columns
populated → ADR 012 close webhook received; plus one solo forum-shaped session
via POST /v1/sessions (with prompt) + /messages; plus mention-grammar `/ask`
answer. Prod publishes only after the dev lane passes the full list.

## 4. What each mechanism closes

| Mechanism | Finding closed | Governing ADR |
|---|---|---|
| Done-policy hooks (S6) + recipient_trigger_text (S7) + structured_verdict (S8) | **B1** — mode-mapped policy inlined in orchestrator | ADR 007 (kernel/policy split), ADR 013 (verdict interpretation ownership) |
| Interpreter-owned supersede cleanup (S4), PostMessage + atomic prompt (S5), mode-enumeration test (S9) | **B3** — controller bypass / caller-remembered side effects / whitelist drift | ADR 008 |
| /bot-config freeze test (S2), demotion runbook + warn log (S17) | **B2** — /bot-config scope creep | ADR 010 |
| issue() externalization + operator token (S14), graceful default flip (S15) | **D1** — token externalization half-state | ADR 016 |
| Plugin module moves (S10-S12) + grep gate | Stage 3 extraction proper | ADR 007 |
| Fail-loud dispatch (S9) | C4-class silent 1-bot hang | ADR 007 (dispatch safety ruling in ADR 018) |

## 5. /bot-config: freeze + demotion (B2, ADR 010)

**Freeze first (S2), demote last (S17), never extract.** The endpoint is
kernel compatibility surface scheduled for deletion — moving its
review-flavored content (agent-profile DSL, chair `[hooks.pre_boot]` gh-auth)
into the plugin would be churn on dead code.

- **Freeze (S2):** golden-snapshot tests convert the ADR 010 B2 freeze from
  declarative to enforced — the drift class already fired once
  (allow_* trust fields, commit 957a547/#60). Lands before any extraction PR
  so the guard covers the whole stage.
- **Demotion (S17 runbook; execution is template-repo-gated, not a Stage 3
  PR):** blocked by the documented B2→D1 coupling (ADR 010:93-98 — production
  configUrl needs the externalized-token story). Order: (a) S14/S15 land;
  (b) both Zeabur templates move agent-profile material and the chair pre_boot
  gh-auth into OpenAB-native per-bot configUrl artifacts (ADR 010 migration
  steps 2/4 — cross-repo PRs, dev lane verified before prod template);
  (c) /bot-config demotes to a compatibility endpoint; (d) removal only on
  evidence — the S17 per-serve warn log is the counter, trigger = no lane
  fetches it for a full release.
- Until removal, the frozen renderer + snapshot + warn log are the only
  mechanisms preventing expansion — exactly what B2 asked for. Note honestly:
  configUrl behavior under Zeabur file-mount semantics is **unverified** (the
  C3-deferred class); that is why demotion execution sits outside this stage's
  exit criteria.

## 6. ADR 016 finish (D1)

In ADR 016's own blocker order:

- **Blockers 1+2 (S14):** issue() honors externalize_tokens() — the
  "API-registered bot is unbootable under the flag" half-state dissolves; the
  plaintext token is returned exactly once in the POST /v1/bots response
  (ADR 016's Neutral clause — API-registered bots never needed /bot-config for
  their token). Operator-supplied token on registration means the plane can
  learn only the hash.
- **Blocker 3 (S15):** default flips to externalized, **gracefully** — the
  boot-time legacy-DB guard implements ADR 016's "flip the default for new
  deployments" without bricking an in-place third-party upgrade (env unset +
  token_plain rows → legacy mode + loud warning; a hard boot failure here
  would misread the ADR). Both owned lanes set =1, so the flip is a verified
  no-op there.
- **Blocker 4 — token_plain drop: STAYS DEFERRED**, and ADR 018 says why: the
  column **cannot** drop while legacy plaintext mode is supported —
  bot_token_plain (store.rs:698-703) is what serves that mode. Named trigger:
  removal of legacy plaintext mode itself, one release cycle after the
  default-on warning ships. Drop pattern pre-committed: the connected-column
  soft-drop precedent (remove from fresh SCHEMA, stop reading/writing,
  tolerate in legacy DBs, never ALTER DROP) — avoiding the first-ever hard
  column drop on this migration machinery.
- Out of scope per ADR 016's own scope rules: endpoint auth (OpenAB cannot
  present one), rotation, admin token-fetch API, secret resolvers,
  encrypt-at-rest.

## 7. Deferred and residual — honest ledger

Each residue gets a name, a location, and an exit trigger in ADR 018; none is
silent.

- **Kernel→plugins import direction** (lookup arms): by design, for
  persisted-mode dispatch safety (C4 class). Enforcement of the real boundary
  = S12 grep gate + S9 fail-loud dispatch. Exit: the ADR 008 transport era, if
  it ever fires.
- **ADR 013 columns, set_session_verdict, fused /v1/stats aggregates**:
  deprecated-in-place; interpretation extracted (S8), storage moves at M4 into
  a plugin-owned pr_review_findings side table created via a future guarded
  `Store::run_plugin_migration` hook after kernel migrate() — designed in
  ADR 018, zero code now. Rule binding M4: new findings state goes to the
  plugin table, **never** new sessions columns.
- **Wire surfaces keep ADR 013 vocabulary** (sessions JSON, verdict SSE, close
  webhook, /v1/stats): frozen v1 kernel contracts, snapshot-pinned (S3);
  evolution owned by the M4 ADR with a versioning decision there.
- **github_app.rs Role→write/read map**: review policy in a kernel
  credential-mint signature. Exit sketch recorded in ADR 018 so the residue
  has a concrete door: `POST /v1/sessions/:id/github-token` gains an explicit
  requested-scope input validated against coordinator policy; trigger = second
  GitHub-writing plugin.
- **Watchdog created_at-anchored reopen trap**: a reopened session
  force-closes within ~30s of the stale timeout. Not fixed this stage (belongs
  to the chat-mode coordinator arm), but no longer a footnote: S13 pins it
  with a regression test, and S1 writes the **hard forum-v1 contract** into
  the forum plan — fresh session after close, follow-ups only within the
  timeout window. Fix trigger: forum dogfood shows session-per-turn churn cost
  (boundary review's chat-mode ruling).
- **Plugin config is process-global env** (OABCP_ALLOWED_REPOS,
  OABCP_BOT_HANDLE, review caps), with test correctness resting on same-crate
  env locks. Recorded in ADR 018 as a **named precondition for any future
  crate/workspace split**: config injection first, split second.
- **Per-north-client auth scoping**: the single OABCP_API_KEY means the forum
  proxy credential can touch every session including PR-review councils.
  Decision recorded in writing (shared-flaw requirement): Stage 3 **accepts
  the blast radius** — forum's proxy is first-party and lane-local — and names
  the trigger for a scoped second bearer: the first non-first-party north
  client, or forum leaving the owned lanes.
- **B2 demotion execution** (template configUrl): outside exit criteria,
  unverified Zeabur file-mount semantics (§5).
- **quorum_n**: stays kernel per the boundary review; moves when a second
  coordinator config lands.
- **close_reason durable column**: reserved to the CloseSession verb design
  (pr-mention-plan §8 trigger), decided in the M3/M4 ADR — named open
  question, not silently decided.

## 8. Explicit non-goals

- **M4 findings ledger** — built later **on this seam** (route.md Phase-2
  item 2): ADR 018 records the plugin-owned-table design and the
  run_plugin_migration hook; no table, no status/resolve-Fn/help commands, no
  EmitStatus implementation this stage.
- **Stage 2 typed OAB wire** (dedup flags, context flag, typed done/verdict
  directives) — upstream ADR to OpenAB; out of this repo's control.
- **ADR 008 external-controller transport** — dormant: HTTPS protocol, event
  signing, action tokens, quotas, manifest enforcement, controller_* tables,
  conformance harness. Trigger: a plugin needing independent deploy cadence or
  a third-party controller author. The reserved verbs + serialized shapes in
  ADR 018 keep externalization a transport change.
- **Cargo workspace / separate crates** — module split only (§7 precondition).
- **Schema changes of any kind** — including verdict-column moves, stats
  split, trigger_ref/fingerprint format changes, token_plain drop.
- **Moving scripts/*.tmpl out of scripts/** — prompt-identity contract.
- **Roster mutation via the interpreter** — reserved AddRoster verb; direct
  orchestrator calls stay until a reconciler needs them.
- **Forum chat-mode coordinator and the watchdog anchor fix** — trigger named
  in §7.
- **Multi-installation GitHub App support** — documented Phase-2 gap.
- **Building any forum client code in this repo** — forum is a pure external
  consumer; Stage 3 guarantees its ingress exists and is proven (S16).

## 9. Exit criteria

Stage 3 exits when **all** of the following hold, in order:

1. **CI gates standing:** S2 bot-config snapshot, S3 wire-shape snapshots, S12
   grep gate, S9 mode-enumeration test all green as permanent jobs on main.
2. **Kernel purity, mechanically checked:** api.rs / orchestrator.rs /
   coordinator.rs / controller.rs contain no `PR Review` / `gh pr` /
   `[[verdict` / `TRIAGE` strings (grep gate), and coordinator.rs:266's
   "only place a mode maps to policy" claim is true (B1 closed).
3. **Live-verified on the dev lane** (openab-council) at publish point 4: the
   full §3 smoke checklist passes — auto-convene → task rewrite → push
   supersede → verdict columns → close webhook — plus the solo forum-shaped
   session; then prod (opencodezebra) upgraded and the same checklist passes
   there.
4. **Forum-support routable through the seam without grandfathering:** the
   S16 second-consumer test passes using only trait defaults and zero
   `plugins::` imports — open(solo, prompt) → verbatim delivery → done →
   text-only close → follow-up per S13 policy → close — and
   docs/forum-north-client-plan.md (revised in S1) prescribes exactly that
   surface: `controller::execute` ingress only, fresh-session-after-close
   rule, no raw two-call open.
5. **D1 closed:** both lanes verified on externalized tokens post-flip; the
   legacy-DB fallback fixture test green; ADR 016 Migration Status current.
6. **No open behavior regressions:** the pinned integration tests
   (chair-auto-🆗, TRIAGE gate, trigger round-trip, reopen) pass in their
   final (moved) locations with unchanged assertions.

## 10. Doc updates required

| Doc | Change | When |
|---|---|---|
| docs/adr/018-stage3-extraction.md | NEW — this plan's rulings (§2 surfaces + serialized shapes, verdict-column resolution, residue ledger §7, M4 seam, crate-split precondition, no-logic-edits move rule) | S1 |
| docs/adr/007, 008, 016 | Status: proposed → **accepted-as-amended**, ratified **before** code PRs land | S1 |
| docs/forum-north-client-plan.md | Rewrite against the S5 surface: prompt field (no two-call open), dedupe/supersede response handling, S13 reopen semantics, fresh-session-after-close hard rule — **before forum Phase 1 starts** | S1 |
| docs/design.md | :126-133 stale supersede text; layer-map row for the plugin boundary; :35 sunset-condition status | S1, S17 |
| docs/roadmap.md | Stage 3 rows: B1/B3 → closed, B2 → frozen+runbook, D1 → done; M4 row points at the ADR 018 seam | S17 |
| docs/route.md | Phase 2 item 1 (Stage 3 extraction) marked done at exit; M4 remains next | exit |
| docs/adr/010 | Amendment: demotion runbook + removal trigger (per-serve warn-log evidence) | S17 |
| docs/adr/016 | Migration Status updated per S14/S15; blocker-4 named trigger + soft-drop pattern | S14/S15 |
| docs/boundary-review-2026-07.md | Addendum note: :691 "verdict columns move with the Stage 3 plugin" amended to the ADR 018 ruling (interpretation now, storage at M4) | S1 |
| docs/steering/pr-review.md | No content change; steering_sync.rs extension (S10) pins the task-prefix contract | S10 |
| MEMORY / release notes | S8 (non-review trailer parsing stops), S13 (non-solo reopen → 410), S15 (default flip + legacy guard) each release-noted loudly | per PR |
