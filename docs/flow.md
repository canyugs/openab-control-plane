# PR Review Council — end-to-end flow

What actually happens when the council reviews a PR, as built and verified
(2026-06-26). Deploy/usage commands live in `TEMPLATE.md`; this is the flow.

## Topology
```
  you ──REST/SSE──▶ control-plane ──gateway /ws──▶ chair  (stock OpenAB Claude pod, has GH_TOKEN)
                         │                        ├─▶ rev1   (stock pod, no GH_TOKEN)
                         │                        └─▶ rev2   (stock pod, no GH_TOKEN)
                         └─ SQLite (/data): bots, sessions, roster, outbox
```
- Pods are **stock** `ghcr.io/openabdev/openab:*-claude` — no fork. They dial OUT to the plane.
- The plane **seeds the roster at boot** (`OABCP_BOTS=chair:chair,rev1:reviewer,rev2:reviewer`),
  giving each bot `id=name`, so pods fetch a static `…/bot-config/<name>` and connect. No manual registration.

## The flow
1. **Open** — `POST /v1/sessions {roster:[chair,rev1,rev2], quorum_n:2, chair_bot:chair, trigger_ref:"github:pr/owner/repo#N"}`.
2. **Trigger** — `POST /v1/sessions/:id/messages` with the review task. For a PR, the
   diff is fetched (`gh pr diff`) and **embedded in the trigger** so reviewers (who
   have no `gh`) can read it. The plane @mentions each roster bot so none are gated out.
3. **Fan-out** — the plane delivers the trigger to every bot except the author
   (`routing.rs`); a forum thread is opened (`channel_type=supergroup`).
4. **Review** — rev1/rev2 each post findings in the thread, then react 🆗
   (OpenAB's default `emoji_done` = 🆗 = the plane's `DONE_EMOJI`). On each done-signal
   the plane relays that reviewer's final message to the chair.
5. **Quorum** — when reviewers-who-🆗 ≥ `quorum_n` (`session.rs`), state →
   `quorum`; the plane prompts the chair to render the verdict.
6. **Verdict + side-effects** — the chair (the only pod with `GH_TOKEN`) synthesizes
   the verdict and acts on the PR via `gh`:
   - in-progress comment → `gh pr comment`
   - verdict comment → `gh pr comment`
   - `gh pr edit --add-label council-reviewed`
   - approve/request-changes → `gh pr review` *(see caveat)*
   then reacts 🆗.
7. **Close** — the chair's done-signal in `quorum` advances state → `closed`; the
   plane emits the `verdict` SSE event and gates further chatter (`deliver_event` +
   `handle_reply` refuse post-close sends). The verdict is also at `GET /v1/sessions/:id`.

## Identity model (why chair-only `gh`)
- Only the **chair** holds `GH_TOKEN` (a fine-grained PAT). Reviewers have none →
  they physically can't write to the PR, so no duplicate comments. They review the
  embedded diff.
- **Self-review caveat:** GitHub blocks approve/request-changes on your *own* PR. The
  token's account must differ from the PR author for `gh pr review --approve` to land;
  comments + labels always work. Reviewing others' PRs is unaffected.

## Verified
A 3-bot council deployed entirely from the template reviewed `canyugs/council-demo#1`
(a planted tax-base bug): `deliberating → quorum → closed`, chair caught the bug,
posted the verdict, applied `council-reviewed`. See [deploy.md](deploy.md) to reproduce.

## Known gaps (see TODO.md)
Auto-trigger (no PR webhook yet — open session manually/script), large-diff chunking,
benchmark/eval, GitHub-App identity (to review own PRs + clean `[bot]` attribution).
