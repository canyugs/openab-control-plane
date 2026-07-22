# openab-control-plane — Roadmap

Generic multi-agent coordination engine. Code review is the first app, not the product.

> Ordering lives in [route.md](route.md) (2026-07-08): Phase 1 = run
> CodeRabbit's loop smoothly, no new surface; Phase 2 = depth. This file stays
> the item-status source of truth.

> **Architecture update (2026-07-22):** [ADR 031](adr/031-provider-neutral-kernel.md)
> activates the external-controller boundary and makes the bundled GitHub path
> compatibility-only. GitHub credential, webhook, and findings items below
> describe the current migration baseline, not the target kernel. The executable
> sequence and removal gates live in the
> [provider-neutral kernel migration plan](provider-neutral-kernel-migration.md).

## Principle: Agent Identity

A council is always multi-party — there is no single user whose permissions a bot
can borrow. So **each bot acts as itself**, and the plane mints session-scoped
external credentials per role: chair gets a *write* token, reviewers get *read-only*.
Tokens are scoped to `session × role` and expire when the session closes. The plane
is the identity registry, so it owns central audit (every side-effect tagged to the
bot that did it) and central revoke (kill once → access ends everywhere).

This unifies the identity items below: GitHub App identity, write enforcement, and
token rotation are three faces of the same model.

**act-as-self vs act-as-user is a north-identity question, not a bot-count one.**
A 1-bot session is only a "DM" if north carries an *authenticated single user* the
plane can delegate down — then the bot acts as that user on their connectors. With
today's single shared `OABCP_API_KEY` there is no user to borrow, so every session
is act-as-self regardless of roster size. The act-as-user mode is gated on the
Phase 4 per-user OAuth/OIDC item, not a separate feature.

### Note: enterprise BaaS asks (act-as-user track)

A team piloting OpenAB as a centralized internal BaaS (single-user task bots —
Confluence Q&A, Slack alerting) raised three governance blockers from a security
review (2026-06-27). They map onto OCP as follows — the plane is the right home for
two of them, the third belongs to another layer:

| Ask | OCP home | Status |
|-----|----------|--------|
| **1. Identity delegation (act-on-behalf-of)** — bot acts as the current Slack user (Okta/OIDC), not a shared service token, so users only see what their own perms allow | **Yes — the plane is the natural home.** This is exactly the act-as-user mode above: north carries an authenticated single user → plane mints a per-connector scoped token → bot acts as that user. Phase 4 *Multi-tenant auth (OAuth/OIDC for humans)* + *Session-scoped credentials* | TODO — deferred act-as-user half; today act-as-self via shared `OABCP_API_KEY` |
| **2. End-to-end audit (trace downstream op → triggering user)** | **Partial.** The plane is already named the central audit point (Phase 4 *Audit log* + *Central revoke*) — but it tags side-effects to the **bot identity**, not the **human**. The user-link only exists once ask 1 lands; the two are coupled | TODO — central choke point planned; user attribution depends on ask 1 |
| **3. Scalable access control — dynamic Okta/Slack-group binding instead of static `allowedUsers`/`allowedChannels`** | **Not OCP.** The membership plane / admission gate governs which *bots* join a roster, not which *humans* may invoke a bot. Per-user gating lives in the OpenAB gateway (platform adapters, which OCP does not own — see [design.md](design.md)) or a front auth-proxy | Out of scope — OpenAB-core / auth-proxy |

**The line:** the actionable-in-OCP work is asks 1+2, and it is one piece of work —
pull the Phase 4 identity quartet (multi-tenant OIDC / session-scoped creds / audit /
central revoke) forward *and* build the act-as-user north-identity path this section
deliberately drew tight. Ask 3 stays out of the plane. None of this is built; the
pilot's use case is precisely the act-as-user mode OCP deferred.

## Phase 1 — Usable (now)

Goal: from one template deploy (plane **+ OAB pods**) to a code review that runs
end-to-end — start it, deliberate, reply through to the final verdict — in under
5 minutes, on your own keys. The whole loop, OAB setup included, not just "deploy
succeeds."

**Status: complete (2026-06-29).** All Phase 1 items ✅ — one-command template deploy
(BYOK, lite default for speed) → working council → verdict, with webhook auto-review
for dogfood and a copied GitHub Action option for external repos. The three Phase-1
scope goals (slow→agent-count, ≤5-min onboarding, BYOK-primary) are met. Hardening for
production (durable audit DB, etc.) is tracked in issue #29; angles/automation continue
in Phase 2.

| Item | Status | Notes |
|------|--------|-------|
| **Liveness watchdog — timeout → forced `Close`** | ✅ | **Completes OCP's reason to exist** (the *guarantee* layer — see [design: steering vs policy](design.md)). `orchestrator::force_close_timeout` + a 30s background scan in `main.rs`; deadline via `OABCP_SESSION_TIMEOUT_SECS` (default 600s / 10 min). Closes a stuck session with a `TIMEOUT` verdict, emits a structured north `timeout` event for app shims/controllers, and names absentees. CAS once-only. Richer per-step/heartbeat detection stays Phase 3 |
| **BYOK** — accept user-provided `CLAUDE_CODE_OAUTH_TOKEN` / `ANTHROPIC_API_KEY` | ✅ | The template's primary path: `CLAUDE_CODE_OAUTH_TOKEN` (from `claude setup-token`) is a deploy var passed to every pod; runs on the user's own Claude subscription. AI Hub keys stay an opt-in add-on — the AI Hub onboarding doc was deliberately skipped to keep BYOK the single primary story |
| **Shared steering via `pre_seed`** — review output format + rules delivered to bots as a bot/OpenAB property, not stuffed in every trigger | **✅ v1** (PR #149, 0.1.15) | Landed on the deployment-owned path (not the plane): both Zeabur templates mount `docs/steering/pr-review.md` into every bot pod (`/home/node/AGENTS.md` for Claude images; `/home/agent/.kiro/steering/` for Kiro variants). The boundary held — the plane never serves steering from `/bot-config` ([ADR 010](adr/010-openab-configurl-boundary.md)); `tests/steering_sync.rs` makes template↔doc drift a CI failure. The concrete driver was hot steering iteration: 0.1.14's binary shipped without the #144/#147/#148 template text. OAB-native `[hooks.pre_seed]` remains an alternative delivery, no longer blocking. |
| **Preset-driven roster** — quick=2, standard=3, full=5; idle bots don't join | ✅ | Solves slowness. `council.rs` `preset_angles` (lite=1 / quick=3 / standard=5 / full=7 angles) + `assign_angles` round-robin trims idle reviewers (quorum = participants only). Default **lite** keeps small PRs fast/cheap; per-PR `review:<preset>` label or env `OABCP_COUNCIL_PRESET` override. Live-proven on canyugs/openab#14 |
| **Application shim** — code-review logic (gh pr diff/comment/label) lives outside the plane | ✅ | Three shims shipped, all outside the plane: (1) copied **GitHub Action** `examples/pr-review.yml` → `POST /v1/review`; (2) the chair pod runs `gh pr diff/comment/label` from the trigger; (3) `scripts/open-council.sh` on demand. The plane only convenes + guarantees close — never calls GitHub itself |
| **Clean template** — one-click deploy, no manual bot registration | ✅ | `seed_bot` on boot + `npx zeabur template deploy` (3 vars, no manual bot registration) + dedicated install docs for the [PAT copied Action](install-pat.md) and [GitHub App webhook](install-github-app.md) tracks |
| **GitHub App identity** — bot acts as itself, not the author (see [Agent Identity](#principle-agent-identity)) | ✅ | **Live + validated.** A fresh App (`zeabur-council[bot]`, perms `pull_requests:write` + `contents:read`) mints per-role installation tokens in the plane; the **chair posts the verdict as the App** via a chair-only `pre_boot` App-auth hook (v0.1.9, #19) on a persistent volume holding the key + minter. End-to-end L3 validation done (issue #9 closed); verdicts now appear as `zeabur-council[bot]`, not the author's PAT |
| **Per-role scoped tokens** — chair gets a write token, reviewers read-only | **OCP side ✅** (consumption → OAB) | OCP's whole job here is **mint + offer + revoke**, and it is done: `/v1/sessions/:id/github-token` mints per-(session,role) tokens (chair `pull_requests:write`, reviewer `pull_requests:read` — role from the bot record, not the caller), purges them from its store on session close (guaranteed: never served again), and in App mode fires a best-effort async GitHub-side revoke via `DELETE /installation/token` (warn-only, no retry; ≤1h TTL remains the backstop). **Bot-side consumption is OAB's, not OCP's** (decided 2026-06-28): a bot fetching its scoped token, configuring `gh`, and dropping the static shared `GH_TOKEN` from its pod env are bot/pod concerns — the "credentials stay in each pod" boundary (see [design.md](design.md)). Tracked on the OAB side. |

## Phase 2 — Angles & Automation

Goal: preset-based angle assignment, auto-trigger, structured output.

| Item | Status | Notes |
|------|--------|-------|
| **Angle assignment in trigger** — `--preset quick\|standard\|full`, assignment table in trigger message | ✅ | `open-council.sh --preset` (manual/PAT path) **and** the webhook path (`council.rs`, env `OABCP_COUNCIL_PRESET`) both round-robin angles onto reviewers (extras trimmed, quorum = participants); table rendered into the trigger; each bot covers its row. Live-proven on canyugs/openab#14 |
| **Self-fetch review (pointer trigger)** — bots fetch the PR themselves (`gh pr diff` / read files) instead of the diff being embedded in the broadcast trigger | ✅ | The webhook path (`council.rs`) and `open-council.sh --self-fetch` both use `pr-review-trigger-pointer.tmpl` (no `{{DIFF}}`). This avoids large trigger payloads and diff-only bias. Manual `open-council.sh` still supports the older inline-diff trigger unless `--self-fetch` is passed, for installs whose reviewers have no GitHub read credential. **Write-safety is an OAB/pod concern, not OCP's**: OCP can offer scoped tokens, but the pod must consume read-only auth and avoid static write PATs |
| **Preset selection** — default lite; PR label `review:quick`/`review:full` overrides | ✅ | A per-PR `review:<preset>` label (`lite`/`quick`/`standard`/`full`) overrides the `OABCP_COUNCIL_PRESET` default on the webhook convene path (v0.1.10, #23). `--preset` flag also on the CLI path |
| **Auto-trigger** — webhook shim: PR opened / `/review` comment → open session | ✅ | The GitHub App webhook (`POST /api/v1/github_webhooks`) convenes a real council on PR `opened`/`reopened`/`ready_for_review` or a write-ish user's `/review` comment (v0.1.6, #14). The dogfood repo no longer carries a repo-local `council-review.yml`; the copied Action remains only as `examples/pr-review.yml` for external/PAT installs, and manual reruns use `scripts/open-council.sh`. `OABCP_ALLOWED_REPOS` and comment-command permission gates are in place; deeper CODEOWNERS/team policy stays enterprise hardening (#29) |
| **Decision→review-state** — chair approve/request-changes as source of truth + label | **✅ v1** (#63/#64/#65) | Chair submits a real `gh pr review` + `[[verdict:decision r= y= g=]]` trailer; plane stores `decision`/`findings_*` and exposes them on the API, north `verdict` event, and close webhook ([ADR 013](adr/013-decision-review-state.md)). Labels deferred |
| **Post-review actions** — chair posts action menu, compact summary (🔴×1 🟡×10 🟢×5) | **✅ v1** (#66) | Final PR comment ends with a summary + action footer: counts, `/ask` follow-up, push-to-re-run. Chair-side prompt change (tmpl + steering + skill) |
| **Commit status target_url** — Checks tab "Details" links to the review comment | **✅ v1** (#66) | Chair sets `openab/council` commit status on the head SHA (`success`/`failure`, description = counts, `target_url` = review comment). Needs `Commit statuses: Read and write` (install docs updated) |
| **Conversational follow-up** — `@mention` / `/ask` the bot on a PR → it answers in the thread | **✅ v1** (#30) | The CodeRabbit `@coderabbitai …` gap. A `/ask` or `@OABCP_BOT_HANDLE` comment from a write-ish user (`author_association`) convenes a **solo** self-fetch session that answers as a new PR comment; comment-id idempotency; opt-in `OABCP_ALLOWED_REPOS`. Plane stays out of GitHub ([ADR 011](adr/011-conversational-followup.md)). Deferred: inline review-thread replies (`pull_request_review_comment`), `/resolve`, single-session streaming |

### Angle definitions

```
quick (3):  correctness, security, integration
standard (5): correctness, architecture, security, testing, docs
full (7):  correctness, architecture, security, testing, docs, performance, spec
```

Assignment rules: roster > angles → extras sit out; roster < angles → one bot covers multiple; quorum = all assigned angles reported.

## Phase 3 — Coordination Primitives

Goal: richer multi-agent patterns beyond broadcast+quorum.

| Item | Status | Notes |
|------|--------|-------|
| **Shared blackboard** — KV/task state agents read+write (claim tasks, partial results) | TODO | Design first: KV vs task-list+claim |
| **Liveness / timeouts** — heartbeat-driven stall detection, per-step timeouts | **✅ v1** (#68) | Liveness policy sweep: a roster member disconnected past `OABCP_LIVENESS_GRACE_SECS` (default 60s) flips to `unreachable`, is replaced from the inventory (same-role connected spare), or — reviewer with no spare — trimmed with the quorum shrunk so the session converges on the survivors (chair is replace-only). Reconnect flips health back. WS ping/pong (`OABCP_WS_PING_SECS`, default 20s, 0 disables) now routes connected-but-silent zombies into the same disconnect path. Live-verified: reviewer pod killed mid-council on #68 → closed with quorum in 2m53s, no watchdog. Deferred: per-step timeouts |
| **Targeted addressing / handoff** — first-class A→B direct send | TODO | Currently broadcast + @mention-gate |

## Phase 4 — Platform

Goal: multi-tenant, discoverable, extensible. Most items here build out the
**membership plane** (who exists / who's alive / who may join) — the weakest of
the three planes today; see [ADR 001](adr/001-three-planes.md).

| Item | Status | Notes |
|------|--------|-------|
| **Bot discovery (membership plane)** — dynamic registration, capability advertisement, health-aware roster; join/leave as first-class events | ✅ core / TODO automation | Moves membership from boot-time-static `OABCP_BOTS` to a dynamic registry. Inventory APIs exist (`GET /v1/bots`, `POST /v1/bots/discover`, `PATCH /v1/bots/:id`) and runtime replacement exists (`POST /v1/sessions/:id/roster/replace`, `POST /v1/council/roster/replace`). Remaining work is policy automation: choosing replacements from inventory and provisioning missing pods outside OCP. |
| **Admission gate** — every roster add/replace passes one chokepoint guaranteeing a bounded, valid roster (registered bot + `OABCP_MAX_ROSTER` quota for adds) | ✅ | `orchestrator::admit` (pure) + `add_to_roster` → `Admission`; `replace_roster_bot` preserves roster size, requires a registered replacement, updates chair identity only for chair-capable bots, purges stale outbox, and backfills the new bot. Fixed a real bug (could add an unregistered bot that hangs the roster). |
| **Self-recruitment + per-bot authz** — a bot adds a member via `[[recruit:<id>]]` in a message; the plane decides admission, never honors it just because a bot asked | ✅ | `orchestrator::maybe_recruit` → the admission gate; authz = `may_recruit` (chair-only v1). No new wire command (text convention like `[[reply_to:]]`). A reviewer's recruit is denied. Widen authz (role/allow-list) in one place when needed |
| **Fleet provisioner** — recruiting a bot type that has no pod yet spins one up (Zeabur) | seam ✅ / impl external | OCP side done: a recruit of an unregistered bot emits `provision_requested` (`orchestrator::recruit_event`) instead of failing. The actual pod-spinning is an **external** provisioner holding the Zeabur token — separate trust domain, off the hot path. Contract: [provisioner.md](provisioner.md). See ADR 001 |
| **Control plugins + OAB Father** — package use cases as installable control plugins and manage them through an operator surface | **✅ in-process extraction** / external extraction planned / OAB Father TODO | [ADR 007](adr/007-control-plugins-and-oab-father.md) executed the Stage 3 in-process extraction ([ADR 018](adr/018-stage3-extraction.md), S1–S17): PR review + triage live in `src/plugins/` behind the `Coordinator` hooks + `controller::execute`; the S16 second-consumer test proves the seam on trait defaults with zero `plugins::` imports. [ADR 031](adr/031-provider-neutral-kernel.md) now fires the independent-deployment trigger and amends [ADR 008](adr/008-external-controller-protocol.md): generic action/runtime-event transport and the external GitHub controller are P2–P8 of the [migration plan](provider-neutral-kernel-migration.md). OAB Father is not started. |
| **Hooks** — `on_session_open`, `on_quorum`, `on_verdict`, `on_bot_connect` | TODO | Plane-native (Rust trait) vs external (webhook) TBD |
| **Multi-tenant auth** — per-org API keys, OAuth/OIDC for humans | TODO | Currently single bearer key |
| **HA / scale** — Postgres/libSQL store, multi-process | TODO | Store trait seam exists, untested |
| **Session-scoped credentials** — plane mints `session × role` tokens at open, expires at close | TODO | Completes [Agent Identity](#principle-agent-identity); current tokens are static |
| **Audit log / message observability** — platform-independent, queryable record of messages + lifecycle/decision events, every side-effect tagged to the bot identity that did it | proposed | [ADR 017](adr/017-message-observability-audit-layer.md): message store + `session-log` already cover inbound/outbound content token-free; the gap is `emit_north` (`state.rs:219`) broadcasting lifecycle/decision events without persisting them. Fix: persist into a new `events` table + a `GET /v1/events` query API, plus a Discord/OAB ingest adapter symmetric to `github_webhook`. tool-call trace held coarse (cross-pod). Plane is the identity registry → single place to audit |
| **Central revoke** — kill a bot/session token once → access ends everywhere | TODO | Pairs with audit; no per-system cleanup |
| **/bot-config token leak** — externalize gateway tokens and demote `/bot-config` to bootstrap | **✅ v1** (#76–#80, [ADR 016](adr/016-gateway-token-externalization.md)) | The blocker fell: with `OABCP_EXTERNALIZE_TOKENS=1` the plane serves `token = "${OABCP_BOT_TOKEN}"` (env-resolved inside the pod) and stores only token hashes, so an unauthenticated `/bot-config` fetch leaks nothing; one-time token revoke shipped alongside (#76). Both templates feed per-bot `BOT_TOKEN_*` deploy vars. Default-flip and column-drop deliberately deferred. |

## Future

### Multi-Agent Panel Framework

The review panel pattern generalizes: N agents research from different angles → one synthesizes.

- **Coding Panel** — angles: codebase, spec, risk → implementor writes code with full context
- **Research Panel** — angles: official-docs, community, codebase-fit, alternatives → structured report
- **Q&A Panel** — same as Research but shallow+fast → concise answer + sources

### Evaluation / Benchmark

Prove the council is actually good. Single demo proves nothing.

- **Kodus CodeReviewBench** — 58/75 cases available via `samples.json`, offline scoring possible. Effort: ~2-3 days to wire council adapter + deterministic scoring.
- **Synthetic expansion** — LLM-injected bugs into real merged PRs (à la Qodo). Controllable ground truth.
- **The thesis** — single agent vs council: does quorum raise precision without dropping catch-rate?
- Prior art: SWE-PRBench, CR-Bench, Martian Code Review Bench, arXiv 2602.13377.

## Done (2026-06-25)

Coordination substrate — south `/ws` gateway, north REST+SSE, SQLite store, per-bot token identity. Council engine: fanout, quorum via 🆗, one-thread-per-session, chair synthesizes verdict. Live 1/3/5-bot proven.

- Streaming content (edit target from `reply_to`)
- Thread recording (`channel_type=supergroup` → OAB opens a topic)
- Verdict SSE timing (close+verdict on chair done-signal)
- Post-close chatter (gate delivery + drop new sends after close)
- Session isolation (roster authorization, two-way)
- Durable delivery (per-bot outbox, offline replay on reconnect)
- Dynamic join + history backfill (`POST /v1/sessions/:id/roster`)
- Agent `gh` access (`GH_TOKEN` in `inherit_env`, confirmed)
- In-progress + verdict comment + label (live on `council-demo#1`)
- Persistence (`/data` volume, generation-tagged conns)

## Known Issues

- **At-least-once delivery** — plane side hardened in Phase 1 (#116 `delivered_at` ack marker, #82 idempotent enqueue via `idem_key`), but OAB still has no `event_id` dedup → rare reprocessing on redelivery until the Stage 2 typed-wire ADR lands upstream.
- **Backfill is active** — now audience-aware and capped (Phase 1), but OAB still responds to in-thread history (no silent-context-load mode; needs the Stage 2 `context` flag).
- **Trailing `…` stub** — status stub can momentarily appear mid-stream (fills via edits).
- **Large diffs untested** — demo PR was tiny; deep repo context (clone/`pr checkout`) untested.
- **Static PAT fallback** — quickstart can still use a chair PAT; production should prefer pod-local App auth or scoped session credentials.

## Live Infra (2026-07-08)

Two council lanes, one GitHub App each (a plane binds one App identity: one
webhook secret, one `OABCP_BOT_HANDLE`, chair binds one installation).

| Lane | GitHub App (owner) | Plane URL | Zeabur project | Covers |
|------|--------------------|-----------|----------------|--------|
| dev | `zeabur-council` (canyugs) | `https://openab-council.zeabur.app` | `openab-council` | canyugs openab repos (selected) |
| prod | `opencodezebra` (zeabur org) | `https://opencodezebra.zeabur.app` | `opencodezebra-council` | zeabur org (all repos) |

Both run `canyu/openab-control-plane:0.1.18` + 3 Kiro OAB pods (chair, rev1,
rev2) on the Code Review dedicated server; chairs post as their App bot via
pod-local keys (`scripts/setup-github-app.sh`). A third local dogfood cluster
(`oabcp-local`, docker-desktop k8s) is used for pre-release verification.
The earlier `openab-hub` demo project is deleted.
