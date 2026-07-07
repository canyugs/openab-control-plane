# ADR 012 — Session close webhook callback

Status: **accepted** · 2026-07-02

## Context

When a council review finishes, the only notification is the chair's PR comment —
which triggers a GitHub notification if the user watches the repo or is assigned.
There is no active push to external channels (Slack, Discord, LINE, custom
dashboards). Teams that want instant feedback on review completion have no hook
to listen to.

OCP already emits north SSE events (`emit_north("verdict", …)` and
`emit_north("state", …, "closed")`) on both close paths:

- **Normal close** — `Action::Close` in `orchestrator.rs:842`, driven by the
  coordinator when chair signals done.
- **Timeout close** — `force_close_timeout` in `orchestrator.rs:272`, driven by
  the watchdog when a session exceeds `OABCP_SESSION_TIMEOUT_SECS`.

These events are consumed only by the SSE `/v1/sessions/:id/stream` endpoint.
There is no outbound HTTP callback.

## Decision

1. **One env var, one URL.** `OABCP_SESSION_CLOSE_WEBHOOK` configures an
   optional outbound webhook URL. Unset = no callback (zero-cost default).
   Stored on `AppState` at startup.

2. **Fire-and-forget POST on close.** Both close paths (`Action::Close` and
   `force_close_timeout`) spawn a `tokio::spawn`'d async POST after the session
   transitions to closed. The request is a single JSON body:

   ```json
   {
     "event": "session.closed",
     "session_id": "ses_…",
     "trigger_ref": "github:pr/owner/repo#123",
     "mode": "review_council",
     "verdict": "LGTM ✅ — …",
     "reason": "normal" | "timeout" | "superseded",
     "roster": ["chair", "rev1", "rev2"],
     "ts": 1782967850544
   }
   ```

   `Content-Type: application/json`. No HMAC signature (item 5 below).

3. **No retry queue.** The POST is best-effort: log a warning on failure, do not
   retry. A notification webhook is advisory — a missed callback is not data loss
   (the verdict is already on the PR and in the store). Adding a retry queue
   would require durable state, backoff, and dead-letter handling for a feature
   whose failure mode is "you don't get a Slack ping." YAGNI.

4. **One shared helper.** A single function
   `fn fire_close_webhook(state: &Arc<AppState>, session_id: &str, verdict: &str, reason: &str)`
   called from both close paths. It reads session metadata from the store,
   builds the payload, and spawns the POST. The helper is a no-op when
   `state.close_webhook_url` is `None`.

   Amendment 2026-07 (M1): `"superseded"` is emitted by the P1 supersede path.
   Until Stage 3 adds a durable close reason, the declared crash window is: a
   crash after the supersede transaction commits but before side effects fire
   loses the webhook event. It is lost, not delayed; there is no pre-P3 re-drive.

5. **No HMAC signature in v1.** The webhook receiver should validate the source
   by network policy (internal URL, VPN, firewall) rather than a shared secret.
   If a secret is needed later, add `OABCP_SESSION_CLOSE_WEBHOOK_SECRET` and
   sign with HMAC-SHA256 in the same `x-hub-signature-256` style as the GitHub
   webhook — one line of code on the existing `hmac_sha256` helper.

## Consequences

- **New:** `close_webhook_url: Option<String>` on `AppState`, one async helper,
  two call sites (normal + timeout close), env var documentation.
- **No new files.** The helper lives in `orchestrator.rs` next to the close
  logic it serves.
- **No new dependencies.** `reqwest` is already in `Cargo.toml` with `json` +
  `rustls-tls`.
- **Latency:** the webhook POST is spawned, not awaited — session close is not
  blocked by the receiver's response time.
- **Failure mode:** a broken webhook URL logs a warning per close. No retry, no
  backlog, no circuit breaker. The session closes regardless.

## Deferred

- **HMAC signature** (item 5) — add when the webhook crosses a trust boundary.
- **Per-session webhook URL** (passed in the `POST /v1/sessions` body) — useful
  for multi-tenant routing but not needed while there is one deployment.
- **Multiple webhook URLs / fan-out** — use a single URL pointing at a relay
  (e.g. Zapier, n8n) rather than building fan-out into OCP.
- **Retry with backoff** — add only if missed notifications become a real
  operational problem, not a theoretical one.

## Effort

Small — ~2 hours. One field on `AppState`, one helper function, two call sites,
env var in `dev-deploy-k8s.sh`, one test.
