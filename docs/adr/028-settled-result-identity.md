# ADR 028 — Settled result identity: the kernel records which messages ARE a session's result

Status: proposed · 2026-07-15

## Context

When a session closes, "what did the bot actually produce" has no durable
answer. Messages are stored as ordered rows; a bot's long output arrives as
~4 KB chunks (one row each, split by the OAB gateway); nothing marks which
rows constitute the settled result. Every consumer re-derives it by guessing.

Two independent consumers hit this, from opposite directions:

1. **The external workflow controller (SDD, finding F4).** A produce
   session's ~30 KB artifact arrived as EIGHT messages. `settled = last
   message` graded a 1.6 KB tail fragment: three attempts scored 110/170/125
   while the fix loop mechanically worked — on garbage. The controller now
   joins all of the author's messages client-side; every future consumer
   would have to rediscover the same workaround. (Evidence:
   sdd-controller `docs/w1-findings.md` §F4; that repo's `settled_text` is
   the client-side join this ADR obsoletes.)

2. **The kernel itself.** `OrchCtx::latest_settled` (`src/orchestrator.rs`)
   picks the *last* non-empty, non-stub message by the author. The close
   path uses it as the verdict text for the north `verdict` SSE event and
   the close webhook — so for any chair final larger than one chunk,
   downstream consumers receive **only the last chunk**. The `[[verdict:]]`
   trailer survives (it sits on the last line), which is why review mode
   works; the synthesis *text* is silently truncated. The kernel computes
   the answer at close and then throws it away: nothing on disk says which
   message rows were the result.

ADR 013 persists the structured verdict (`decision`, R/Y/G counts). ADR 020
persists per-finding rows. Neither identifies the result *messages* — the
ledger's own spec wants `source_message_id` references, which presume
exactly the identity this ADR creates.

## Decision

**At the close CAS, the kernel durably records the settled result span:
`result_author_id` + `result_message_ids` (ordered JSON array), computed by
one shared rule, and exposes it on the read surface. No bot cooperation, no
wire change.**

1. **One span rule, one implementation.** The settled result span of an
   author = walk back from that author's last *settled* message — settled by
   the SAME predicate `latest_settled` uses (non-empty, not the `…`
   streaming placeholder; ASCII `...` counts as content), so the recorded
   result can never disagree with the verdict about which message settled —
   and collect contiguous settled messages by the same author. `system`
   rows are transparent (a coordination prompt between chunks must not
   truncate an artifact). A **client row breaks the run**: a client message
   starts a new turn, so a reopened session's later close records only the
   later answer, never both. A message from a *different bot* author also
   breaks it. Ordered oldest→newest. A run of one message (the common
   review case) degrades to today's `latest_settled` behavior. The rule
   lives next to `latest_settled`; the close path is its only writer and
   the read path only ever reads what was recorded — no second guess
   anywhere.

2. **Atomic durable capture at close.** The normal close is ONE store
   transaction (a single guarded UPDATE with the `advance_state` CAS
   guard): state→closed + `decision`/findings columns (ADR 013) +
   `result_author_id` (the author whose settlement closed the session —
   chair for council, the solo bot for solo, last stage bot for pipeline) +
   `result_message_ids`. The close arm computes the verdict and the span
   *before* the transaction; either everything lands or nothing does — no
   window where a session is visible as closed without its result, and no
   torn close if the process dies mid-close. **Failure semantics:** if the
   combined transaction fails, the session simply stays open and the
   watchdog timeout path remains the guaranteed-termination backstop; a
   result-write failure can delay a close to the timeout path, but a
   normal close can never produce closed-without-result. Two additive
   `sessions` columns via the guarded `migrate()` path. This is
   deliberately *not* a plugin table (ADR 018 ruling 4 reserves those for
   review semantics): result identity is mode-agnostic session lifecycle
   state, exactly like `closed_at`. Legacy closed sessions keep NULL — the
   read surface says "unknown", never re-guesses history.

   Two lifecycle corollaries keep the recorded identity truthful:
   - **Reopen clears it.** A client follow-up that reopens a terminal
     session (the ADR 011 solo pattern) NULLs both result columns in the
     same guarded UPDATE that flips the state — a stale span never
     survives as the reopened session's result; the next close records its
     own turn's span (and a timeout close records nothing).
   - **Closed transcripts are frozen.** `edit_message`/`delete_message`
     on a closed or aborted session are rejected (no-op + warn, like
     unknown commands), so the recorded span's *text* is immutable. This
     does not break streaming bots: a legit stub fill-in edit lands before
     close — that edit itself carries the done-signal that closes.

3. **Read surface (ADR 020 shape: ids attached, text on demand).**
   - `GET /v1/sessions/:id` gains an additive `result` key: the identity
     object `{ "author_id", "message_ids" }`, `null` until
     closed-with-result or for legacy rows.
   - `GET /v1/sessions/:id/result` returns one envelope with one
     discriminator: `{ "result": { "author_id", "message_ids", "text" } }`
     when present — `text` is the server-side join (chunks concatenated in
     order, `\n`-separated) — and `{ "result": null }` when absent. This
     is the route that retires every client's re-implemented join.
   - Both surfaces share the one parse of `result_message_ids`; corrupt or
     non-array JSON (impossible via the write path) is treated as absent
     on both, with a warn — the two routes never disagree about whether a
     result exists.
   - The verdict SSE event and close webhook keep their frozen v1 shape
     (ADR 018 ruling 6); they gain nothing. Consumers who need full text
     now have a durable route instead of a bigger transient event.

4. **The frozen bot wire is untouched.** The plane already knows the
   answer; asking bots to flag results would put a frozen-contract change
   (`tests/gateway_contract.rs`) on the critical path for zero information
   gain. If a future bot wants to *override* the span explicitly, that is
   a separate additive proposal.

### Non-goals

- No content dedup/normalization: the join is bytes-in-order plus `\n`
  between rows; noise filtering (tool-status lines etc.) is consumer
  vocabulary, not kernel (kept in the SDD profile where it lives today).
- No retro-backfill of legacy closed sessions.
- No change to `[[verdict:]]` / findings-block parsing (they already work
  on their own carriers).

## Consequences

- The SDD controller (and any future workflow consumer) reads
  `/v1/sessions/:id/result` instead of carrying a join heuristic — F4's
  class of "graded a fragment" bugs becomes structurally impossible for
  consumers that use the route.
- The kernel's own truncated-verdict-text hazard is now documented and
  measurable (the span is on disk; the SSE keeps last-chunk behavior until
  a consumer actually needs more — evolution deferred, ADR 018 M4).
- Two new nullable columns on `sessions`; write cost stays one UPDATE at
  close (the same guarded UPDATE that closes). Interleaved multi-bot
  chatter can still truncate a span at a genuinely intervening foreign
  message — accepted ceiling, documented in the span rule; the fix
  (bot-declared spans) is the separate proposal in Decision 4. Likewise a
  bot that interleaves its answer chunks *around* a client message keeps
  only the post-client run — accepted: a client row is a turn boundary by
  definition.
- Second-consumer proof: a `tests/second_consumer.rs` case exercises a
  chunked solo session end-to-end (N chunks → close → `result` ids + joined
  text) using only generic vocabulary — the W5 exit criterion.

## References

- sdd-controller `docs/w1-findings.md` F4 (chunked artifact graded as
  fragment), F10 (noise stripping = consumer concern)
- ADR 013 (decision columns at close), ADR 018 (rulings 4/5/6: plugin
  tables, guarded migrate, frozen v1 wire), ADR 020 (findings ledger;
  `source_message_id` presumes message identity)
- `src/orchestrator.rs` `latest_settled` / close `run_actions` arm;
  `src/store.rs` `messages` ordering + `migrate()`
