# ADR 023 — Bot health: agent-level liveness (connected ≠ healthy)

Status: proposed · 2026-07-13

## Context

A shared `KIRO_API_KEY` hit its usage quota. Both lanes' kiro chair started
returning JSON-RPC `-32603 Internal Error` on every request — while their
websockets stayed **connected**. Separately, two codex reviewers' device-login
tokens were revoked, same symptom. Prod's official council was silently unable
to produce a verdict for ~a day; nothing surfaced it. It was found only because
an operator manually opened throwaway free-text "reply PONG" sessions against
each bot and read the logs.

The gap is not missing data — it is that the plane never looks. What the plane
already has (verified against current code):

- **`bots.health TEXT NOT NULL DEFAULT 'ok'`** — a health column exists in the
  schema (`src/store.rs`) and is currently never driven by agent behavior.
- **Transport liveness** — `bots.connected` (reset to 0 on boot) + `last_seen`
  track the websocket, extended by the C7 reconnect-zombie work
  ([[dogfood-reconnect-zombie]]). This answers "is the socket up," NOT "can the
  agent behind it produce output."
- **The failure signal already flows through the plane.** The `-32603` text is
  NOT generated in the plane's Rust — the agent (kiro-cli/codex ACP) raises the
  JSON-RPC error and the bot-side gateway wraps it into a message frame. The
  plane stores that frame verbatim as opaque content, unaware it is an error.

So `connected == true` is treated as healthy while the agent is dead. That is
the whole bug.

## Decision

1. **Introduce agent-level health, distinct from transport `connected`, and
   drive the existing `bots.health` column with it.** A bot is `connected`
   (socket up) yet `health != ok` (agent failing). The two are orthogonal and
   both matter; do not collapse them.

2. **Passive detection is the MVP — it is free, and the signal is already in
   hand.** When a bot's turn in a session yields an error frame instead of
   content, increment a per-bot consecutive-error count; a successful content
   frame resets it to 0. Cross a threshold (default 3 consecutive) → set
   `bots.health = 'degraded'`, stamp `last_error_at`, and emit one structured
   `WARN` log (existing log-based alerting is the delivery path — no new
   notification system). This fires the instant a real session touches a broken
   bot.
   - Error-frame recognition: match the known error signature in frame content
     (`code: -32603` / "Internal Error" with no model output). `ponytail:`
     brittle-but-zero-protocol-change; upgrade to a frame-level `is_error` flag
     set by the bot gateway if the wrapper text drifts.

3. **A sparse active probe closes the idle-arming gap — phase 2, not the MVP.**
   Passive detection cannot see a bot that broke while no PRs are flowing (the
   exact prod case: armed-and-silent for a day). A plane background task opens
   an internal solo "reply PONG" session per connected bot on a long interval
   and feeds the result into the same health counter. **Cadence is bounded by
   quota, not latency** — the thing that broke was a usage limit, so probe
   sparsely (default every 30–60 min, 1-token prompt); an over-eager probe
   would burn the very budget it guards. `ponytail:` fixed interval first;
   only probe-after-N-idle-minutes if the flat cost proves to matter.

4. **Detect and surface; never auto-remediate.** The plane does not restart
   pods, rotate keys, or re-auth agents from a health signal. Health degrades →
   `/v1/bots` shows it + a WARN fires → a human acts (raise quota, re-login),
   dev-first per the deploy gate. This matches the human-gated stance of
   [[review-effectiveness-feedback-loop]] (ADR 021): OCP reports, humans decide.

## Non-goals

- **No external polling script as the system of record.** The manual
  `bot-health.py` smoke used to find this incident is a throwaway operator tool;
  detection belongs in the plane, not a bolted-on cron.
- **No per-request health SLA / no dashboard / no metrics store.** A `health`
  field on `/v1/bots` + a WARN log is the whole surface. A rollup can come later
  if asked, same as ADR 021's read-only-endpoint deferral.
- **No auto-remediation** (restart, key rotation, re-auth) — see Decision 4.
- **Health does not gate convening (yet).** This ADR makes breakage *visible*.
  Whether a `degraded` bot is trimmed from a convene (like an idle reviewer) or
  still counted is a follow-up — today an error frame already counts toward
  `reviewers_done`, so a broken reviewer degrades a review to noise rather than
  hanging it. Changing that is a separate decision.

## Migration / build order

1. **Phase 1 (passive, MVP):** recognize error frames in the session-ingest
   path → drive `bots.health` + `consecutive_errors`/`last_error_at`; expose
   `health` on `GET /v1/bots`; WARN on threshold crossing. No protocol change.
2. **Phase 2 (active probe):** plane background task, sparse internal PONG per
   connected bot → same health counter. Closes the idle gap.
3. Retire the manual `bot-health.py` smoke once Phase 1 lands (keep only as an
   ad-hoc debugging aid).
4. If wrapper-text matching proves fragile, add a frame-level `is_error` flag at
   the bot gateway and switch Phase 1 onto it.

## References

- [[dogfood-reconnect-zombie]] — the C7 transport-liveness work this extends
  from socket to agent.
- ADR 021 (review-effectiveness-feedback-loop) — the report-not-remediate,
  human-gated stance this inherits.
- `openab-control-plane-ops/docs/deploy-gate.md` — dev-before-prod, the path a
  human takes once a bot shows `degraded`.
