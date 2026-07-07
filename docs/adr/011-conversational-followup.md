# ADR 011 — Conversational follow-up: `@mention` / `/ask` answered by a solo self-fetch session

Status: **accepted** · 2026-06-28

## Context

OCP has matched CodeRabbit's table stakes — auto-review on PR open (the App webhook,
`src/github_webhook.rs`), a structured verdict posted by the bot identity
(`zeabur-council[bot]`, [ADR 004](004-bot-identity-shared-app-pod-local.md)), and a
`/review` on-demand command. The largest remaining interaction gap is **conversational
follow-up**: a user can't `@mention` the bot on a PR ("@zeabur-council why is this a
P1?", "can you suggest a fix?") and get an answer. CodeRabbit's `@coderabbitai …` is a
core part of why it feels like a reviewer rather than a linter.

What exists today constrains the design:

- **The webhook only triggers a full review.** `parse_trigger` (`src/github_webhook.rs:74`)
  matches `pull_request` opened/reopened/ready_for_review and `issue_comment` whose body
  `starts_with("/review")` (line 101). There is **no `@mention` parsing** and no
  command other than `/review`.
- **A review session closes after the verdict and cannot be appended to.** The
  orchestrator drops new sends to a closed session (post-close drop, `handle_reply`).
  `active_session_for_trigger` (`handle_webhook`, line 159) is used only for
  *idempotency* (dedupe a re-delivered `/review`), not to route a follow-up back into a
  running thread. So "reply into the existing council" is not available.
- **The plane is deliberately out of GitHub.** Since v0.1.8 / [ADR 004](004-bot-identity-shared-app-pod-local.md),
  `convene_for_pr` (`src/council.rs:141`) posts a **pointer** trigger (PR ref + angles,
  no diff); the bots **self-fetch** the PR. The plane makes zero GitHub calls and stores
  no PR content.
- **Only the chair writes to the PR**, as the App, maintaining exactly one `--edit-last`
  verdict comment (`scripts/pr-review-trigger-pointer.tmpl`). A conversational answer is
  a *new* comment, not an edit of the verdict.
- **`solo` mode already exists** (`src/coordinator.rs:118`, `for_session("solo")`): one
  bot's done closes the session directly. A Q&A is a single-answer turn, not a fan-in
  deliberation.

## Decision

1. **A follow-up is a separate, lighter flow from `/review` — not a reopened council.**
   A `/review` convenes the full quorum council and produces a verdict. A follow-up
   question is answered by a **`solo` session** (one bot, reusing the existing `Solo`
   coordinator) — cheaper and faster, and the right shape for a single answer. Consistent
   with [ADR 005](005-cost-governance-roster-swap.md) (cheap-by-default).

2. **Trigger surface = `issue_comment` that `@mention`s the bot (or `/ask <q>`).**
   Extend `parse_trigger` with a third arm: an `issue_comment` on a PR whose body
   mentions the App login (e.g. `@zeabur-council …`) or starts with `/ask` →
   `WebhookTrigger { reason: "ask", question: <text> }`. `/review` stays the
   convene-a-council path; `@mention`/`/ask` is the answer-a-question path. Inline
   review-thread replies (`pull_request_review_comment`) are **deferred** (item below).

3. **Reuse self-fetch for context — the plane still makes zero GitHub calls.**
   The follow-up opens a `solo` session and posts a **pointer** trigger: the PR ref +
   the user's question + an instruction to read the PR conversation. The answering bot
   **self-fetches** the PR and its comment thread (`gh pr view --comments` / the diff)
   for context — exactly as review bots already self-fetch the diff. The plane neither
   reads GitHub nor stores the conversation; the thread *is* the state. Consistent with
   [ADR 004](004-bot-identity-shared-app-pod-local.md).

4. **The answer is posted as a NEW PR comment by the App (chair identity).** Not an
   edit of the verdict comment — a follow-up reply is its own comment. The answering bot
   is the chair (the only writer; posts as `zeabur-council[bot]`). A follow-up trigger
   template (sibling of `pr-review-trigger-pointer.tmpl`) tells it to `gh pr comment`
   (plain, not `--edit-last`) with the answer.

5. **Multi-turn is stateless on the plane — keyed by PR, context re-fetched each turn.**
   Each `@mention` opens a fresh `solo` session (trigger_ref `github:ask/<repo>#<n>` or
   `…/comment/<id>`); the bot self-fetches the *whole* thread each time, so prior turns
   are context without a plane-side conversation store. Idempotency: dedupe by the
   triggering comment id so a re-delivered webhook doesn't double-answer (extend the
   `active_session_for_trigger` check to the comment-scoped key).

6. **Permission + allowlist become load-bearing here.** Comment-command triggers
   (`/review`, `/ask`, `@mention`) require a write-ish GitHub commenter
   (`OWNER`/`MEMBER`/`COLLABORATOR`), and `OABCP_ALLOWED_REPOS` provides the per-repo
   allowlist hook. Production deployments should set that allowlist; deeper
   CODEOWNERS/team policy remains enterprise hardening.

## Consequences

- **New:** a `parse_trigger` arm + `question` field; a `convene_ask` (solo + pointer)
  in `src/council.rs`; a `pr-ask-trigger-pointer.tmpl`; comment-id idempotency; and
  the comment-command permission / repo-allowlist guardrails. The session/message model
  and `Solo` coordinator are reused unchanged.
- **Plane stays thin** (ADR 001): it routes the mention and opens a solo session; it
  does not read GitHub, post, or hold conversation state. The bot self-fetches and
  the chair posts — same trust/credential boundary as review.
- **Cost-bounded** (ADR 005): one bot, one turn, the existing watchdog timeout.

## Deferred (not in v1)

- **Inline review-thread replies** (`pull_request_review_comment` events + posting via
  the review-comment replies API) — answering *inside* a CodeRabbit-style line thread,
  not just at PR level.
- **`/resolve` and review-state mutation** from a comment (ties to the separate
  *Decision→review-state* roadmap item).
- **True single-session back-and-forth** (streaming a live thread into one open
  session) — the stateless re-fetch model above is simpler and sufficient first.

## Effort

Trigger parse (item 2): small (~½ day, parse + test). The full loop (items 3–6:
solo-ask convene + pointer template + new-comment post path + comment-id idempotency +
the permission/allowlist gate): medium, ~2–4 days. Tracked in a new issue.

## Amendment 2026-07 — deterministic command tier

The mention surface grows a deterministic command tier, with no LLM intent
classifier. Per [pr-mention-plan §2](../pr-mention-plan.md#2-command-grammar--trigger-surface),
comment-leading `@handle review [fix notes]` and `@handle full review` become
re-review commands at P2; all other mention text remains the solo ask path from
this ADR. The command tier is active only when `OABCP_BOT_HANDLE` is set; unset
means mention parsing is off and fails closed. The ask path never supersedes a
review council.
