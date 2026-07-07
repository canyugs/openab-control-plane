# PR Mention & Re-review Plan — grammar, supersession (M1), delta context (M2), findings ledger (M4)

Status: **proposed** · 2026-07-08 · builds on the boundary-review-2026-07
Addendum (findings M1–M4), [ADR 011](adr/011-conversational-followup.md)
(mention → solo ask), [ADR 008](adr/008-external-controller-protocol.md)
(controller protocol). Planning only — no implementation in this document.

## Summary

The product goal: replace CodeRabbit for the author's loop — open PR →
council verdicts head H1 with numbered findings → author pushes fixes and/or
comments `@zeabur-council review <fix notes>` → a cheaper round reviews the
delta, marks findings Resolved/Outstanding, never re-raises → repeat until
approve; every stale round visibly superseded, never silently wrong. The
competitive survey's takeaway (facts inlined here; no separate doc): its weak
points are precision and undefined in-flight-push behavior — we win on M1
correctness and delta signal, not command count.

Load-bearing observation (Gen-1): the mention was never a trigger there
(`multi-agent-review-ops/TODO.md:20-25`) — the running loop is
push-triggered, fix notes read via self-fetch. OCP has every piece except
one: a `synchronize` during an active council is **swallowed** as a dedupe
(github_webhook.rs:335-345) and the council verdicts on the stale head
(**M1** — a live bug). So: fix M1 in the plane as the one new mechanism; ship
mention grammar and delta context as thin, flagged app logic; keep findings
memory as steering prose until Stage 3 owns M4. One honesty correction this
plan makes against its own first draft: **M3 has no structural fence on the
GitHub comment surface before P3** — the chair's posting credential is
pod-local (ADR 004), so token revoke never bites it (§5). The plan claims
only what holds.

## 1. Goal & non-goals — CodeRabbit parity map

Table stakes from the survey, mapped to this plan's phases (§7):

| Capability | Disposition |
|---|---|
| `review` (incremental, delta-scoped) | **P0** prose (push / `/review` + self-fetch delta) → **P2** `@handle review` |
| Auto incremental re-review on push | **P1** (`synchronize` supersedes instead of being swallowed) |
| `full review` (from-scratch escape hatch) | **P2** (parse keyword; omit the delta header) |
| Fix-notes verify loop ("fixed F1 in `<sha>`") | **P0** prose via thread self-fetch → **P2** notes carried in trigger |
| Free-form chat at PR level | shipped ([ADR 011](adr/011-conversational-followup.md)) |
| Unresolved-findings ledger; `status` / `resolve F3` | **P3** (M4 — controller state) |
| `help` / command card | **P3** (needs a non-LLM GitHub write path = controller-owned side effect, §5 P3 — *not* `emit_status`); interim: verdict footer |
| Rate/cost valves | **P1** hourly auto-cap + per-PR round budget |
| Inline review-thread replies | defer — trigger: M4 shipped + first dogfood request (ADR 011 deferred item) |
| `pause` / `ignore` | defer — trigger: first noise/cost complaint the caps can't handle |
| `summary` placeholder, `generate docstrings/tests/diagram`, `autofix`, `plan`, config dump | drop — write-back/content generation is a different product; trigger: explicit product decision |
| Dashboard RBAC, learnings/preference memory | drop for now — trigger: controller state stabilizes post-M4 |

## 2. Command grammar & trigger surface

**Deterministic only — no LLM intent tier.** A classifier that doesn't exist
can't hallucinate; Gen-1's own design needed no LLM for its command tier
(`TODO.md:19-25`). Free text stays `/ask`.

| Ingress event | Parse rule | Action | Phase |
|---|---|---|---|
| `pull_request` opened/reopened/ready_for_review/**synchronize** | existing arm (github_webhook.rs:155-177) | convene-or-supersede; fingerprint `sha:<head_sha>` (§3) | P1 |
| `issue_comment` `/review` | existing arm (:189-206); P1 adds `comment.id` capture — today `comment_id: None` (:204) | convene-or-supersede; fingerprint `cmd:<comment_id>` | P1 |
| `POST /v1/review` (north REST — a third ingress with the same swallow, api.rs:1252-1258) | n/a (authed REST) | convene-or-supersede; no SHA or comment id in the request → fingerprint NULL = **always-supersede** (explicit command semantics; retries are caller-owned — REST has no GitHub-redelivery story; the round budget bounds abuse) | P1 |
| `@handle review [free text…]` | **comment-leading** mention + keyword, case-insensitive, word-boundary | re-review round; rest of body = author fix notes, carried verbatim in the trigger (M2 channel); fingerprint `cmd:<comment_id>` | P2 |
| `@handle full review` | comment-leading keyword pair | same, but the successor prompt omits the delta header ("review from scratch") | P2 |
| `@handle <anything else>` / `/ask` | fallthrough | solo ask, unchanged (ADR 011); **never supersedes** — disjoint `github:ask/…` namespace (council.rs:221-226) | shipped |
| `@handle status` / `resolve Fn` / `help` | keyword | ledger commands | P3 |

Parsing rules:

- Handle matching reuses `parse_ask_comment` word-boundary rules incl. the
  `[bot]`-suffix tolerance (github_webhook.rs:81-105); mention grammar is
  active only when `OABCP_BOT_HANDLE` is set (:70-75) — unset = off,
  fail-closed. The `review` keyword check runs before the ask fallthrough.
- **The command tier is comment-leading** (mirrors `starts_with_slash_command`,
  :107-110). This is load-bearing, not style: `parse_ask_comment` matches the
  handle *anywhere* in the body (`c.find(&tag)`, :92), so a GitHub quote-reply
  copying the footer (`> … @handle review …`), a code-span paste, or a
  mid-sentence "…@handle review of the auth flow missed X" would otherwise
  convene a paid panel **and supersede a live council**. Comment-leading kills
  all three (quoted/fenced text never starts the body). Match-anywhere stays
  for solo asks only — a stray quoted mention costs one cheap ask, never a
  supersede; accepted pre-existing behavior.
- **Accepted ambiguity:** `@handle review the auth flow` (comment-leading)
  triggers a re-review with those words as notes, not a question. `/ask` is
  the escape hatch; documented in the footer. Revisit on measured confusion.
- **Discoverability:** the footer advertises `@handle <question>` and `push
  or @handle review <fix notes>` — command examples rendered **inside code
  spans** so a quote-reply can never self-match even if the leading rule ever
  loosens. Primary edit point is the **code twin** (orchestrator.rs:94-97):
  on the webhook path the chair never sees the template — fanout rewrites the
  client trigger (orchestrator.rs:157-170) into a text that mandates its own
  verdict format and today carries **no footer at all**; tmpl:32-35 is the
  secondary copy. Q4's single-source collapse is a P0 requirement (§9).

Permission gates (all plane ingress):

- `can_command` (`author_association ∈ {OWNER, MEMBER, COLLABORATOR}`,
  github_webhook.rs:55-57) stays on **every** comment-command path. Pushes
  are ungated — the author already holds push. A PR-author arm is deferred —
  open question Q1.
- **New explicit bot-author guard (P1):** drop `issue_comment` events where
  `comment.user.type == "Bot"`. Today the only loop protection is the
  *implicit* `author_association:"NONE"` of App comments — and P0 introduces
  a footer carrying the literal commands, so the guard must land before or
  with it: "no self-trigger even when the chair is buggy" must be structural.
- HMAC fail-closed (:240-249) and `OABCP_ALLOWED_REPOS` (:61-66) unchanged.

**Boundary flag:** this parsing lives in `github_webhook.rs` today —
grandfathered per the boundary review's B1 ruling, tagged for Stage 3
extraction into `plugins/pr_review`; the plane's durable share is only
"webhook event → session op (open/supersede/dedupe/ignore)" (Gen-1 B10:
grammar buried in webhook conditionals is why mention never shipped there).
The P2 arm amends the Addendum's M2 stage map — argued in §7 P2.

## 3. Supersession (M1) — the one new kernel mechanism

**Invariant (I1).** At most one active review session per PR, targeting the
trigger that convened it: a retry/redelivery never double-convenes and never
supersedes; a new head or new command never lands on a stale council. Must
hold under concurrent deliveries and dead/buggy bots → **plane mechanism**.
The rule (Stage 0 doc item): *same-delivery retry dedupes; a new-head or
new-command trigger supersedes* — today's swallow is undeclared and wrong.

**Declared residual — out-of-order delivery.** GitHub guarantees no
cross-delivery ordering, and the plane makes no GitHub calls to compare
ancestry (council.rs:7-10) — so under rapid pushes an *older* `synchronize`
delivered late can supersede a newer round and verdict on the older head.
Resolved toward the smaller plane: **no recency mechanism in v1.** Bounded:
the next trigger re-supersedes, and the P0 `Reviewed at <sha>` label makes
the staleness visible; note honestly that push storms — where reordering is
likeliest — are also where the hourly cap lets a stale-labeled round stand.
Re-open trigger: a dogfooded wrong-head verdict the label didn't surface;
then add a payload-only recency rule (store the event's
`pull_request.updated_at`, refuse strictly-older supersedes).

**Fingerprint.** New nullable app-opaque column `sessions.trigger_fingerprint
TEXT` (guarded additive `ALTER TABLE` per store.rs:387-389). The plane only
compares it for equality — no ADR-013-style shape leakage. Ingress writes:

- `pull_request` events → `sha:<head_sha>` from `pull_request.head.sha`
  (in the payload but uncaptured today). Prefer it over top-level `after`
  deliberately: `head.sha` is rendered at delivery, so two back-to-back
  pushes can deliver two events carrying the same newest head while `after`
  differs per event — collapsing rapid pushes into one dedupe is the desired
  outcome. A base-branch "Update branch" merge also fires `synchronize` and
  supersedes with no author change — accepted, cap-covered.
- Comment commands → `cmd:<comment_id>`. `issue_comment` payloads carry no
  head SHA and the plane makes zero GitHub calls (council.rs:7-10, ADR 011) —
  the reviewed head for command rounds is pinned by the chair in the comment
  body, not plane-verified.
- `POST /v1/review` → NULL = always-supersede (§2 table).
- Consequences fall out for free: redelivery reuses the SHA/comment id →
  equal fingerprint → dedupe; a new push or second command → fresh
  fingerprint → supersede. Pre-migration sessions have NULL fingerprints;
  NULL compares as never-equal, so any new trigger supersedes them.

**Atomic store op — routed through the interpreter.** The swallow lives in
*two* layers above the store: the webhook pre-check (github_webhook.rs:335-345)
and `controller::open_session`'s own active-trigger dedupe
(controller.rs:43-63), which `convene_for_pr` reaches via
`controller::execute` (council.rs:163). Replacing only the pre-check
re-swallows one layer down. P1 therefore changes the interpreter, not just
the handler — bypassing it would cut against the boundary review's B3
direction:

- `OpenSessionAction` gains `trigger_fingerprint`; `open_session` calls a new
  `create_session_superseding(…) → (Session,
  Outcome{Created|Deduped|Superseded{old_id}})`; `ControllerActionResult`
  grows the `Superseded` outcome; post-commit side effects run at the caller.
- The `/v1/review` handler's duplicate pre-check (api.rs:1252-1258) is
  deleted for the same reason. The **ask dedupe (:281-295) is untouched** —
  asks never supersede (§2).
- One SQLite transaction: (1) read the active row for `trigger_ref` (partial
  index `idx_sessions_active_trigger_ref`, store.rs:430-435, stays the
  backstop); (2) equal fingerprint → `Deduped` (existing
  `create_session_deduped` semantics, store.rs:882-914); (3) different/NULL →
  CAS the old row to `closed` (as `close_if_active`, store.rs:1122-1130) +
  INSERT the successor, same transaction.

**Ordering of the valves.** The hourly cap and round budget (below) are
checked **before** the supersede transaction, against session history; a
refusal is dedupe-shaped — the running session stays active and untouched, a
north event fires. A refusal must never close the live round and then refuse
its successor.

**Race analysis.** Close-then-open as two calls has a half-open window: a
webhook landing between them sees no active session and can convene a third
candidate that loses the index race — the single transaction is the point.
Concurrent deliveries serialize on the store mutex; the loser re-reads —
equal fingerprint ⇒ `Deduped` onto the winner, newer fingerprint ⇒
supersedes again; exactly one active successor either way.

**Post-commit side effects** for the superseded session (idempotent,
at-least-once; caller = the `Superseded` outcome's handler):

- Revoke its session-minted GitHub tokens (orchestrator.rs:1110-1116;
  identity.rs:137-156 — purge + *best-effort spawned* server-side revoke,
  warn-only, no retry). This fences only ADR-009-path tokens; it is **not**
  the M3 fence for the chair — see §5.
- Purge its outbox rows — **hard dependency on the Stage 1 A5
  purge-on-close** item; without it the stale chair replays its "synthesize
  the verdict" prompt on reconnect and the M3 window reopens.
- Emit north `state:closed` + fire the ADR 012 webhook with new
  `reason:"superseded"` (reason is a call-site argument today,
  orchestrator.rs:528-550 — no schema change). Without the distinct reason,
  downstream consumers misread superseded rounds as failures.
- **Crash window, stated fully:** a crash between commit and side effects
  leaves session-minted tokens live until expiry and outbox rows until the
  A5 sweep — bounded; but the `reason:"superseded"` events are **lost, not
  delayed** (no re-drive exists). Acceptable pre-P3 (consumers are humans on
  dogfood). P3's comment lifecycle consumes the reason — that is the §8
  `close_reason` trigger firing: Stage 3 adds a durable reason (column or
  startup re-emit sweep, decided in the Stage 3 ADR).
- **No verdict parse** — decision stays NULL (trailer parse is normal-close
  only, orchestrator.rs:1119-1140). NULL distinguishes superseded from
  **verdicted** rounds only: timeout closes never parse a trailer
  (force_close_timeout, orchestrator.rs:272-337) and malformed/absent
  trailers also yield NULL (:1134-1140; the webhook payload's own comment
  says "all null on timeout / missing trailer", :543-545). Superseded vs
  timeout vs mangled-trailer is distinguishable *only* via the event/webhook
  `reason` — exactly why the §8 non-goal's re-open trigger exists.

**Interplay.** The successor is a **fresh session**: fresh watchdog clock
(reuse would inherit the old `created_at` timeout) and fresh done-signal
quorum (fixes Gen-1's quorum-re-wait gap, lesson B2: round boundaries in
session memory) — session reuse across rounds is **rejected permanently**.
Roster/preset re-resolve at convene (council.rs:30-51, :93-138); the mention
arm reads `issue.labels` for the preset as `/review` does
(github_webhook.rs:202). `aborted` stays untouched — `closed` + reason
suffices.

**Cost bounds** (guarantee test: must bound spend under a looping commander →
plane check; the *values* are policy env):

- **Hourly auto-cap:** if `list_sessions(trigger_ref)` (store.rs:946-1034)
  shows ≥ K sessions created in the last hour (K=3 default,
  `OABCP_REVIEW_HOURLY_CAP`), a `synchronize` dedupes instead of
  superseding, and logs; the running round completes and verdicts pinned to
  its now-stale head, labeled "head has advanced" (§5) — a labeled
  slightly-stale verdict beats silence in a push storm. Explicit human
  commands bypass the cap (intent, gated). Session history *is* the counter.
- **Per-PR round budget:** sessions for the ref ≥
  `OABCP_REVIEW_ROUND_BUDGET` (default 10) → refuse convene on **all** paths,
  emit a north event — the backstop for a malicious-but-authorized human
  loop that the bot-author guard and `can_command` don't cover.
- Supersession itself is a cost *reducer* vs Gen-1: stale rounds stop
  spending at supersede instead of running seven agents to a prose abort at
  synthesis (`aggregator-output.md:41-52`).

**What stays policy:** which events supersede, cap values, fingerprint
choice — grandfathered in `github_webhook.rs`/`council.rs`, tagged for the
Stage 3 controller. The store op and fingerprint column are kernel, permanent.

## 4. Re-review context (M2) + findings ledger (M4)

**Layer ruling first.** "Round N+1 never re-raises resolved findings" cannot
survive a hallucinating reviewer — it fails the guarantee test, so it is
**not a plane guarantee**; its failure mode is wasted attention, never state
corruption (the safety half — stale-head verdicts — is M1's job). Delta
content is controller policy; per-finding state is **controller state, never
kernel schema** (ADR 013's count columns are already flagged leakage — a
findings table would tip that scale decisively; Addendum M4).

**Decision: v1 is thread-is-state (prose + self-fetch) with exactly two
plane-carried strings; the structured ledger waits for Stage 3.** The prior
verdict comment is the findings ledger reviewers self-fetch (ADR 011 stance),
and Gen-1 proves the format — but three GitHub realities would silently
destroy that ledger, so P0 must carry their belts:

- **Ledger preservation (was a silent self-destruct).** The chair maintains
  exactly one comment via `--edit-last --create-if-none` (tmpl:17-20), and
  its opening turn overwrites that comment with "review started" (tmpl:22-28;
  code twin orchestrator.rs:94-97). Round N's chair is the *same App user* as
  round N−1's — the opening edit would wipe the round-N−1 verdict at the
  exact moment round-N reviewers are told to self-fetch it. P0 fix (steering;
  chosen over one-comment-per-round to keep the single-comment UX): on a
  re-review, the opening turn must first fetch the current comment body and
  **prepend** the in-progress status above the retained ledger — never
  replace it. Prompt-enforced; failure = ledger lost = next round degrades to
  comment archaeology (noise, not corruption), consistent with this ruling.
- **`--edit-last` mis-targeting.** `--edit-last` edits the bot user's *last*
  comment, and ask answers are new comments by the same user (pr-ask
  tmpl:16-20) — an ask interleaved with a review round makes the chair's next
  edit clobber the ask answer, and P2's footer deliberately increases that
  interleaving. P0 belt: before any `--edit-last`, list own comments and
  verify the last one starts with the council marker line; if not, post a
  new comment instead. Structural fix is P3's comment-by-ID.
- **Rebase/force-push.** `synchronize` payloads carry no `forced` flag (that
  exists only on `push` events) — the plane *cannot* detect a rebase;
  detection is chair-side by necessity. After one, `<reviewed-sha>` may be
  unreachable or `git diff <reviewed-sha>..HEAD` spans upstream commits. P0
  steering and the P2 header both carry: if `git merge-base --is-ancestor
  <reviewed-sha> HEAD` fails, fall back to a full review and say so in the
  verdict. Rebase-heavy authors (the actual nuphos workflow) hit this on the
  first fix round.

The tiers:

- **P0 (steering prose; single-sourced first — Q4 is a P0 pre-task, §9):**
  chair verdict gains a `Reviewed at <head-sha>` line and stable finding IDs
  `F1..Fn`, monotonic across rounds, with Resolved / Outstanding / New tables
  on re-review rounds, never re-raising Resolved (Gen-1 reqs 1-4); plus the
  three belts above. Reviewer prompt: if an OpenAB verdict comment exists,
  this is a re-review — read it and author fix-note comments, verify each
  open finding against the current head keeping its F-number, scope new
  analysis to the delta (with the ancestor check). Ask prompt gains a
  redirect ("push or comment `/review` for a re-review"). Honestly
  prompt-enforced (Gen-1 lesson B5's class) — acceptable only because P3
  replaces it and the failure is noise.
- **P2 (two injected strings):** the successor trigger renders the prior
  round's fingerprint ("review the diff since `<sha>`", when it was a SHA)
  and the author's fix notes verbatim — the piece Gen-1 never wired. `full
  review` omits this header. **The strings must survive the fanout rewrite:**
  for `mode=review_council` the orchestrator *replaces* the client trigger
  per recipient via `review_trigger_context` → recipient texts
  (orchestrator.rs:157-170, :71-85, :87-113), which today carry only
  repo/PR/angles/diff — anything rendered only in council.rs is a dead
  letter. P2 scope therefore includes extending `ReviewTriggerContext` to
  carry a delta/notes block into both recipient texts; this is also why Q4's
  collapse precedes it. Grandfathered, flagged.
- **P3 (Stage 3 ledger):** `plugins/pr_review` owns a findings table (own
  state, not `sessions`): IDs minted once per PR, `open/resolved/dismissed`
  lifecycle, written by the controller parsing a machine-readable findings
  block in the chair's verdict (shape = Q3); round N+1 injects the open set
  so never-re-raise becomes a diff against ground truth, not reviewer memory
  (Gen-1 B3/B4: 69% multi-round, findings re-discovered across 5 rounds via
  comment archaeology). Enables `status` / `resolve Fn`.

**Panel size:** unchanged in v1 (`review:lite` preset exists,
council.rs:93-138). Reduced re-review panel — defer; trigger: measured
round-N spend > 50% of round-1 on dogfood (eval-harness; P2 records it).

## 5. Superseded side-effect race (M3)

The plane cannot prevent a pod-side `gh` call (design.md:46) — the chair
owns `--edit-last`, `gh pr review`, and the commit status (tmpl:17-55).
**Correction against this plan's first draft:** the chair's posting
credential is *pod-local and session-independent* by decision — ADR 004: "the
plane does NOT mint or distribute GitHub tokens for posting"
(adr/004:32-43; council.rs:7-10) — provisioned as a chair pre-boot hook that
mints installation tokens from GitHub with a pod-held App key and re-auths
every 50 minutes (api.rs:1119-1124, :1178-1193; `get-gh-app-token.sh`).
`revoke_session_github_tokens` (identity.rs:137-156) touches only tokens
minted via `POST /v1/sessions/:id/github-token` — an endpoint nothing on the
review path ever calls (only mint site: api.rs:1310). A superseded chair's
`gh` keeps working until its pod rotates; "your `gh` calls fail auth ⇒ you
were superseded" never fires and is deleted from the steering. M3 splits
honestly:

- **The record is structurally immune (plane; exists + P1).** A superseded
  close stores no verdict, and the once-only Close CAS
  (orchestrator.rs:1103-1108) means no later path can. Late sends and
  `create_topic` are dropped at the closed gate (orchestrator.rs:867-878);
  edits/reactions are deliberately exempt and land **inert** — the CAS, not
  reply-dropping, is what protects the record. The A5 purge means a
  superseded chair usually never receives its synthesis prompt at all. P1
  adds one kernel clause: **the session-token endpoint refuses (409/410)
  closed/aborted sessions** — today it checks existence and roster but never
  state, and re-mints on cache miss (api.rs:1277-1319 → identity.rs:98-116),
  so revoke alone is self-defeating even for ADR-009-path pods. With the
  refusal, session-minted tokens are terminally fenced.
- **The GitHub comment surface has no structural protection before P3 —
  declared.** Residual: a chair already holding its synthesis prompt at
  supersede time posts a stale verdict comment/review/status with working
  credentials; harm is a mislabeled comment surface (visibly stale via the
  P0 `Reviewed at <sha>` line, later overwritten by the successor), never
  record corruption. Rejected alternative, resolved toward the smaller plane
  and the Addendum's M3 direction ("never into the kernel"): moving chair
  posting onto per-session scoped tokens would make revoke a real fence but
  reverses ADR 004 decision 4 and puts the plane in the posting-credential
  path. Re-open trigger: verdict delivery becomes plane-held (boundary
  review B4).
- **Steering, P0 (belt, not fence).** Chair quorum-turn step 0: always pin
  `Reviewed at <sha>`; if `headRefOid` (already fetched, tmpl:43-49)
  differs, add "head has advanced — push or `/review` to re-run"; run the §4
  marker check before `--edit-last`. Decision: label-and-post, not
  abort-on-moved — Gen-1 aborted synthesis on head-moved, but the hourly cap
  deliberately lets an over-cap round finish (§3), so head inequality is not
  a supersession signal. Fails the guarantee test (a hallucinating chair
  skips it); residual harm is a cosmetic clobber. Edited in the single
  source per Q4.
- **Controller, P3 (structural close).** The controller owns the canonical
  PR comment **by stored comment ID**, posting/editing on typed verdict +
  close events. **Write-path correction:** ADR 008's `emit_status` records an
  operator-visible status *inside OCP*, only "optionally mapped later" to
  provider surfaces (adr/008:157) — it is not a GitHub write; provider writes
  are controller-owned side effects declared under the manifest's
  `sideEffects`, "not OCP-executed connector operations" (adr/008:223-228).
  So P3's comment lifecycle is a controller-performed GitHub write under that
  declaration, its credential story decided in the Stage 3 controller ADR
  (controller-held installation token out of the plane process, or the
  controller steering the chair pod) — a bundled in-process controller doing
  GitHub I/O would collide with ADR 004 decision 4 ("GitHub I/O belongs to
  the pods", adr/004:44-47), a tension that ADR must resolve, not this plan.
  `emit_status` stays OCP-side status/audit. This kills identify-by-convention
  (Gen-1 B8) and the stuck-"processing" comment: the controller edits on
  *any* session end (`reason` durable by then, §3 crash note), not only on
  the next trigger (B7).

## 6. Layer map

Guarantee test (design.md): "must it hold even if a bot is slow/dead/buggy/malicious/hallucinating?" → plane; else policy/steering.

| Moving part | Layer | Guarantee-test one-liner |
|---|---|---|
| Atomic supersede txn + `trigger_fingerprint` column + interpreter routing | **Plane mechanism** (P1, permanent) | one-active-per-ref must hold under concurrent webhooks + dead chair (out-of-order residual declared, §3) |
| Close CAS + closed gate + A5 outbox purge | Plane (CAS/gate exist; A5 = Stage 1 prerequisite) | the *record* must stay verdict-free when the superseded bot posts late or reconnects |
| Token revoke + closed-session refusal at the session-token endpoint | Plane (P1) | terminally fences ADR-009-path tokens; the chair's pod-local posting identity is out of scope by ADR 004 — declared, not fenced |
| `reason:"superseded"` on webhook + north events | Plane (lost on crash pre-P3 — declared §3) | downstream must distinguish supersede from failure with zero bot cooperation |
| HMAC, allowlist, `can_command`, bot-author guard | Plane ingress | must hold under a malicious commenter and a self-mentioning buggy chair |
| Hourly cap + round budget (checked before the txn) | Plane check; values = policy env | cost must stay bounded under a looping commander; a refusal never kills the live round |
| Supersede-vs-dedupe event policy, mention grammar, preset choice, notes extraction | Controller policy — **grandfathered** in `github_webhook.rs`/`council.rs`, exits at Stage 3 (B1) | a wrong parse wastes a round, never corrupts state |
| Delta prompt content (what context round N gets) | Controller policy (P2 interim in council.rs + orchestrator rewrite) | a hallucinating reviewer degrades quality only |
| F-numbers, ledger preservation, marker check, rebase fallback, never-re-raise | Chair steering | fails under hallucination; failure = noise, not corruption |
| Head-advanced label + comment surface pre-P3 | Chair steering **only** — M3's declared residual | belt for a stale chair with working pod credentials |
| Findings ledger, `status`/`resolve`/`help`, comment-by-ID lifecycle | Controller (Stage 3); GitHub writes as manifest `sideEffects`, `emit_status` = OCP-side status | app data — never kernel schema (M4 ruling) |
| Verdict `gh` side effects (comment/review/status) | Chair steering / OAB pod | plane cannot prevent pod-side `gh` (design.md:46) — declared boundary |
| Typed `[[done]]`/`[[verdict:]]` | Plane parse (exists) + OAB typed upstream (Stage 2) | must hold when a bot omits etiquette |

## 7. Phased delivery

Each phase names its trigger; exits are live-verified on dogfood.

**P0 — docs + steering prose (Stage 0; trigger: fired — this plan).**
Closes: M1 rule declared; M2/M3 prose tier.
Scope: this document; supersede-vs-dedupe rule into design.md; reconcile
design.md:46 / roadmap.md:66 with the actual purge + best-effort-revoke
behavior (identity.rs:137-156 contradicts both today); short ADR amending
ADR 011 (mention grows a deterministic command tier); `reason:"superseded"`
added to ADR 012's payload spec; **Q4 single-source collapse as a pre-task**;
then the §4 P0 + §5 steering edits land in the single source (template +
whatever twin remains).
*Exit:* docs merged; a dogfood PR goes two sequential (non-overlapping)
rounds; round-2 verdict shows `Reviewed at <sha>` and Resolved/Outstanding
keyed on round-1 F-numbers with no Resolved re-raised, **and the round-1
ledger survived the round-2 opening edit**; an ask interleaved between
rounds is not clobbered by the next verdict edit (marker check); an ask for
a re-review gets the redirect answer.

**P1 — M1 mechanism (Stage 1; trigger: fired — verdict-on-stale-head is a
live bug; hard dependency: A5 purge-on-close lands with or before it).**
Closes: M1; M3's record-immunity backstop (not the comment surface — §5).
Scope: capture `pull_request.head.sha`, `comment.user.type`, **and
`comment.id` on the `/review` arm** in `parse_trigger`
(github_webhook.rs:152-230; today `comment_id: None`, :204); bot-author
guard; `trigger_fingerprint` column (store.rs migrate, :387-389 convention);
`create_session_superseding` store op; **interpreter routing** —
`OpenSessionAction.trigger_fingerprint`, `open_session` →
`create_session_superseding`, `ControllerActionResult::Superseded{old_id}`
(controller.rs:43-63); replace the review-path pre-checks
(github_webhook.rs:335-345 **and** api.rs:1252-1258 `/v1/review`); ask
dedupe (:281-295) untouched; **closed-session refusal at the github-token
endpoint** (api.rs:1277-1319); supersede side effects (token revoke + outbox
purge + `reason:"superseded"`, orchestrator.rs:528-550, :1110-1116); hourly
cap + round budget, checked before the txn.
*Exit:* (a) push mid-council → old session `closed` `reason:"superseded"`
with empty outbox, successor active on the new head, verdict cites the new
SHA; (b) replayed delivery (same head SHA / same comment id) →
`deduped:true`, no third session; (c) two concurrent synchronize deliveries
→ exactly one active successor; (d) `POST /v1/sessions/:id/github-token` for
the superseded session is refused, and a superseded chair's forced late send
is dropped with no stored verdict — its late `gh` call itself **succeeds**
(pod-local identity; declared §5, do not test for auth failure); (e) 4 rapid
pushes → cap holds at K, `/review` still supersedes; (f) budget refusal
leaves the running council active and emits a north event.

**P2 — mention grammar + carried context (Stage 1 tail; trigger: P1 exits
verified).** Closes: M2 interim.
Scope: `@handle review [notes]` / `full review` ingress arm, comment-leading
(grandfathered, flagged B1); footer edit — code twin primary
(orchestrator.rs:94-97), tmpl:32-35 secondary, examples in code spans;
**`ReviewTriggerContext` extension** carrying prior fingerprint + notes into
both recipient texts (orchestrator.rs:71-113, :157-170 — rendering in
council.rs alone is a dead letter, §4); preset from `issue.labels` as
`/review` does (github_webhook.rs:202).
**Amends the Addendum's M2 stage map, deliberately:** the Addendum put all of
M2 at Stage 3 ("controller-owned, kernel unchanged"); P2 pulls only the
trigger grammar + two carried strings forward because both strings are
already plane-held at convene — no controller-shaped *state* enters the
kernel, no schema; the code is grandfathered under B1 and bound to the Stage
3 extraction exit (it moves out with the rest of the webhook grammar; never
a second grandfathered exception beyond it). The delta *policy* — findings
state, what round N is fed — stays at Stage 3 as ruled.
*Exit:* an author mention with fix notes convenes a round whose opening
prompt **as delivered to a reviewer (post-rewrite)** shows the notes and
"diff since `<sha>`"; the verdict carries F-numbers and marks Resolved
without re-raising; the chair's own footer comment triggers nothing; a
quote-reply of the footer triggers nothing; round-N spend recorded against
the 50% target.

**P3 — controller extraction + ledger (Stage 3; trigger: ADR 007's "another
real plugin" — fires with the forum north-client build or the nuphos Gen-1
migration).** Closes: M2 full, M3 full, M4.
Scope: `plugins/pr_review` (webhook parse, presets, prompts, `/v1/review`
exit the kernel; ingress via `controller.rs` only — B1); findings table +
machine findings block + `status`/`resolve Fn`/`help`; comment-by-ID
lifecycle as a **controller-owned GitHub write under the manifest's
`sideEffects`** (credential story + any ADR 008 extension named in the Stage
3 controller ADR — a P3 dependency; `emit_status` stays OCP-side, §5) incl.
edit-on-timeout/superseded; **durable close reason** (fires the §8 trigger);
`full review` vs delta divergence owned by the controller; ADR 013 columns
move out too.
*Exit:* a nuphos PR runs two rounds where the round-2 prompt contains the
open-findings set and no Resolved finding is re-raised (spot-check);
`resolve F2` then `status` reflect state across a plane restart; a chair
killed mid-round still yields a terminal "round failed" comment edit; a
superseded round renders as "Superseded".

## 8. Non-goals & deferred (named triggers)

| Item | Replaced by / owner | Build trigger |
|---|---|---|
| Session reuse across rounds | fresh session per round | **rejected permanently** — dissolves the round boundary (Gen-1 lesson B2) |
| LLM intent tier for commands | deterministic grammar + `/ask` | measured author confusion in dogfood |
| Chair posting via per-session scoped tokens (a real plane M3 fence) | pod-local App identity (ADR 004) + §5 steering + P3 comment-by-ID | reverses ADR 004 decision 2/4; trigger: verdict delivery becomes plane-held (B4) |
| `close_reason` column / `superseded` store state | ADR 012 webhook + north event `reason`; NULL decision alone is ambiguous (timeout/malformed-trailer are NULL too — §3) | **fires at Stage 3**: the P3 controller is the store-consumer; column vs startup re-emit decided in the Stage 3 ADR |
| Recency rule for out-of-order `synchronize` | declared residual, §3 (label + re-supersede bound it) | a dogfooded wrong-head verdict the `Reviewed at` label didn't surface |
| Plane-side GitHub reads (verify head for command rounds) | chair pins the SHA in the comment | a verified incident where the chair-pinned SHA proves insufficient; would break zero-GitHub-calls convene (council.rs:7-10) |
| Plane-side prevention of pod `gh` calls | closed gate/CAS + steering + P3 comment-by-ID | impossible under design.md:46; reclassify only if verdict delivery becomes plane-held (B4) |
| Reduced re-review panel | `review:lite` preset (council.rs:93-138) | measured round-N spend > 50% of round-1 on dogfood |
| Inline review-thread replies (`pull_request_review_comment`) | top-level `@handle why F3?` (bot self-fetches the thread) | M4 shipped + first dogfood request |
| `pause` / `ignore` / rate-limit query | hourly cap + round budget + repo allowlist | first noise/cost complaint the caps can't handle |
| Plane-posted "processing" comment / commit-status pending | chair opening turn (tmpl:22-28) | a dogfood council dies leaving no PR feedback → P3 controller write |
| `autofix` / `plan` / `generate *` write-back | — (different product) | explicit product decision post-parity |
| Learnings / preference memory | — | never plane; reconsider as controller state after M4 stabilizes |

## 9. Open questions (with recommendations)

**Q1 — PR-author command permission.** `can_command` admits only
OWNER/MEMBER/COLLABORATOR (github_webhook.rs:55-57); GitHub's full
`author_association` enum also surfaces PR authors as CONTRIBUTOR,
FIRST_TIME_CONTRIBUTOR, FIRST_TIMER, MANNEQUIN, or NONE — all excluded, which
kills the core loop on external-contributor repos; dogfood authors are
MEMBERs, so nothing is blocked today. *Recommendation:* defer; trigger =
first onboarded repo whose PR authors can't command. Then allow `review`/ask
when `comment.user.login == issue.user.login` (payload-only, no GitHub call,
and it covers the whole excluded set at once), keeping `full review` and
future valves association-gated.

**Q2 — repeat command with no intervening push.** With `cmd:<comment_id>`
fingerprints, a second `/review`/mention always supersedes and burns a round
even if the head hasn't moved. *Recommendation:* keep supersede — explicit
human intent, the round budget bounds it, and deduping on "head unchanged"
needs a GitHub call the plane must not make.

**Q3 — structured findings channel shape (Stage 3).** Extend the
plane-parsed `[[…]]` trailer family vs a fenced machine block parsed by the
controller. *Recommendation:* fenced block, controller-parsed — the trailer
family is kernel surface and must stay tiny; findings are app data
end-to-end (M4). Decide in the Stage 3 controller ADR, alongside the durable
close reason and the controller write-path credential story (§5, §7 P3).

**Q4 — template / code-twin duplication. Resolved → P0 pre-task.** The
chair's turn text lives in both `pr-review-trigger-pointer.tmpl:17-55` and
`review_recipient_text_from_context` (orchestrator.rs:87-113), and the two
already diverge: the webhook-path chair sees only the code twin, whose
verdict format differs and which carries no footer (§2). P0's steering edits
and P2's carried strings all flow through this rewrite, so collapsing to a
single source is a **requirement before those edits land**, not a
recommendation — silent steering drift is a proven Gen-1 failure mode
(lesson B5).