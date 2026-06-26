# Coordinators — pluggable coordination patterns

Status: increments 1–3 implemented (`src/coordinator.rs` + `orchestrator.rs`):
`QuorumCouncil`, `Solo`, `Pipeline`. Tracking: ROADMAP Phase 3.

## Why

OCP's value is a coordination primitive, but today exactly **one** pattern is
welded into the orchestrator: convene → quorum(🆗) → chair synthesizes → close.
The product needs **multiple modes** (council, debate, pipeline, solo, …), so the
coordination *policy* must become pluggable while the *mechanism* stays shared.

This also closes two scope leaks of the single hardcoded flow:
- the chair-prompt string `"Quorum reached. Chair, please render the verdict."`
  is hardcoded English prose injected by the engine;
- `output.rs` (`verdict → gh pr comment`) is review-specific side-effect welded
  into the close path — a violation of [design scope](design.md) (PR logic is the
  app shim's job, not the plane's).

## Mechanism vs Policy

The split is already clean in `orchestrator.rs`:

| Mechanism (stays in orchestrator, mode-agnostic) | Policy (moves behind a Coordinator) |
|---|---|
| roster auth, closed-gate, command dispatch (`handle_reply`) | what a done-signal (🆗) means |
| store message, fanout, emit north, ack (`on_send`) | when/whether to relay a bot's final to another |
| thread upsert (`on_create_topic`) | when quorum is reached + what message prompts whom |
| deliver the trigger to the roster (`post_client_message`) | *who is mentioned* (prompted to act) on the trigger — `starters` (council: all; pipeline: stage 0) |
| store reaction, emit, ack (`on_reaction`) | what closes the session + what the verdict is |
| edit (`on_edit`), backfill (`add_to_roster`), `deliver_event` | — |

All current policy hangs off **one block**: `on_reaction` lines 228–232, on a
`DONE_EMOJI` add — `share_final_with_chair` + `maybe_quorum` + `maybe_close_verdict`.

## The seam

The orchestrator runs the mechanism, then asks the Coordinator what coordination
actions to take. The Coordinator decides from read-only accessors; the
orchestrator executes the writes (keeping CAS guards for once-only correctness).

```rust
/// Read-only view a Coordinator decides from (pure-ish → unit-testable).
trait Ctx {
    fn roster(&self) -> &[String];
    fn chair(&self) -> Option<&str>;
    fn quorum_n(&self) -> i64;
    fn reactors(&self, emoji: &str) -> Vec<String>;     // distinct bot ids
    fn latest_settled(&self, bot: &str) -> Option<String>; // last non-stub message
    fn state(&self) -> SessionState;
}

enum Action {
    Relay  { from: String, to: String },       // deliver from's settled final to `to`
    Prompt { to: String, content: String },    // deliver a system message to a bot
    Transition { from: SessionState, to: SessionState }, // guarded CAS; emits state
    Close  { from: SessionState, verdict: String },      // CAS from→Closed; emit verdict + closed
}

trait Coordinator {
    /// A settled done-signal (🆗 add) arrived from `bot`. Return actions.
    fn on_done(&self, cx: &dyn Ctx, bot: &str) -> Vec<Action>;
    // future: on_reply / on_join for patterns that need them (debate rounds, etc.)
}
```

The orchestrator change is surgical — replace lines 228–232 with:

```rust
if add && emoji == DONE_EMOJI {
    for action in coordinator.on_done(&cx, bot_id) { execute(state, session, action)?; }
}
```

`Close` emits the `verdict` + `state:closed` north events and **nothing else** —
`output::fire` is deleted. The verdict SSE event is the seam any side-effect
subscriber (the chair bot already does `gh pr comment`, or an app shim) acts on.

## Flow — before vs after the seam

Same `🆗` done-signal, two control flows. The shift is **decide vs execute**:
before, the orchestrator decided *and* executed inline; after, the Coordinator
decides (returns `Action`s) and the orchestrator only executes them.

```
BEFORE — policy welded into the orchestrator
────────────────────────────────────────────
  🆗 done-signal
       │
       ▼
  on_reaction
   ├─ share_final_with_chair   ← knows: chair
   ├─ maybe_quorum             ← knows: quorum_n, prompt text
   └─ maybe_close_verdict      ← knows: chair, Quorum→Closed
       │  (decides AND executes, inline)
       ▼
  store · deliver · emit · advance_state

  New mode ⇒ edit these functions. One welded flow.


AFTER — policy behind the Coordinator seam
────────────────────────────────────────────
  🆗 done-signal
       │
       ▼
  on_reaction ──build──► Ctx (read-only store view)
       │
       ▼
  Coordinator.on_done(cx, bot) ─► [ Relay, Transition, Prompt, Close ]   POLICY (decides)
       │                            QuorumCouncil │ Solo │ Debate │ …
       ▼
  run_actions(actions) ─────────► store · deliver · emit · CAS           MECHANISM (executes)

  New mode ⇒ new Coordinator. run_actions untouched.
```

Worked example — `rev1` 🆗 reaching quorum, then `chair` 🆗 closing:

```
rev1 🆗 → on_done ⇒ [ Relay{rev1→chair}, Transition{Delib→Quorum}, Prompt{chair} ]
           run_actions: relay · CAS ok→emit "quorum" · prompt chair

chair 🆗 → on_done ⇒ [ Transition{Delib→Quorum}, Prompt{chair}, Close{Quorum} ]
           run_actions: CAS FAILS (already Quorum) → transition_failed
                        Prompt SUPPRESSED (no re-prompt)
                        Close CAS Quorum→Closed ok → emit verdict + closed
```

Adding **Solo** (fixes 1-bot) touches no orchestrator code — just a new impl:

```rust
impl Coordinator for Solo {
    fn on_done(&self, cx: &dyn Ctx, bot: &str) -> Vec<Action> {
        // the lone bot's 🆗 closes directly — no quorum gate
        vec![Action::Close { from: Deliberating, verdict: cx.latest_settled(bot).unwrap_or_default() }]
    }
}
```

## QuorumCouncil — the parity port

The existing flow becomes the first impl. Exact mapping (preserves behavior):

```rust
impl Coordinator for QuorumCouncil {
    fn on_done(&self, cx, bot) -> Vec<Action> {
        let mut a = vec![];
        let chair = cx.chair();
        // 1. relay reviewer's settled final to the chair (share_final_with_chair)
        if Some(bot) != chair {
            if let Some(c) = chair { a.push(Action::Relay { from: bot.into(), to: c.into() }); }
        }
        // 2. quorum reached → enter Quorum + prompt chair (maybe_quorum)
        if quorum_reached(cx.roster(), chair, &cx.reactors(DONE_EMOJI), cx.quorum_n()) {
            a.push(Action::Transition { from: Deliberating, to: Quorum });
            if let Some(c) = chair {
                a.push(Action::Prompt { to: c.into(),
                    content: "Quorum reached. Chair, please render the verdict.".into() });
            }
        }
        // 3. chair's own done in Quorum → close with its final as verdict (maybe_close_verdict)
        if Some(bot) == chair {
            a.push(Action::Close { from: Quorum, verdict: cx.latest_settled(bot).unwrap_or_default() });
        }
        a
    }
}
```

The CAS guards (`Transition`/`Close` execute via `advance_state`) keep the
once-only + correct-ordering semantics the current code relies on, so a single
`on_done` can safely emit both a quorum transition and a close — each fires only
from its required prior state.

## Goal — the completion condition

Completion must be a **defined goal**, not a hardcoded "quorum_n reached". The
session carries a goal; the active Coordinator evaluates it to decide when to
`Close`. `quorum_n` becomes one kind of goal, not the only knob.

```rust
enum Goal {
    Quorum(i64),            // N reviewers signal done (today's behavior)
    AllAngles(Vec<String>), // every assigned angle reported (review presets) — not a fixed N
    Rounds(u32),            // K debate rounds completed
    AllStages,              // every pipeline stage done
    // opaque/predicate goals → interpreted by a WebhookCoordinator
}
```

The Goal is interpreted by the Coordinator that owns the session, not by the
engine — `QuorumCouncil` understands `Quorum`/`AllAngles`, `Debate` understands
`Rounds`, etc. Exposed via `Ctx::goal()`. The engine only executes the resulting
`Close` action; *what* satisfies completion is the Coordinator's call.

## OAB contract — unchanged (hard invariant)

The entire refactor is **plane-internal**. Stock OAB pods must keep working with
no change:

- Coordinators only **recombine existing mechanism primitives** (`deliver_event`,
  `emit_north`, `advance_state`, reactions). They introduce **no new gateway wire
  message types** and require **no OAB-side change**.
- The done-signal stays the OAB-default `🆗` (`emoji_done`); the gateway protocol
  (`GatewayEvent`/`Reply`/`Response`) is untouched.
- Guardrail: `tests/spike.rs` drives **mock bots over the real gateway wire
  protocol** — if any coordinator needed a protocol change, those tests break.
  Every increment keeps 1/3/5-bot spike tests green.

If a future mode genuinely needs a new bot-facing capability, that is an OAB
upstream change tracked separately — **not** smuggled into a Coordinator.

## Mode selection

A `mode` string on the session (default `"council"`), chosen at
`POST /v1/sessions`. `for_session(mode) -> Box<dyn Coordinator>` dispatches:
`"solo"` → `Solo`, else `QuorumCouncil`. The `mode` column landed in increment 2
(additive `ALTER`, so existing DBs migrate). A 1-bot deploy opts in by sending
`"mode": "solo"` — see `scripts/open-council.sh` / the template.

## Other modes (sketches — prove the seam is general)

- **Solo** — fixes the 1-bot case (today a lone chair never closes: quorum counts
  reviewers = roster minus chair = ∅). `on_done`: the sole bot's 🆗 →
  `Close { from: Deliberating, verdict: latest_settled(bot) }`. No quorum gate.
- **Debate** — N rounds: each 🆗 advances a round counter; `Prompt` all members
  with the others' finals; close after K rounds or convergence. Needs `on_reply`.
- **Pipeline** (implemented) — sequential handoff A→B→C: each bot's 🆗 → `Relay`
  to the next in the ordered roster + `Prompt` it; close when the last finishes.
  Order from roster position; `starters` mentions only stage 0 on the trigger.

## Webhook coordinator (future, no architectural fork)

External coordination is just **another impl behind the same trait**: a
`WebhookCoordinator { url }` POSTs the event and parses returned `Action`s. So
"native trait vs external webhook" is not a fork in the architecture — native
impls ship first (deterministic, fast, self-contained); webhook is added when a
user needs to define coordination outside the plane. Cost: a network round-trip
per event on the hot path — opt-in only.

## Implementation increments

1. ✅ **Extract the seam + QuorumCouncil + drop `output.rs`.** Done: `Ctx`/`Action`/
   `Coordinator` in `coordinator.rs`, `on_done`→`run_actions` in `orchestrator.rs`;
   `output.rs` removed. Mechanism untouched → 1/3/5-bot spike tests prove parity.
   No schema change — QuorumCouncil reads `quorum_n` directly (== `Goal::Quorum(n)`).
2. ✅ **`mode` selection + Solo.** Done: session `mode` column (default
   `council`, additive migration), `for_session(mode)` dispatch, `Solo` impl;
   `tests/spike.rs::solo_single_bot_closes` proves the 1-bot close over the wire.
   **`goal` column deferred** — `QuorumCouncil` reads `quorum_n` directly and
   there is no second completion condition yet (design.md disposition:
   speculative policy → cut; add `Goal` when a real consumer lands).
3. ✅ **`Pipeline` — a structurally-different (non-fan-in) mode.** Done:
   sequential handoff stage0→…→stageN, `tests/spike.rs::pipeline_three_stages_closes_in_order`
   proves in-order close over the wire. Generalization cost was **smaller than
   predicted**: it needed a `starters(roster)` kickoff hook (so only stage 0 is
   @mentioned on the trigger; others wait), **not** `on_reply`/`on_join` or a
   `Goal` enum — `on_done` + ordered roster (`ORDER BY rowid`) sufficed. Debate
   (multi-round) is the mode that would force `on_reply` + round state; build it
   only when actually needed.

Every increment keeps the 1/3/5-bot spike tests green (OAB contract guardrail).

## Risk

The close path's streaming/edit-fill timing (`maybe_close_verdict` reads the
*settled* final, not the stub) is subtle and live-proven. Increment 1 must keep
it byte-identical — guarded by the existing `spike.rs` integration tests
(1/3/5-bot to closed verdict with streaming edits).
