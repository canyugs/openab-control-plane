# PR Review Council — end-to-end flow

What happens when the council reviews a PR, on the controller path that has
run production since the 2026-07-31 cutover (ADR 031). Install commands live
in `controller-action-api.md` (controller) and `install-github-app.md` (App);
the embedded quickstart profile that predates the cutover is described at the
end. This file describes the runtime flow.

## Topology

```text
                     GitHub
      pull_request / issue_comment webhooks
                       |
                       v
            github-pr-controller
      webhook auth · delivery dedupe · repo/author
      admission · trigger parsing · SessionPlan
      durable write outbox (ALL GitHub writes)
                       |
         action API ->   <- signed runtime events
                       v
North API / SSE <-> control-plane <-> SQLite or Postgres (ADR 033)
(CLI, operators)    sessions · roster · fanout       bots, sessions, messages
                    coordinator policy · quorum      findings ledger, waivers
                    durable delivery · watchdog      outbox, scoped tokens
                       |
          gateway /ws (per-bot OCP tokens)
                       v
     stock OpenAB pods (config via configUrl or mount, steering via pre_seed)
       chair, rev1, rev2 — read-only PR context, no GitHub write credentials
```

- Pods are stock OpenAB pods. They dial out to the plane over `/ws`.
- The plane seeds the roster at boot from `OABCP_BOTS`, usually
  `chair:chair,rev1:reviewer,rev2:reviewer`.
- **No bot writes to GitHub.** The controller owns every write: round
  comments, the verdict comment, the `openab/council` commit status, and the
  formal PR review. Bots read PR context through whatever read-only tools
  their pods carry (e.g. a broker MCP or `gh` with a read token).

## Full Review

1. **Trigger** — a PR `opened` / `reopened` / `ready_for_review` webhook or a
   write-ish commenter's `/review` reaches the controller, which
   authenticates the delivery, dedupes it, and checks repository + author
   admission. Push-triggered rounds are rate-limited per PR; `/review`
   bypasses the hourly cap but not the per-PR round budget.
2. **Open** — the controller builds a `SessionPlan` and calls the plane's
   action API: `open_session` with mode `council`, `chair_bot`, the reviewer
   roster, and `trigger_ref="github:pr/owner/repo#N"` (the plane rewrites it
   to a hashed controller-scoped ref; re-delivery dedupes on it, a new head
   supersedes the old round). Active operator waivers for the repo (ADR 035)
   are injected into the CHAIR's opening input at this boundary — reviewers
   never see them.
3. **Started comment** — from the action result the controller posts the
   round comment: "Review Council started (round N)" plus a baseline (files
   changed, CI state). Since 2026-08-02 this post **stays in the thread**;
   the verdict arrives later as its own comment. `/ask` sessions get no round
   comment.
4. **Fanout / review** — every roster member receives the PR-pointer trigger
   (never an inlined diff); reviewers self-fetch context, post findings, and
   signal done with `[done]` or the done reaction (`🆗`). The plane relays
   each reviewer's settled final message to the chair.
5. **Quorum → synthesis** — at reviewer quorum the plane prompts the chair.
   The chair's final message carries the human report, the machine findings
   block (ADR 020, incl. `"status":"waived"` rows matched to waiver ids), and
   the `[[verdict:approve|request_changes r= y= g=]]` trailer.
6. **Verdict required** — a synthesis turn without a parseable trailer
   re-queues (bounded) with chair-pool rotation instead of closing
   verdict-less (v0.1.59); the liveness watchdog stays the hard backstop for
   stalls.
7. **Close** — the chair's `[done]` in `quorum` closes the session (a `[done]`
   in `deliberating` is ignored). The plane records the findings ledger and
   bumps fired counters on waived findings, then emits a signed
   `session.terminal` event.
8. **GitHub writes** — the controller turns the terminal event into queued
   writes drained from its outbox: the verdict comment (a NEW comment),
   the `openab/council` status pinned to the webhook head sha (never the
   sha the chair merely claims), and the formal PR review. Round-marker
   reconciliation makes crash replays adopt their earlier success instead of
   double-posting.
9. **No-verdict terminals** — a superseded or timed-out session never leaves
   its started comment claiming to review forever: the controller rewrites it
   into a tombstone ("closed without a verdict").

## Dynamic Replacement

OCP supports two replacement scopes:

- **Future sessions / webhook reviews** — `POST /v1/council/roster/replace`
  updates the DB-backed standing roster override. Future PR webhooks and `/ask`
  sessions use this override; if no override exists, OCP falls back to
  `OABCP_COUNCIL_ROSTER`.
- **Active sessions** — `POST /v1/sessions/:id/roster/replace` swaps one current
  roster member for another registered bot. The replacement keeps the old bot's
  roster position, receives backfilled history, and future fanout uses the new
  roster.

Replacing a bot is one-for-one. The replacement must already be registered and
must not already be in the target roster. Replacing the current chair requires a
bot registered with `role=chair`. OCP purges pending outbox frames for the removed
bot in that session so an offline bot cannot reconnect and continue stale work.

## Follow-Up Comments

Conversational follow-up is separate from a full review:

1. A write-ish commenter posts `/ask <question>`, or `@mentions` the bot when
   `OABCP_BOT_HANDLE` is configured.
2. The webhook opens a comment-scoped `solo` session with
   `trigger_ref="github:ask/owner/repo#N@comment_id"`.
3. The chair self-fetches PR context and writes the answer in-session.
4. The chair sends `[done]`; `Solo` closes and the controller posts the
   answer as a new PR comment.

This was dogfooded on PR #43: a `/ask` comment opened a solo session, the chair
answered as `zeabur-council[bot]`, and the session closed.

## Debugging

Use the north API instead of reading SQLite directly once deployed:

```sh
curl -H "Authorization: Bearer $KEY" \
  "$PLANE/v1/sessions?trigger_ref=github%3Apr%2Fowner%2Frepo%2343"

curl -H "Authorization: Bearer $KEY" \
  "$PLANE/v1/session-log?trigger_ref=github%3Aask%2Fowner%2Frepo%2343%4012345"
```

`GET /v1/sessions/:id` returns the session, messages, roster, and reactions.
`GET /v1/sessions/:id/log` returns a text timeline useful for quick dogfood
investigation.

## Embedded quickstart profile (legacy)

The published Zeabur templates predate the controller: webhooks go straight to
the plane's `POST /api/v1/github_webhooks`, sessions open as `review_council`,
and the chair posts the round comment and verdict from its own pod (PAT or
App credentials on the pod). That profile still works and remains the fastest
way to try the council, but it is not the production write path — migrating it
to the controller is tracked on the roadmap.

## Boundary

OCP is the runtime kernel: sessions, roster, fanout, coordinator policy,
delivery, durable state, auth, and liveness. PR review is the first control
plugin/profile on top of that runtime. See
[ADR 007](adr/007-control-plugins-and-oab-father.md).
