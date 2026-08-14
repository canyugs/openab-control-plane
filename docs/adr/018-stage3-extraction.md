# ADR 018 — Stage 3 extraction rulings

Status: accepted · 2026-07-09 · S1 of
[stage3-extraction-plan.md](../stage3-extraction-plan.md); ratified by council
review of the S1 PR. Amends [ADR 007](007-control-plugins-and-oab-father.md),
[ADR 008](008-external-controller-protocol.md),
[ADR 016](016-gateway-token-externalization.md) (each flipped
proposed → accepted-as-amended in the same PR) and the boundary review's
verdict-column phrasing (boundary-review-2026-07.md, "verdict columns move
with the Stage 3 plugin").
Amended by [ADR 031](031-provider-neutral-kernel.md) (ratified 2026-08-14):
the same-crate compatibility implementation these rulings govern has since
been removed — see Amendment below.

> **Amendment (2026-08-14, ADR 031 ratification) — the Stage 3 compatibility
> implementation is gone; the GitHub-residue exit triggers fired.** This ADR
> remains the historical Stage 3 ruling (ADR 031 keeps it as the migration
> starting point), but the shipped kernel has moved past several of its
> surfaces:
>
> - **Ruling 2's `review_council` arm went with the embedded plugin.** The
>   external `github-pr-controller` took GitHub ingress and writes at the
>   2026-07-31 prod cutover; the embedded ingress was deleted in v0.1.67 after
>   zero-use evidence on both lanes (ADR 031 invariant #9, #361); the v0.1.70
>   kernel slim (#366) renamed `plugins/pr_review` to the generic
>   `plugins/council`. `coordinator::lookup("review_council")` now returns
>   `None`, guarded by the S9 fail-loud unknown-mode dispatch. The S12 grep
>   gate outlived the plugin as the permanent kernel-purity gate in CI.
> - **Ruling 4's "M4 plugin-owned findings table" was overtaken by ADR 031
>   §6:** findings are controller product state, not plane state. The findings
>   ledger moved to `github-pr-controller` (SEI-895, #364) and
>   `pr_review_findings` is dropped from the plane's schemas.
> - **The residue ledger's "ADR 008 transport era, if it fires" trigger
>   fired.** The review lookup arm is deleted; the generic `triage_council`
>   arm remains by design (dispatch safety).
>
> Ruling 6's frozen v1 wire contracts and the ADR 013 verdict columns
> (Rulings 3/4 storage, `set_session_verdict`) remain live kernel surfaces:
> ADR 031 §8 deprecates review-shaped compatibility contracts with live
> consumers; it does not delete them.

## Context

Stage 3 extracts the PR-review application from the kernel into
`src/plugins/pr_review` behind `controller.rs` (boundary finding B1), freezes
and demotes `/bot-config` (B2), and finishes ADR 016 (D1). The execution
sequence (S1–S17), acceptance tests, and deployment cadence live in
[stage3-extraction-plan.md](../stage3-extraction-plan.md); this ADR records
the **decisions** that plan rests on, so a mid-stage amendment is a visible
ADR change rather than silent drift. Ratification is deliberately up front —
landed PRs must not be invalidated by re-litigating the governing ADRs.

## Rulings

1. **Module layout: single crate, `src/plugins/{pr_review,triage}` modules.**
   No cargo workspace and no crate split this stage. Named precondition for
   any future split: plugin config must first move off process-global env
   reads (`OABCP_ALLOWED_REPOS`, `OABCP_BOT_HANDLE`, review caps) to injected
   config — test correctness currently rests on same-crate env locks.

2. **Kernel→plugins dependency direction is by design.**
   `coordinator::lookup`'s review/triage arms reference `crate::plugins::*`
   structs directly: a compile-time static match, chosen over a dynamic
   registry because persisted-mode dispatch across restarts must never depend
   on registration order (a missing arm silently downgrades a persisted
   `review_council` row to `QuorumCouncil` and hangs a 1-bot roster — the C4
   bug class). The enforced boundary is "no review vocabulary in kernel
   *logic*", proven by a permanent CI grep gate (S12: `PR Review`, `gh pr`,
   `[[verdict`, `TRIAGE` may not appear in api.rs / orchestrator.rs /
   coordinator.rs / controller.rs), plus fail-loud dispatch for unknown
   persisted modes (S9: error log + north `dispatch_error` + force-close
   `reason:"unknown_mode"`, never silent quorum adoption).

3. **Coordinator hook surface (S6–S8, S13).** Four defaulted policy methods
   whose defaults encode the native OAB contract, so generic coordinators —
   and forum, the consumer that carries no code — override nothing:
   `reaction_counts_as_done`, `accepts_text_done`, `recipient_trigger_text`
   (pure; applied by mechanism at ALL trigger-delivery sites: fanout,
   backfill, chair redelivery), `structured_verdict` (returns
   `Option<StructuredVerdict>`; called in the Close arm before the close
   webhook reads the row). Plus `reopen_on_client_message` (default false,
   Solo true) as a separate follow-up PR (S13), never bundled mid-extraction.
   `StructuredVerdict{decision, red, yellow, green}` keeps review-flavored
   vocabulary in a kernel trait signature: accepted residue, moves with the
   columns at M4.

4. **Verdict-column ruling (amends boundary-review :691).** Stage 3 moves the
   ADR 013 verdict columns' **interpretation** (the `[[verdict:]]` grammar
   becomes plugin code behind `structured_verdict`); their **storage** moves
   at M4 into a plugin-owned findings table — route.md ordering wins over the
   boundary review's "move with the Stage 3 plugin" phrasing. Until M4 the
   columns and `set_session_verdict` are deprecated-in-place with doc
   comments. Binding rule for M4: new findings state goes to the plugin
   table, **never** new `sessions` columns; the plugin table is created via a
   guarded `Store::run_plugin_migration` hook that runs after kernel
   `migrate()` — that seam is designed **here**, the migration itself lands
   at M4, zero code this stage.

5. **Zero SQLite schema changes all stage.** Every Stage 3 PR deploys onto
   the lanes' legacy DBs and rolls back with a plain image revert. This
   structurally excludes the PR #145 hazard class (schema/index-ordering
   breakage on upgrade).

6. **Frozen v1 kernel wire contracts.** The `/v1/sessions` JSON, the north
   `verdict` SSE event, the ADR 012 close-webhook payload, and the `/v1/stats`
   review aggregates keep their ADR 013 vocabulary: they are external
   contracts with live consumers. Golden snapshots pin all four (S3); their
   evolution belongs to the M4 ADR. Internal-signature purity is not bought
   by breaking external receivers.

7. **Controller action vocabulary and serialized shapes (ADR 008 v1).** The
   in-process `ControllerAction` structs are the de facto ADR 008 v1 schema.
   Implemented this stage: `OpenSession` (gains a genuinely-fed `prompt`,
   S5), `PostMessage` (S5). Reserved, not implemented: `AddRoster`,
   `CloseSession`, `EmitStatus` — roster routes keep calling the orchestrator
   directly until M3/M4 need them. Serialized JSON shapes, recorded so
   externalization is checkably a transport change (unversioned; frozen when
   a transport first carries them):

   ```json
   {"action":"open_session","title":"…","trigger_ref":null,
    "trigger_fingerprint":null,"roster":["…"],"quorum_n":2,
    "chair_bot":null,"mode":"council","prompt":"…"}

   {"action":"post_message","session_id":"ses_…","content":"…"}

   {"action":"add_roster","session_id":"ses_…","bots":["…"]}          // reserved
   {"action":"close_session","session_id":"ses_…","reason":"…"}       // reserved
   {"action":"emit_status","session_id":"ses_…","target":"…","body":"…"} // reserved
   ```

   Results: `{"result":"session_opened","session_id":"…","deduped":false}` ·
   `{"result":"superseded","session_id":"…","old_id":"…"}` ·
   `{"result":"message_posted","session_id":"…","message_id":"…"}`.
   Field names track the `ControllerActionResult` structs verbatim (`old_id`,
   not the north HTTP surface's flattened `old_session_id` — that flattening
   is an api.rs concern, frozen separately by the S3 wire snapshots).
   Interpreter semantics: the Superseded arm performs supersede cleanup
   itself (S4) — side effects belong to the interpreter, never to caller
   memory (ADR 008).

8. **Mechanical-move rule.** The move PRs (S10–S12) allow **no logic edits**;
   review enforces `git mv` + path/import adjustments only. The pinned
   integration tests (chair-auto-🆗/TRIAGE gate, trigger round-trip,
   forum-shaped reopen) stay byte-unmodified through S4–S9 and move only
   with their subjects.

9. **`/bot-config` (B2): freeze first, demote last, never extract.** Golden
   snapshots (S2) land before any extraction PR; the demotion runbook (S17)
   is gated on S15 plus template-repo `configUrl` work; removal only on
   evidence (per-serve deprecation warn log silent for a full release). The
   endpoint's review-flavored content is dead code slated for deletion —
   moving it into the plugin would be churn.

10. **ADR 016 finish (D1).** `issue()` honors `externalize_tokens()` and the
    plaintext token is returned exactly once in the `POST /v1/bots` response
    (S14); registration accepts an operator-supplied token so the plane can
    learn only the hash (S14); the default flips to externalized **with a
    boot-time legacy-DB guard** — env unset + non-empty `token_plain` rows →
    legacy mode with a loud deprecation warning naming the exact
    `OABCP_BOT_TOKEN_<NAME>` vars, never a boot failure (S15). **Blocker 4
    (`token_plain` drop) stays deferred**: the column serves legacy plaintext
    mode; trigger = removal of that mode itself, one release after the
    default-on warning ships; drop pattern = the `connected`-column soft-drop
    precedent (out of fresh SCHEMA, stop reading/writing, tolerate in legacy
    DBs, never `ALTER DROP`).

## Residue ledger (named, located, exit-triggered)

| Residue | Location | Exit trigger |
|---|---|---|
| Kernel→plugins lookup arms | coordinator.rs | ADR 008 transport era, if it fires |
| ADR 013 columns + `set_session_verdict` + fused `/v1/stats` aggregates | store.rs / api.rs | M4 plugin-owned findings table |
| ADR 013 vocabulary on wire surfaces | sessions JSON, verdict SSE, close webhook, /v1/stats | M4 ADR owns evolution (snapshot-pinned meanwhile) |
| `StructuredVerdict` fields in a kernel trait | coordinator.rs | moves with the columns at M4 |
| Role→write/read scope map | github_app.rs | second GitHub-writing plugin → explicit requested-scope input on the token route, validated against coordinator policy |
| ~~Watchdog `created_at`-anchored reopen trap~~ | orchestrator watchdog | **resolved** — forum prod dogfood fired the trigger; the deadline now anchors on the session's last message (`active_sessions_before`), so silence rather than age marks a session stuck. No coordinator arm was needed; S13's regression test now pins the fixed behaviour |
| Plugin config via process-global env | plugins/* | precondition for any crate split: config injection first |
| Single shared `OABCP_API_KEY` north auth (forum proxy can touch every session) | api.rs | first non-first-party north client, or forum leaving the owned lanes → scoped second bearer |
| B2 demotion execution (template config) | Zeabur templates | **executed 2026-07-09** — pod-owned config.toml mounts (ADR 010 amendment; templates externalized since #80 satisfied the D1 coupling on this path). Endpoint removal stays S17-evidence-gated |
| `quorum_n` in core schema | store.rs | second coordinator config |
| Durable `close_reason` column | (none yet) | decided in the M3/M4 ADR alongside the `CloseSession` verb |

## Consequences

- Forum support routes through `controller::execute` from day one — never a
  second grandfathered exception (B3). Its v1 contract: open(solo, prompt) in
  one call, follow-ups only into a non-terminal session, **fresh session
  after close**, no reliance on reopen.
- Phase-1 behavior is preserved through S4–S9 by the unmodified pinned tests;
  the stage's only intentional behavior changes are S8 (non-review modes stop
  attempting trailer parsing), S13 (non-solo reopen → 410), and the S15
  default flip (a verified no-op on the owned lanes, legacy-guarded
  elsewhere), each release-noted.
- Exit criteria and the four publish points are as specified in
  [stage3-extraction-plan.md §9/§3](../stage3-extraction-plan.md); the S16
  second-consumer proof test must pass using only trait defaults and zero
  `plugins::` imports.
