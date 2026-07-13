# ADR 025 — Tell the requester: a plane-posted status notice when a review can't complete

Status: proposed · 2026-07-13

## Context

ADR 023 makes a broken bot *visible to operators* (a `WARN` log + `health` field)
and ADR 023 Phase 4 *routes future convenes* around it. Both serve the operator.
**Neither reaches the person who asked for the review.** On 2026-07-13 the chair
degraded mid-review; the session timed out; the plane emitted its internal
`timeout`/`verdict` events and fired the close webhook — and posted **nothing to
the PR**. The requester saw an open PR with a review that never came: silent for
~a day, from their side.

This silence is structural, not incidental: in the normal path the **chair** posts
the verdict to GitHub (agent-delegated `gh`). When the chair is the thing that
died, no actor posts — and the plane, which force-closes the stuck session
(`force_close_timeout`), deliberately makes no GitHub content calls.

### The invariant, examined

"The plane makes zero GitHub calls" is the usual shorthand, but it is already
false in a precise way: the plane's `GitHubApp` mints installation tokens,
**revokes** them, and does live permission checks (`user_repo_permission`, ADR
019 D2). What the plane does *not* do is post **review content** — LLM output
stays agent-delegated, which is what keeps the App key away from untrusted-input
processing (ADR 019 C3). A *canned operational status* ("the review couldn't
complete") is not review content: it is fixed, non-LLM, carries no PR data back
into a model, and is analogous to a commit status. So it does not reintroduce C3.

## Decision

1. **Permit the plane to post a single canned operational status notice to a PR
   — and only that.** Fixed strings describing service state ("review temporarily
   unavailable / could not complete"). Never review content, never model output,
   never anything derived from the PR diff. Review content stays agent-delegated.
   This is the narrow, named exception to "the plane posts no content."

2. **Trigger: a PR-review session that reaches a terminal close with no
   synthesized verdict.** Hooked in `force_close_timeout`, the one place the plane
   already owns a stuck session's death. Triggering on *verdict-less close* (not
   on `health == degraded`) is deliberately broader — it catches every silent
   failure, including ones passive detection missed, and needs nothing from ADR
   023. A normal close (chair synthesized) already posted its verdict and gets no
   notice.

3. **Default OFF, opt-in per lane (`OABCP_PLANE_STATUS_NOTICE`).** Like
   auto-failover (ADR 023 Phase 4), the capability ships dark: enable in dev,
   confirm it reads right on a real PR, then prod under the deploy gate. Merging
   changes no behavior until the flag is set.

4. **Idempotent by marker, distinct from the review comment.** The notice carries
   `<!-- openab-council-status -->` — a *different* anchor than the review
   comment's `<!-- openab-council -->` — so it never clobbers a real review and a
   repeat outage upserts one notice, not a pile. (First cut may post-once and
   leave upsert to the [[council-comment-upsert]] / #226 work; the marker is
   present from day one so the upsert can attach later.)

5. **Fire-and-forget, minted fresh.** The notice posts on a spawned task (like
   `fire_close_webhook`) with a freshly minted chair-scoped token — never the
   session tokens, which are being revoked in the same close. A failed post is a
   `WARN`, never blocks the close.

## Non-goals

- **Not a substitute for failover.** Phase 4 keeps the *next* review working; this
  tells the requester about the *one that just failed*. Complementary.
- **No status dashboard, no per-PR SLA.** One canned comment on the failed PR is
  the whole surface, matching ADR 023's "a field + a log" restraint.
- **No convene-time refusal (yet).** Posting "unavailable" the instant a request
  arrives with no healthy chair is the natural sibling trigger, but failover
  usually seats a standby, so the verdict-less-close trigger covers the real gap
  first. Carry convene-time as a follow-up.
- **The plane still posts no review content.** If this ADR ever tempts a
  richer, PR-derived message, that crosses back over the C3 line — stop.

## Migration / build order

1. `GitHubApp::post_pr_comment(repo, pr, token, body)` — the one new outbound
   primitive, mirroring the existing mint/revoke/permission calls.
2. `force_close_timeout`: if the session is a PR review that closed with no
   synthesized verdict and the flag is on, spawn a canned marker-anchored notice.
3. Follow-ups: marker upsert (with #226), and the convene-time sibling trigger.

## References

- ADR 023 (bot agent-level liveness) — the operator-facing half; this is the
  requester-facing half of the same incident.
- ADR 019 (untrusted-PR-input boundary) — the C3 line this decision is careful to
  stay behind (canned status ≠ review content).
- [[council-comment-upsert]] / #226 — the marker-upsert mechanism this notice will
  share.
