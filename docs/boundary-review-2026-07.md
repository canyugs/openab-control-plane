# Boundary review 2026-07 — OCP ↔ OAB separation of responsibilities

Status: review · 2026-07-07 · method: 4 independent code/doc/history mappers →
4 critics → dedupe → per-finding adversarial verification (15 confirmed /
9 confirmed-with-corrections / 0 refuted). Planning only — no implementation.

Mission statement judged against: **"OCP exists to guarantee stability and
determinism of interactions among tens to hundreds of OABs."**

Paths: bare = this repo; `oab:` = the OpenAB repo (`crates/openab-core/src/`
unless stated). Line numbers are from the verification pass.

## Verdict

Is the responsibility split clear? **The declared boundary is unusually clear
and mostly correct.** The scope tables in [design.md](design.md), the guarantee
test ("must it hold even if a bot is slow, dead, buggy, malicious, or
hallucinating?"), and [ADR 001](adr/001-three-planes.md)/[010](adr/010-openab-configurl-boundary.md)/[016](adr/016-gateway-token-externalization.md)
survive audit against source: the delivery spine, the CAS safety kernel, the
watchdog, substrate acceptance, and the config/credential retreat are
load-bearing and right (next section). Failures elsewhere degrade to "hangs,
then the watchdog terminates" — not corruption.

The verified drift concentrates in three places:

1. **The south protocol carries semantic meaning on cosmetic/text
   conventions.** Quorum votes ride OAB's per-turn status emoji; `[done]`,
   `[[verdict:]]`, `[[recruit:]]` are unanchored substring conventions;
   delivery is at-least-once into a consumer with no dedup and an active
   backfill. A determinism gap on the exact axis the mission names — already
   three mode-string carve-outs in core plus the C2 incident.
2. **The PR-review application is compiled into the kernel** despite
   design.md:47's own disclaimer ("an app on top of OCP, not part of it").
3. **Several scale mechanics have known knees before 100 bots** — default-
   journal SQLite behind one mutex, an unindexed unbounded messages table, a
   lag-blind global SSE channel, env+restart membership ops, a presence
   dual-write that manufactures zombies.

None of this needs a redesign. Every fix lands inside existing seams
(`Coordinator` trait, `controller.rs`, the rendered-config retreat) or as one
small typed extension to the OAB wire protocol.

## What holds (keep as-is)

| Keep | What it is | Evidence |
|------|-----------|----------|
| Delivery spine + safety kernel + chokepoint (high) | Durable outbox (UNIQUE `idem_key`, `INSERT OR IGNORE`, replay-on-connect), CAS `advance_state`, once-only close, roster gate, post-close drop, watchdog, `controller.rs` interpreter. Incident-hardened — C2/C4/C7/C8 each made it strictly more correct without widening it. | store.rs:401-404, :1330-1341; orchestrator.rs:860-867; controller.rs:39-115; spike.rs wire tests |
| ADR 001 one-process stance + substrate acceptance + no-GitHub-I/O convene (medium) | One process, one SQLite; escape hatches pre-designed not pre-built (`WebhookCoordinator`; networked `Store`, main.rs:17-19); pointer trigger does zero GitHub I/O; webhook ingress HMAC fail-closed. What a broker/K8s reflex would "fix" first; every post-mortem says the shape was right. | adr/001; council.rs; github_webhook.rs |
| Scoped GitHub token mint (low) | Scope derives from the bot's *stored* role, roster-bound per session, chair-write/reviewer-read physically split, mint lock kills the double-mint race. ADR 009 made structural — the counterexample to prompt-enforced guarantees. | design.md:46; adr/009; github_app.rs |
| Config/credential retreat direction (medium) | ADR 010/016: the only seam where every fix worked by *shrinking* OCP, zero repeat incidents. No new `/bot-config` fields, no OCP-hosted steering. Queued deletions filed under B2/D1. | adr/010, adr/016 |
| Split-by-guarantee framing (low) | Confirmed: the first plane to hit its limit (data-plane store) is cured by PRAGMAs + an index + purge-on-close, not a process split. Only gap: the split test is unfalsifiable; `store::stats` already computes the dials. Armed in Stage 0. | adr/001; /v1/stats |

## Verified gaps

Corrected claims (9 findings) appear in corrected form. Incident names like
C7/#94 refer to past incidents, not these finding IDs. Fix directions are
summarized here; staging and ownership detail in the plan below.

### A. Determinism on the south seam

One defect class throughout: **semantic load on conventions the other side
treats as cosmetic or free-form.**

**A1 — Done-signal aliasing (confirmed · high).** OAB auto-emits 🆗 after
*every* completed turn as UI status (oab:config.rs:971-972, dispatch.rs:786);
OCP counts any roster 🆗 anywhere in the session as a quorum vote
(orchestrator.rs:966-968; store.rs:1316-1325 counts DISTINCT over all session
messages), and text `[done]` is an unanchored `ends_with` (:1001-1003).
Failure: a reviewer completing any incidental turn (peer ack, backfilled
prompt) votes done before its review exists — already forced three
live-incident carve-outs into core (chair exemption :972-981 "live-hit twice";
TRIAGE prefix :1019-1032 "hit on dogfood rounds 2 and 5"); each new mode needs
its own. Fix: OCP counts 🆗 only when it targets the client/system trigger
message (Stage 1); OAB typed `[[done]]` deletes the aliasing (Stage 2).

**A2 — "No message lost" is unreachable end-to-end (confirmed · high).**
design.md:112 claims it plane-held; the channel is at-least-once with three
duplicate sources and one loss source, and no layer currently dedups:
`ack_outbox` DELETEs the row so `idem_key` dedups only while pending
(store.rs:1352-1356) — `relay_settled` re-delivering post-ack is a reachable
duplicate (orchestrator.rs:~1170-1195); `flush_outbox` is unserialized
read→send→ack invoked concurrently (state.rs:138-150; ws.rs:58, :106); ack =
handed-to-unbounded-mpsc, not socket-confirmed (state.rs:126-132;
roadmap.md:160 admits it); a fresh `event_id` is minted per delivery
(state.rs:177) and OAB has no dedup at all (oab:gateway.rs:830-834;
custom-gateway.md:314). Every duplicate is a full agent turn — duplicate public
speech, spend, done-signals (the C2 residue). Fix: OCP per-bot flush
serialization + delivered-marker instead of ack-DELETE (Stage 1); OAB bounded
per-thread dedup on `(channel.id, message_id)` — OCP already keeps `message_id`
stable across copies (Stage 2); design.md:112 downgrades to "at-least-once
(duplicates possible at OAB)" until fixed (Stage 0).

**A3 — Silent death loses acked frames; no WS ping (confirmed · medium).**
Neither endpoint pings (ws.rs:35-108; oab:gateway.rs:1040-1115; roadmap.md:100
defers it), so a silently-dead TCP conn keeps accepting sends: frames are acked
out of the durable outbox and die in the kernel buffer — at-most-once,
permanently. The sweep skips `connected==true` bots (orchestrator.rs:354-405)
so no replacement fires; a stalled-open consumer grows the unbounded channel
(state.rs:13) without bound. Dogfood already restarts the plane only for this.
Fix: OCP WS ping with pong deadline that closes the socket, routing silent
death into the already-hardened disconnect path (~15 lines; Stage 1);
socket-confirmed acks fold into Stage 2.

**A4 — Backfill is active, per-message, audience-blind (confirmed · high).**
`backfill_bot` replays the *entire* history to a joiner
(orchestrator.rs:752-777) including system prompts whose targeting was never
persisted (:1198-1241; no audience column, store.rs:1220-1236); OAB's default
dispatch is per-message and the rendered config never pins
`message_processing_mode` (api.rs:1194-1220; key exists, oab:config.rs:568) —
the comment "OAB batches the in-thread burst" (orchestrator.rs:657-661) is
false for the default config (roadmap.md:161). Failure: a replacement reviewer
receives "Quorum reached. Chair, synthesize…" (coordinator.rs:85) and may run
the chair's task; a joiner burns O(history) agent turns whose replies fan to
everyone; a plane-restart mass-replace at 100 bots is conditional (downtime >
60s grace, spare available, done-voters exempt) but real, bounded only by the
watchdog. Fix: OCP persists per-message audience, backfills broadcast +
own-audience only, caps backfill, pins the dispatch mode (Stage 1); OAB
`context: true` flag — "load, don't react" (Stage 2).

**A5 — Closed-session outbox frames replay as live prompts (confirmed ·
high).** State gates at enqueue only (state.rs:164-174); reconnect flush
replays every pending frame with no state filter (store.rs:1344-1350 selects by
bot_id only; ws.rs:58); neither close path purges — only trim/replace do
(orchestrator.rs:466, :742 vs Close :1111-1162, watchdog :280-345). Scenario: a
chair pod down over a weekend while its councils watchdog-close replays every
stale "synthesize the verdict…" prompt on Monday; replies are dropped by the
closed gate — pure token burn plus quota exhaustion throttling live sessions.
Exposure survives the sweep's purge: done-voters, chairs with no spare,
`OABCP_LIVENESS_GRACE_SECS<=0` (main.rs:86-95). "Post-close drop" holds on the
ledger, not at the bot's eyes. Fix: OCP purges the session's outbox on both
close paths — one DELETE, deleting state rather than adding mechanism
(Stage 1).

**A6 — Quorum votes are erasable rows with an unreachable fallback target
(confirmed · medium).** A done-vote is a `reactions` row; `remove_reaction`
unconditionally DELETEs it (store.rs:1288-1294) and quorum is recomputed per
event (coordinator.rs:67-83) — OAB's cosmetic `remove_after_reply` flag
(default false, oab:config.rs:859, :1009) would arrive as `remove_reaction 🆗`
and silently regress a not-yet-reached quorum to the watchdog: a cosmetic OAB
flag changes plane liveness. Separately, a reaction naming no message falls
back to `session.id` as target (orchestrator.rs:950-952), which never matches
the `JOIN messages` (store.rs:1316-1325): accepted, acked, never counted
(latent — OAB always sets `reply_to` today). Fix: OCP makes counted done-votes
monotonic, rejects/logs unresolvable targets, pins `remove_after_reply=false`
in the rendered config — same pattern as the `allow_all_users` pin
(api.rs:1207) (Stage 1).

**A7 — `[[recruit:]]` substring-matched anywhere in chair text (confirmed ·
medium).** `parse_recruit` is `text.find("[[recruit:")` — first occurrence, no
line/fence/quote anchoring (orchestrator.rs:781-787); authority is
author-anchored only (:792-794) while the chair's job is synthesizing content
the prompts call untrusted (:96). A quoted `[[recruit:x]]` executes with chair
authority; an unknown id emits `provision_requested` north with the
attacker-controlled string, un-rate-limited (:801-848) — a
prompt-injection-to-infra signal path. Bounded today: admission caps known bots
(max 16, :649-655), provisioner.md:36-54 mandates catalog validation
consumer-side, and no reference provisioner exists yet. Gap: parses only in
`on_send` (:922), never on streamed-edit finalization. Fix: OCP anchors to a
directive-shaped position (own line, outside fences, leading header block per
OAB's `[[key:value]]` rule) + per-session rate limit (Stage 1); typed directive
upstream (Stage 2).

**A8 — `[[verdict:]]` last-occurrence-wins lets quoted trailers override the
decision (confirmed · medium).** `rfind`, deliberate and unit-tested ("chair
quoted an earlier draft", orchestrator.rs:1299-1303), no fence/line anchoring
(:507-533). Author anchoring is correct (only `latest_settled(chair)` at Close,
coordinator.rs:145); intra-message anchoring is absent; malformed → whole
trailer NULL (:1127-1148) — so a malformed *quoted* trailer after the real one
NULLs the stored verdict entirely, mitigated only by a warn. The structured
record ([ADR 013](adr/013-decision-review-state.md)) silently diverges from the
prose verdict on the PR; ADR 013:46-58 never says which occurrence is
authoritative. Fix: OCP accepts the trailer only on the final non-empty line
(chair steering already puts it on the `[done]` line), skips fenced blocks,
keeps fail-to-NULL (Stage 1); ADR 013 documents the rule (Stage 0).

**A9 — Pre-thread mention gate silently drops the opening trigger for
non-starters (confirmed · medium).** The trigger goes out before any thread
exists, so OAB's group mention gate applies (oab:gateway.rs:73-81) and a skip
is a bare `continue` — gone, not buffered (:832-834). Non-starters get
`mentions=[]` (orchestrator.rs:227-231); the comment claiming they "still get
the trigger as context" (:213-214, duplicated at coordinator.rs:49-50) is false
for stock OAB. In plain council the chair is a non-starter
(coordinator.rs:106-112) and never sees the task — it survives only because
Relay later delivers in-thread. Mentions key on display name with no
UNIQUE(name) (api.rs:1209; store.rs:330-339): corrected — collisions can
cross-*fire* on shared-mentions fanout (orchestrator.rs:160-173), not
cross-drop the trigger. Fix: OCP mentions the full roster on the trigger or
re-delivers it in-thread once the topic exists; fix both comments regardless
(Stages 0-1); custom-gateway.md states skipped = dropped, not deferred
(Stage 0).

**A10 — Cross-talk determinism hangs on an undeclared streaming-stub invariant
(corrected · high).** With rendered `streaming=true`, fanout filters the
`""`/`"…"` placeholder (orchestrator.rs:137-153) and `on_edit` never re-fans
(:1236-1253), so Relay-on-done is the de facto only bot→bot channel — a
load-bearing accident documented nowhere. Corrected: OAB streams
unconditionally under this config (oab:gateway.rs:701-707), so
"final-delivered-twice to the chair" requires `streaming=false` (a flip OCP
never renders), and multiplicity also depends on ack timing. The *live* leak:
OAB's session-reset turn streams the placeholder "⚠️ _Session expired, starting
fresh..._\n\n…" (oab:adapter.rs:748-763), which evades the stub filter and fans
to all peers. No bot-turn cap exists on the gateway path (`BotTurnTracker`
lives only in the discord/slack/ambient/feishu adapters) — the N² storm is one
config flip away. Fix: declare the invariant in design.md's substrate section
(Stage 0); make multiplicity explicit — delivered-marker per A2, or stop
fanning bot messages and declare Relay the sole bot→bot channel, matching
observed behavior (Stage 1); OAB gateway-path turn cap, defensible
independently (Stage 2).

**A11 — Trust-layer overlap is prose-coordinated (corrected · medium).** Both
sides filter who may act: OCP's roster gate (orchestrator.rs:860-867) and OAB's
`should_skip_event` (oab:gateway.rs:56-83). OCP switches the bot-side layer off
by rendering `allow_bot_messages=true` with only an in-band TOML comment as
justification (api.rs:1199-1208). Corrected: only `allow_bot_messages` is
strictly required (`allow_all_users` defaults to allow-all when the list is
empty, oab:config.rs:1057-1058), and the Path-B runbook example already
contains both lines — unexplained (bot-operations.md:332-345). Failure: an
ADR 016 Path-B bot omitting the flag drops council-peer speech *after* the
outbox marks it delivered — plane-invisible; the thread just goes silent. Fix:
docs — a checked MUST with the failure mode stated, in the attach runbook and
proposed for OAB's compliance list (absent today, oab custom-gateway.md:389-395)
(Stage 0); the Stage 2 handshake would let the plane reject a config that
black-holes fanout.

### B. App-in-kernel drift

**B1 — PR-review is compiled into the kernel (corrected · high).** design.md:47
assigns PR-specific logic to "application shim or chair bot"; in code: prompt
fabrication with literal `gh` command lines and verdict sections
(orchestrator.rs:34-40, :94-114), presets/angles/template (council.rs, 431
lines), webhook parsing (github_webhook.rs, 666 lines), `/v1/review` (api.rs:47,
:1245-1263); zero plugin-loading code (`src/north/`, `src/south/` empty). Two
done-*policy* decisions live in mechanism — the mode-string arms and TRIAGE
gate (orchestrator.rs:972-981, :1019-1031) — making coordinator.rs:260-262's
"the only place a mode is mapped to a policy" currently false. Corrected: the
forum app has *not* shipped (untracked plan); the app that did ship via the
shim pattern — the [ADR 014](adr/014-triage-panel.md) triage panel — cost
exactly one kernel edit (the TRIAGE arm), so the accurate form is "each app so
far cost a kernel edit"; and "overdue" must be argued against
[ADR 007](adr/007-control-plugins-and-oab-father.md):50-54's own trigger
("another real plugin") — which the triage panel plus the forum plan arguably
now satisfy. Fix: in-process extraction — bundled `plugins/pr_review` whose
only ingress is `controller.rs` actions; a `Coordinator` method
(`reaction_counts_as_done`) moves the mode arms and TRIAGE gate onto the trait
(Stage 3); until then, amend design.md/ADR 007 to declare PR review a bundled
first-party controller (Stage 0).

**B2 — `/bot-config` is full OAB runtime-config rendering — sanctioned but
drifting, no sunset (corrected · high).** `bot_config()` renders a complete
`config.toml` (~375 lines, api.rs:855-1230): provider-credential `inherit_env`
whitelist (:863-872), the `OABCP_AGENT_PROFILES` override DSL (:899-916),
`[pool]` sizing, a chair `[hooks.pre_boot]` gh-auth script with refresh loop
(:1119-1124, :1185-1193). Corrected from "direct violation":
[ADR 010](adr/010-openab-configurl-boundary.md) was written about this exact
code and grandfathers it (Scope Rules; Migration step 1), and step 3 partially
executed (ADR 016, api.rs:1140-1154). The real finding is **drift**: one
genuine post-ADR expansion (957a547/#60, 2026-07-02 — the `allow_*` trust
fields, exactly the coupling ADR 010's "should not add… trust… fields" bullet
targets) and no freeze or sunset condition anywhere. Fix: freeze (bugfix-only),
move profiles + pre_boot to templates/`configUrl` artifacts per ADR 010 steps
2/4, add a sunset condition to design.md:35's row (Stage 0 docs, Stage 3
execution).

**B3 — `POST /v1/sessions` bypasses the action interpreter (corrected ·
high).** The north endpoint calls raw `create_session` (api.rs:421-438) — the
only production caller skipping `controller.rs`'s idempotent-retry path
(:39-79) and *all* of `validate_open_session` (:81-115). Corrected: a buggy
client **cannot** open a duplicate council for an active `trigger_ref` — the
UNIQUE partial index holds regardless (store.rs:430-436; test :1606-1621: raw
INSERT errors → HTTP 500). Real defects: unsatisfiable sessions (unknown bots,
quorum > capacity) whose only backstop is the watchdog; non-idempotent retry
(500 instead of `{session_id, deduped:true}`, forcing every north client to
pre-resolve — forum-north-client-plan.md:88-90, :410); and whitelist drift —
controller.rs:87-90 omits `triage_council` while coordinator.rs:266 dispatches
it, so that mode is openable only via the unvalidated endpoint. ADR 008 has no
numbered "rule 10", but its bundled-controller prose (~:288-292, "must not
bypass validation by writing directly to the store") is violated in spirit on
the plane's own front door. Fix: `open_session` builds
`ControllerAction::OpenSession`; derive the whitelist from
`coordinator::for_session` — design.md step 3, deleting a second weaker path
(Stage 1).

**B4 — "Terminal verdict" and side-effect completion are prompt-enforced
(corrected · medium).** The watchdog guarantees a terminal STATE, not a verdict
or its side effect: the PR-comment is mandated by prompt prose only
(coordinator.rs:85); the trailer is optional (malformed → NULLs); force-close's
only structural consequence is an explicitly fire-and-forget, unsigned,
no-retry webhook (orchestrator.rs:560-585) while
[ADR 008](adr/008-external-controller-protocol.md):40,:101 specifies signed +
retries. Corrected: the docs partly *disclaim* this (design.md:132, :135-137
keep side effects out of the kernel) — the defect is design.md:113's misleading
"always reaches a terminal verdict" wording, not a false plane claim. The
GH_TOKEN gap is narrower than claimed: `inherit_env` is a name whitelist, not
injection (api.rs:863-871); production templates set GH_TOKEN chair-only, so
"writes will fail for you" is structurally true except in the local-dev
PAT-everywhere shortcut (local-development.md:206-217), which self-flags as
steering-reliant. Fix: reword design.md:113 to "the session always terminates",
move verdict-posting to the steering column (Stage 0); if verdict delivery ever
becomes plane-held, harden the [ADR 012](adr/012-session-close-webhook.md)
webhook to ADR 008's posture (Stage 3, optional).

### C. Scale knees before 100 bots

A sixth candidate knee — the global `github_mint_lock` — was not adversarially
verified; see Unverified observations.

**C1 — Default-journal SQLite behind one blocking mutex (confirmed · high).**
Every store call takes one process-wide `std::sync::Mutex<Connection>` with
zero PRAGMAs and no `spawn_blocking` anywhere, so every fsync'd autocommit
blocks a tokio worker (store.rs:519-534; state.rs:154-210 = 4 lock round-trips
+ 2 write commits per connected recipient; fanout orchestrator.rs:136-176).
MEASURED locally: 0.41ms/delivery default vs 0.04ms WAL+NORMAL (10×);
ESTIMATED on a network PVC: 4-20ms → a 15-target fanout holds 60-300ms of
globally-serialized time; knee at ~50-100 plane-wide messages/min — the stated
mission scale. Fix: `PRAGMA journal_mode=WAL` + `synchronous=NORMAL` in
`SqliteStore::open` (~3 lines, ~10×; Stage 1); then, behind a measured trigger:
batch enqueue per fanout, settled cursors, only then the networked `Store`
(main.rs:17-19; Stage 4).

**C2 — messages table: no index, no retention (confirmed · high).** The schema
indexes outbox and sessions but not `messages(session_id)` (store.rs:354-358,
:390-437), and nothing ever deletes messages/sessions rows. `messages()` plans
as SCAN + temp B-tree (EXPLAIN-verified, :1258-1263) on the hottest paths:
latest_settled per done-signal, relay, watchdog close, backfill, and
`reactors_in_session` per done-check and per active session per 30s sweep
(orchestrator.rs:257-260, :362, :757, :1063-1066, :1173; main.rs:97). The DB
monotonically slows with age — a creep, not a cliff. Fix: one line in
`migrate()` — `CREATE INDEX idx_messages_session ON messages(session_id,
created_at)` (Stage 1); retention deferred (Stage 4).

**C3 — Sweep's replace path never re-checks `connected` (confirmed · medium).**
`trim_reviewer` re-checks before acting (the #68 fix, orchestrator.rs:456-461);
the replace arm — `mark_unreachable` → `find_spare` → `replace_roster_bot` —
has no re-check between the stale snapshot (:366-371) and the bare UPDATE
(store.rs:1063-1077; body :691-750). A bot reconnecting inside that window is
flipped unreachable *after* ws.rs unwound it (the wrong value sticks until next
reconnect), evicted, its session outbox purged, and excluded from `find_spare`
fleet-wide (:444). Same odds curve that made incidents C7/C8 real: small
per-sweep probability × 2,880 sweeps/day × reconnects landing at rolling
deploys. Fix: mirror #68 — re-read and bail if `connected==true` at the top of
the replace branch (~5 lines); C7's presence cleanup narrows but does not close
the window — do both (Stage 1).

**C4 — One global SSE broadcast; `Lagged` silently swallowed (confirmed ·
medium).** All north events share one `broadcast::channel(1024)` (state.rs:74,
:219-227); every subscriber parses every global event and filters client-side,
and `r.ok()?` converts `Lagged` into a silently skipped item (api.rs:1322-1339).
ESTIMATED ~20-50s of tolerance plane-wide at 100 busy bots; a stalled watcher
misses a verdict/close with no signal — `open-council.sh --watch` hangs
(scripts/open-council.sh:157-186) and the forum client's SSE proxy inherits the
blindness (forum-north-client-plan.md:206-215). Fix: handle `Lagged` explicitly
— one SSE resync event; clients re-GET `/v1/sessions/:id/log` (exists,
api.rs:34) (Stage 1); per-session channels are the later surgery (Stage 4).

**C5 — Unanimity quorum makes stragglers, and provider quota, the convergence
limit (confirmed · medium).** MEASURED: 4 reviewers + chair converged in ~140s
with zero throttling — but only split across two provider keys; 429s appeared
at N=5 even with the split (docs/eval/scale-knee.md:36-37), so quota binds an
order of magnitude before any plane mechanism. Without a preset, quorum = all
convened reviewers (webhook presets cap at 7, council.rs:54, :186-190 —
unanimity arises only via the manual driver); at p≈5% failure/turn, N=15 →
~54% of councils need a trim or the watchdog, and the timeout is
created_at-anchored with no activity reset (main.rs:53-79). The degradation is
empirically confirmed: N=6 closed at 630s with decision=None
(scale-knee.md:40-42). Fix: mostly keep-as-is on the plane; document the
per-key ceiling in scale-council.md (Stage 0); K-of-N quorum via preset when N
grows — app-layer policy, not core mechanism (Stage 4).

**C6 — Membership ops at bot #50: env + plane restart per change (confirmed ·
medium).** The documented scaling path mutates `OABCP_BOTS`/
`OABCP_COUNCIL_ROSTER` env, which on the RWO-PVC Recreate deployment takes the
plane down per change (scale-council.md:63-66, :124-131; bot-operations.md
Add/Remove both mandate a restart) — every restart re-exposes A5 and C7. Full
identity retirement is hand-run `sqlite3 DELETE` (scale-council.md:81-90;
seeding is INSERT OR IGNORE, store.rs:620); standing-roster removal itself is
env-only. The restart-free path exists and no membership runbook uses it:
`POST /v1/bots`, `/v1/bots/discover`, standing-roster override (api.rs:28-30,
:42; council.rs:44-51); there is no `DELETE /v1/bots/:id`. ADR 001:30's own
"membership plane is weakest". Fix: delete-first — retire the env path from the
scaling runbook, document the APIs as the add/remove flow (`OABCP_BOTS` =
first-boot seeding only), add the small DELETE handler (Stage 1).

**C7 — Presence dual-write + no boot reset: permanent connected=true zombies
(corrected · high).** Presence is written twice: the connection stack in
`AppState` (routing-authoritative, state.rs:104, :112-122) and a persisted
`connected` flag (store.rs:333; ws.rs:40, :93-98 — the write that needed the
incident-C7 generation guard). Boot never resets the column (main.rs:37-49) and
the sweep examines only `connected==false` bots (orchestrator.rs:369-371), so a
bot that dies while the plane is down is a *permanent* zombie: reported live by
`/v1/bots` (api.rs:212-213), invisible to the sweep, and selected by
`find_spare` as a "connected, healthy" replacement that hangs the session to
the watchdog (:441-448). Corrected: initial convene rosters never read the flag
(council.rs:46-51, :161) — the poison is reporting, sweep blindness, and
liveness replacement, not convening; and `state.is_connected` is
`#[cfg(test)]`-gated (state.rs:212-217), so the fix must ungate it. Fix: drop
`set_connected` and the column (keep `last_seen` for grace); read the stack.
Presence then resets on plane restart — the correct semantics; bots redial in
1-30s against 60s grace. Interim one-liner: `UPDATE bots SET connected=0` at
boot. Leaves the store a pure identity registry — the shape a future membership
split needs (Stage 1).

### D. Credential hygiene residue

**D1 — ADR 016 externalization implemented but off by default (corrected ·
medium).** `externalize_tokens()` defaults to false (identity.rs:27-31, "Off by
default — legacy plaintext"); `identity::issue()` persists plaintext
unconditionally, never consulting the flag (identity.rs:19-22); `/bot-config`
is served with "no client auth — the token IS the bot's credential"
(api.rs:52-54; flag gate :1131-1155). This is ADR 016's *declared* step-3
deferral — a tracked gap, not a leak. Corrected: forum Phase 0's Path-B attach
did **not** fire the ADR's trigger — ADR 016 names no second-app condition; the
deferral rests on seed()/test ergonomics and the `POST /v1/bots` plaintext
dependency, neither of which Path B touches (it bypasses `/bot-config`,
bot-operations.md:325). Phase 0 adds evidence only. Exposure is bare/
non-template deployments — shipped templates and dogfood run
`OABCP_EXTERNALIZE_TOKENS=1`. Sharpening from verification: with the flag on,
an API-registered bot is *unbootable* (issue() stores plaintext, endpoint
serves the env ref). Fix: make `issue()` honor the flag; give `POST /v1/bots`
an externalized story; then flip the default for new deployments; then the
`token_plain` column drop (Stage 3). ADR 016's status section gets these as
named blockers instead of open-ended deferral (Stage 0).

## Design alternatives assessed

**The big call: "substrate, accepted not owned" (design.md:62) has expired.**
Right at zero leverage; the ledger flipped: both standing determinism gaps are
OAB-side verbs by OCP's own admission (roadmap.md:160-161); the done-signal has
cost three in-core mode hacks; OAB's `event.v1` schema is a self-declared draft
(oab custom-gateway.md:~391) with reconnect/replay an open question (:314); and
OAB already owns the right primitive — the position-anchored `[[key:value]]`
output-directive parser (oab:adapter.rs:26-113), strictly stronger than every
substring convention OCP hand-rolled, plus its own tech-debt note asking for a
capability handshake (oab:gateway.rs:16-41). One correction the plan must
carry: **this is not an intra-org edit.** OCP (github.com/canyugs) and OAB
(github.com/openabdev) have disjoint maintainer sets — the extension is an
upstream contribution argued with OCP as reference consumer, and acceptance is
not in OCP's control. Consequence: Stage 1 text-fallback hardening ships
regardless, and the proposal stays minimal — the fields with a nameable owner
today (dedup, context flag, then typed directives), NOT the full
handshake/heartbeat/trace_id vocabulary, which fails design.md:13's
name-the-owner test. Compatibility: serde-default fields, `proto=v2` dial
param, old pods degrade gracefully; `[[done]]` needs colonless support or
`[[done:1]]` under the current `split_once(':')` parser.

Verdicts on rejected alternatives (first two not adversarially verified):

- **Message broker (NATS/Redis Streams) as data plane: reject.** Fails
  design.md step 1 — no owner can name why. Broker at-least-once redelivery is
  precisely what a dedup-less OAB cannot tolerate (industrializes the C2
  class); the real bottleneck is the SQLite mutex, which a broker doesn't
  touch. Re-open trigger: horizontal OCP replicas/HA — and evaluate the
  networked Store first even then.
- **K8s-style desired-state reconciler for membership: defer.** `sweep_liveness`
  already is a level-triggered reconciler; with no provisioner there is no
  actuator, so "desired" cannot differ from "attainable". Adopt when a
  provisioner or ADR 009 quota-based auto-replacement lands; it must emit
  actions through `controller.rs` and takes C7's presence cleanup as its
  actual-state prerequisite.
- **ADR 008 external HTTP controller: defer the transport, do the extraction.**
  The forum plan is evidence *against* the HTTPS protocol now (it plans zero
  plane changes), but B1 shows the in-process seam is due by ADR 007's own
  trigger: route PR-review and forum-support through `controller.rs` as bundled
  controllers now; HTTPS externalization waits for its named trigger (a plugin
  needing independent deploy cadence, or a third-party author). The extraction
  makes that a transport change, not a redesign.

## Addendum 2026-07-07 — mention-driven re-review (the Gen-1 council's requirements)

Added after the verification pass; evidence-cited from source but **not run
through the adversarial verify pass** above. Trigger: nuphos PRs are reviewed
today by the *predecessor* stack — the OAB + Discord review council
(`multi-agent-review-ops` webhook + 7 reviewer pods + aggregator), whose
migration onto OCP is the natural next consumer after forum support. Observed
on zeabur/nuphos#350: author comments `@zeabur-review-agent <fix notes>` →
re-review round with prior finding numbers understood → stale rounds marked
"Superseded by a new review trigger."

Product framing: the stated goal of the review product is to **replace
CodeRabbit** (nuphos runs both side by side today). That makes the
conversational loop table stakes, not polish — ADR 011 already names
`@coderabbitai` as the gap it was closing, and the predecessor's observed
author workflow (mention with fix notes → superseding re-review → carried
finding numbers) is the bar an OCP-based replacement is measured against.

What the predecessor actually does (from its source):

- **The mention is not a trigger.** `@zeabur-review-agent` comment-triggering
  is an open TODO (`multi-agent-review-ops/TODO.md:20-25`); re-review is
  push-triggered (`synchronize` + `request-changes` label,
  `webhook/index.js:51-69`). The author's fix-notes comment is prose the
  reviewers *read* on self-fetch — ADR 011's "the thread is the state" stance,
  independently arrived at.
- **Supersession is prose-enforced.** A new trigger PATCHes the stale
  "Processing" comment (cosmetic, `webhook/index.js:98-114`) and the aggregator
  SHA-guards *at synthesis time* (`aggregator-output.md` Step 1) — the
  superseded round runs to completion and self-aborts at the last step;
  reviewers have no guard at all. A skippable prose step holding a
  should-be-structural invariant — design.md's own tell.
- **The re-review loop is the documented cost sink.** ~69% of reviews go
  multi-round; the full 7-agent panel re-runs per push, re-discovering
  carried-open findings (PR #664: 7 rounds, two findings carried 5 rounds) —
  `docs/HANDOFF-commercialization.md:29-31`. Their named fix: delta-only
  review + open-findings state. Their post-mortem list (prose quorum handoff
  missed, in-memory dedup lost on restart, no re-review protocol) re-derives
  OCP's guarantee thesis item by item.

Gap vs OCP today, and where each lands on the boundary:

**M1 — Supersede-vs-dedupe on the review path (mechanism → plane).** A
`synchronize` arriving while a council is active is *swallowed* — the active
session is returned as `deduped` (github_webhook.rs:159, :341-350;
store.rs:430-436 UNIQUE active trigger_ref), GitHub never re-delivers, and the
in-flight council verdicts on the **stale head**. The predecessor's semantics
(supersede stale rounds) are correct for reviews; its enforcement (prompt) is
what the plane exists to replace. Direction: same-delivery retry still dedupes;
a new-head/new-command trigger CAS-closes the active session
(`reason=superseded`, north event, outbox purged per A5) and opens the
successor under the same trigger_ref index — no half-open window. Which events
supersede is coordinator/controller **policy**; the atomic close-and-succeed is
**mechanism**.

**M2 — Mention → panel re-review with delta context (policy/steering →
controller).** OCP's mention path is solo Q&A only (ADR 011); a full re-review
needs `/review`, and nothing carries prior-round findings into it beyond
self-fetch. The predecessor's cost data says delta-review is the feature that
matters. Direction: controller-owned (Stage 3) — the pr-review controller
distinguishes ask/re-review intents and renders the re-review trigger with the
open-findings delta; kernel unchanged.

**M3 — Superseded side-effect race (steering + controller).** The Gen-2 chair
maintains one PR comment via `--edit-last`; a superseded chair that posts late
clobbers the successor round's comment. Plane close cannot prevent a pod-side
`gh` call (design.md:46 boundary). Direction: round token in the chair steering
(check-before-edit), and/or the status-comment lifecycle moves to the
controller via ADR 008 `emit_status` — never into the kernel.

**M4 — Open-findings ledger (controller state, not kernel schema).** Delta
review needs per-finding open/resolved state across rounds. ADR 013's
decision/findings *counts* are already flagged as review-shaped columns in the
core schema (Unverified observations); a per-finding table would tip that scale
decisively. Direction: it is application data — it lives with the Stage 3
bundled controller, and moves out with the ADR 013 columns.

Net effect on the plan: M1 adds one mechanism item (Stage 1 — it closes a
verdict-on-stale-code correctness gap that exists today, before any
predecessor migration); M2–M4 attach to Stage 3 and name the predecessor
migration, alongside forum support, as that stage's second firing consumer.

## Staged improvement plan

Planning only. Each stage names its start trigger per design.md discipline.

**Stage 0 — paper only: declare the undeclared.** Trigger: **fired — this
review.**
- design.md:112 → "at-least-once (duplicates possible at OAB)" (A2); :113 →
  "the session always terminates", verdict-posting to the steering column (B4).
- Declare the streaming-stub invariant in the substrate section (A10); fix the
  false non-starter comments; document "skipped = dropped" (A9).
- `allow_bot_messages=true` as a checked MUST in the attach runbook + proposed
  for OAB's compliance list (A11). ADR 013: authoritative-occurrence rule (A8).
- design.md/ADR 007: PR review declared a bundled first-party controller until
  Stage 3 (B1); ADR 010 gets a `/bot-config` freeze + sunset condition (B2);
  ADR 016 gets named blockers for step 3 (D1).
- Document the supersede-vs-dedupe rule for review triggers (same-delivery
  retry dedupes; new head/command supersedes) — today's swallow-on-active
  behavior is undeclared (M1, Addendum).
- Arm ADR 001's split test on `/v1/stats`: sustained `outbox.pending > X` for
  Y min, or ttv p95 > watchdog/2 *after* Stage 1's WAL+index → revisit the
  Store swap; first false-trim or zombie-connected incident post-Stage-1 →
  revisit the membership registry split. Document the per-key ceiling in
  scale-council.md (C5).

**Stage 1 — determinism hardening, OCP-side, no wire change.** Trigger:
**fired — every item closes a confirmed defect already observed in dogfood or
measured** (TRIAGE live-hits, C2 residue, scale-knee N=6 timeout close,
incident-C7/C8-class races).
- Anchored parsing: trigger-targeted 🆗 (A1); `[[verdict:]]` final-line +
  fence-skip (A8); `[[recruit:]]` header-anchored + rate limit + parse on
  edit-finalize (A7).
- Purge closed-session outbox on both close paths (A5); delivered-marker +
  per-bot flush serialization (A2); WS ping/pong deadline (A3).
- Persist message audience, cap backfill, pin `message_processing_mode` (A4);
  monotonic done-votes, reject unresolvable targets, pin
  `remove_after_reply=false` (A6); re-deliver the trigger in-thread (A9);
  declare Relay the sole bot→bot channel or dedup fanout copies (A10).
- Boot-time `connected` reset now, column drop after (C7); `connected` re-check
  in the replace branch (C3).
- WAL + NORMAL PRAGMAs (C1); `idx_messages_session` (C2); SSE `Lagged` resync
  event (C4).
- `POST /v1/sessions` routed through the interpreter; whitelist derived from
  `coordinator::for_session` (B3).
- Membership runbook flip to the existing APIs + `DELETE /v1/bots/:id` (C6).
- Supersede-on-new-head for the review path: CAS close(`superseded`) + open
  successor, atomic under the trigger_ref index; retry-redelivery still
  dedupes (M1 — closes today's verdict-on-stale-code gap).

**Stage 2 — minimal typed upstream extension (OAB wire change).** Trigger:
**OCP-side case already made (roadmap.md:160-161); starts when OAB maintainers
accept the proposal ADR** — write and submit it now; OCP keeps text-convention
parsing as an inert fallback tier for stock pods regardless.
- One small ADR to OAB, in value order: (1) bounded per-thread dedup on
  `(channel.id, message_id)` — no schema change (A2, A5); (2) `context: bool`
  on `GatewayEvent`, serde-default false — backfill says "load, don't react"
  (A4); (3) typed `[[done]]`/`[[verdict:…]]`/`[[recruit:…]]` via the existing
  anchored directive parser, forwarded structured, stripped from body (A1, A7,
  A8); (4) optional receive-ack for events with a `request_id` —
  socket-confirmed outbox ack (A2, A3). Gate: `proto=v2` dial param.
- Independently defensible: gateway-path bot-turn cap (A10). Defer
  handshake/capabilities with a named trigger: the second platform-conditional
  hack after `EDIT_RESPONSE_PLATFORMS`, or OAB starting protocol-spec v2.

**Stage 3 — app extraction, in-process.** Trigger: **ADR 007's own trigger
("another real plugin") — arguably fired by the ADR 014 triage panel; fires
definitively when the forum north client moves from plan to build**
(docs/forum-north-client-plan.md), **and again when the Gen-1 Discord council
(nuphos) migrates onto OCP** (Addendum).
- `plugins/pr_review` bundled controller: prompts, presets/angles, webhook
  parsing, `/v1/review` exit the kernel; only ingress is `controller.rs`
  actions; done-policy moves onto the `Coordinator` trait (B1). Forum support
  routed the same way from day one — never a second grandfathered exception.
- CodeRabbit-parity conversational loop, controller-owned: mention → panel
  re-review with open-findings delta context (M2); per-finding open/resolved
  ledger as controller state, moving out with the ADR 013 columns (M4);
  round-token check-before-edit in chair steering and/or status-comment
  lifecycle via ADR 008 `emit_status` to close the superseded `--edit-last`
  race (M3).
- `/bot-config` frozen + demoted per ADR 010: profiles and pre_boot move to
  templates/`configUrl` artifacts (B2).
- ADR 016 finish: `issue()` honors the flag, `POST /v1/bots` externalized
  story, default flips on for new deployments, then `token_plain` drop (D1).
  Optional: ADR 012 webhook hardened to ADR 008's posture if verdict delivery
  is to become plane-held (B4).

**Stage 4 — scale, trigger-gated.** Trigger: **only when Stage 0's arm
conditions trip** — sustained outbox.pending / ttv-p95 thresholds after
WAL+index (store work); first post-Stage-1 membership incident (registry
split); measured 429 ceiling (quorum policy).
- Batch enqueue per fanout, settled cursors, then the networked `Store` behind
  the existing trait (C1); messages retention (C2); per-session SSE channels
  (C4); membership registry split per ADR 001 — prerequisite met by C7's
  presence cleanup, not a deployment boundary (C6, C7); K-of-N quorum presets
  as app-layer policy (C5).

### Finding → stage map

| Finding | 0 | 1 | 2 | 3 | 4 |
|---------|---|---|---|---|---|
| A1 done-signal aliasing | | anchored 🆗 | typed `[[done]]` | | |
| A2 delivery gap | doc downgrade | flush serial., delivered-marker | dedup, recv-ack | | |
| A3 silent death | | WS ping | recv-ack | | |
| A4 active backfill | | audience, cap, pin mode | `context` flag | | |
| A5 closed-session replay | | purge on close | dedup backstop | | |
| A6 erasable votes | | monotonic votes, pin flag | | | |
| A7 recruit injection | | anchor + rate limit | typed directive | | |
| A8 verdict trailer | ADR 013 rule | final-line anchor | typed directive | | |
| A9 mention-gate drop | fix comments, doc semantics | in-thread re-delivery | | | |
| A10 streaming stub | declare invariant | explicit multiplicity | turn cap | | |
| A11 trust overlap | MUST-flag contract | | handshake (deferred) | | |
| B1 app in kernel | honest-docs amendment | | | extraction | |
| B2 /bot-config | freeze + sunset | | | demotion | |
| B3 /v1/sessions bypass | | route via interpreter | | | |
| B4 prompt-enforced | reword :113 | | | ADR 012 hardening (opt) | |
| C1 store hot path | arm metrics | WAL PRAGMAs | | | batch, Store swap |
| C2 messages index | | index | | | retention |
| C3 sweep race | | connected re-check | | | |
| C4 SSE lag | | Lagged resync | | | per-session channels |
| C5 stragglers | document ceiling | | | | K-of-N presets |
| C6 membership ops | | runbook + DELETE route | | | registry split |
| C7 presence zombies | | boot reset, column drop | | | registry split |
| D1 ADR 016 default | named blockers | | | flip + column drop | |
| M1 supersede semantics | doc the rule | atomic supersede | | | |
| M2 mention re-review | | | | controller intent + delta | |
| M3 --edit-last race | | | | round token / emit_status | |
| M4 findings ledger | | | | controller state | |

## Unverified observations

Overflow from the critic pass — **not adversarially verified**; treat as leads.

- **Message broker as data plane: reject** (assessed above). Re-open trigger:
  horizontal OCP replicas/HA, and even then the networked Store first.
- **Desired-state reconciler for membership: defer** (assessed above) until a
  provisioner exists — no actuator today, and a naive reconciler retrying
  convergence risks the once-only CAS semantics.
- **Global `github_mint_lock`** serializes all installation-token mints
  (~200-500ms round-trip each); a 30-session convene burst queues ~18s of chair
  bootstraps. The comment names the fix (key by session/role); defer until
  convene concurrency rises; track mint wait in `/v1/stats`.
- **Review-shaped columns in the core schema** (`quorum_n`, ADR 013
  decision/findings): honestly-labeled defer-extract, currently unowned — bind
  extraction to named events (verdict columns move with the Stage 3 plugin;
  `quorum_n` when a second coordinator config lands).
  *Amended at Stage 3 S1 ([ADR 018](adr/018-stage3-extraction.md) ruling 4):
  Stage 3 moves the verdict columns' interpretation (`[[verdict:]]` parsing
  behind the `structured_verdict` hook); their storage moves at M4 into a
  plugin-owned findings table — route.md ordering wins over the "move with
  the Stage 3 plugin" phrasing above.*
- **Forum "chat mode"**: a long-lived completion condition is legitimate
  `Coordinator` policy (one `for_session` arm, watchdog unchanged), not new
  mechanism; adopt when forum Phase 1 dogfood shows session-per-turn churn
  cost.

## Method note

This review ran 2026-07-07 as a multi-agent workflow over both repos (OCP and
the OpenAB source it rides): four independent mappers (code, docs, git history,
incident record) fanned into four critics (conformance, protocol, scale,
alternatives); findings were deduped, then individually re-verified against
source by adversarial verifiers instructed to refute them — 15 confirmed, 9
confirmed with corrections (carried here in corrected form), 0 refuted. Related
docs: [design.md](design.md), [roadmap.md](roadmap.md),
[coordinators.md](coordinators.md), [ADR 001](adr/001-three-planes.md),
[ADR 007](adr/007-control-plugins-and-oab-father.md),
[ADR 008](adr/008-external-controller-protocol.md),
[ADR 010](adr/010-openab-configurl-boundary.md),
[ADR 013](adr/013-decision-review-state.md),
[ADR 014](adr/014-triage-panel.md),
[ADR 016](adr/016-gateway-token-externalization.md).
