# PR Review Council — end-to-end flow

What happens when the council reviews a PR, as built and dogfooded in the webhook
path. Install commands live in `install-pat.md` and `install-github-app.md`; this
file describes the runtime flow.

## Topology

```text
GitHub webhook / north API
        |
        v
control-plane ── gateway /ws ── chair  (stock OpenAB pod, PR write credential when enabled)
      |                       ├─ rev1   (stock OpenAB pod)
      |                       └─ rev2   (stock OpenAB pod)
      v
SQLite: bots, sessions, roster, messages, reactions, outbox
```

- Pods are stock OpenAB pods. They dial out to the plane over `/ws`.
- The plane seeds the roster at boot from `OABCP_BOTS`, usually
  `chair:chair,rev1:reviewer,rev2:reviewer`.
- The chair is the only actor expected to write to GitHub. It may post through a
  PAT profile or pod-local GitHub App auth; the plane itself does not post PR
  comments.
- Reviewers have no PR write token. On pointer triggers they still need GitHub
  read access for private repos.

## Full Review

1. **Trigger** — a PR `opened` / `reopened` / `ready_for_review` webhook, a
   write-ish commenter's `/review`, or `POST /v1/review {repo, pr, preset?}` asks
   the plane to review a PR.
2. **Open** — the controller creates a session with
   `trigger_ref="github:pr/owner/repo#N"`, `mode="review_council"`,
   `chair_bot=chair`, and `quorum_n` equal to the assigned reviewers.
   Re-delivery dedupes while a non-terminal session with the same `trigger_ref`
   exists.
3. **Pointer trigger** — the plane posts a PR pointer trigger, not an inlined
   diff. Bots self-fetch PR context with `gh` according to the prompt and their
   assigned review angles.
4. **Fanout / starters** — every roster member receives the trigger for history.
   For `review_council`, the chair and reviewers are @mentioned on the opening
   trigger. The chair's opening turn posts/updates a short "OpenAB Council review
   started" PR status comment and does not send `[done]`; reviewers start
   fetching the diff.
5. **Review** — reviewers post findings and signal done with `[done]` or the
   gateway done reaction (`🆗`). The plane records done as a reaction and relays
   each reviewer's settled final message to the chair.
6. **Quorum** — when reviewer done count reaches `quorum_n`, state moves
   `deliberating → quorum`. The plane prompts the chair to synthesize the final
   verdict and complete whatever side effect the opening trigger required.
7. **Chair side effect** — the chair updates the same PR comment with the final
   verdict as configured by the deployment profile and prompt. The plane does not
   run `gh` itself.
8. **Close** — the chair's `[done]` in `quorum` closes the session and emits the
   north `verdict` and `state:closed` events. A chair `[done]` in `deliberating`
   is ignored so an opening-trigger response cannot close the review early.
9. **Watchdog fallback** — if reviewers or the chair stall, the liveness watchdog
   force-closes stale sessions with the work already present in the thread.

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
3. The chair self-fetches PR context and posts a new PR comment answer.
4. The chair sends `[done]`; `Solo` closes directly.

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

## Boundary

OCP is the runtime kernel: sessions, roster, fanout, coordinator policy,
delivery, durable state, auth, and liveness. PR review is the first control
plugin/profile on top of that runtime. See
[ADR 007](adr/007-control-plugins-and-oab-father.md).
