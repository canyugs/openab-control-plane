# openab-control-plane — Roadmap

Generic multi-agent coordination engine. Code review is the first app, not the product.

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
| **3. Scalable access control — dynamic Okta/Slack-group binding instead of static `allowedUsers`/`allowedChannels`** | **Not OCP.** The membership plane / admission gate governs which *bots* join a roster, not which *humans* may invoke a bot. Per-user gating lives in the OpenAB gateway (platform adapters, which OCP does not own — see [design.md](docs/design.md)) or a front auth-proxy | Out of scope — OpenAB-core / auth-proxy |

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
(BYOK, lite default for speed) → working council → verdict, with a droppable GitHub
Action for auto-review. The three Phase-1 scope goals (slow→agent-count, ≤5-min
onboarding, BYOK-primary) are met. Hardening for production (durable audit DB, etc.)
is tracked in issue #29; angles/automation continue in Phase 2.

| Item | Status | Notes |
|------|--------|-------|
| **Liveness watchdog — timeout → forced `Close`** | ✅ | **Completes OCP's reason to exist** (the *guarantee* layer — see [design: steering vs policy](docs/design.md)). `orchestrator::force_close_timeout` + a 30s background scan in `main.rs`; deadline via `OABCP_SESSION_TIMEOUT_SECS` (default 900s). Closes a stuck session with reviews-in-hand, naming absentees. CAS once-only. Richer per-step/heartbeat detection stays Phase 3 |
| **BYOK** — accept user-provided `CLAUDE_CODE_OAUTH_TOKEN` / `ANTHROPIC_API_KEY` | ✅ | The template's primary path: `CLAUDE_CODE_OAUTH_TOKEN` (from `claude setup-token`) is a deploy var passed to every pod; runs on the user's own Claude subscription. AI Hub keys stay an opt-in add-on — the AI Hub onboarding doc was deliberately skipped to keep BYOK the single primary story |
| **Shared steering via `pre_seed`** — review output format + rules delivered to bots as an object-store layer (`shared/default.tar.gz`), not stuffed in every trigger | **TODO (deferred)** | OAB's job, not the plane's (see [design scope](docs/design.md)). Mechanism: OAB `[hooks.pre_seed]` layer concept — base shared layer + per-agent override. Use **Cloudflare R2 / any S3-compatible store via `endpoint_url`** (our users are on Zeabur/Cloudflare, not AWS). Ref: OAB `docs/hooks.md`. Package `docs/steering/pr-review.md` into the shared archive. **Investigated 2026-06-27:** `pre_seed` confirmed built-in (default-on), R2-compatible via `endpoint_url`, layered + SHA-256-checksummed — it landed in OAB **0.9.0-beta.3** (the latest release; `git diff beta.2..beta.3` touched no gateway-wire / bot-config files, so the OCP plane's wire contract is unaffected). The standing council's bots were bumped **beta.2 → beta.3-claude**, so the capability is now live and validated. **Deferred** because trigger-based steering is already proven (dogfood ×2 + the `council-review.yml` auto-trigger) and the review format is already consistent *without* a pre_seed layer. Do it once a *second* trigger builder (the webhook path under *GitHub App identity*) would otherwise duplicate the steering text, or to iterate the house style centrally. **Storage backend (where the archive lives / who serves it) is an open decision — see [ADR 003](docs/adr/003-steering-delivery.md): external R2 vs an OCP-hosted S3 origin (the latter blocked on an OAB `force_path_style` change).** |
| **Preset-driven roster** — quick=2, standard=3, full=5; idle bots don't join | ✅ | Solves slowness. `council.rs` `preset_angles` (lite=1 / quick=3 / standard=5 / full=7 angles) + `assign_angles` round-robin trims idle reviewers (quorum = participants only). Default **lite** keeps small PRs fast/cheap; per-PR `review:<preset>` label or env `OABCP_COUNCIL_PRESET` override. Live-proven on canyugs/openab#14 |
| **Application shim** — code-review logic (gh pr diff/comment/label) lives outside the plane | ✅ | Three shims shipped, all outside the plane: (1) droppable **GitHub Action** `examples/pr-review.yml` → `POST /v1/review`; (2) the chair pod runs `gh pr diff/comment/label` from the trigger; (3) `scripts/open-council.sh` on demand. The plane only convenes + guarantees close — never calls GitHub itself |
| **Clean template** — TEMPLATE.md + one-click deploy, no manual bot registration | ✅ | `seed_bot` on boot + `npx zeabur template deploy` (3 vars, no manual bot registration) + [TEMPLATE.md](TEMPLATE.md) marketplace page with two install tracks (PAT+Action / GitHub App) |
| **GitHub App identity** — bot acts as itself, not the author (see [Agent Identity](#principle-agent-identity)) | ✅ | **Live + validated.** A fresh App (`zeabur-council[bot]`, perms `pull_requests:write` + `contents:read`) mints per-role installation tokens in the plane; the **chair posts the verdict as the App** via a chair-only `pre_boot` App-auth hook (v0.1.9, #19) on a persistent volume holding the key + minter. End-to-end L3 validation done (issue #9 closed); verdicts now appear as `zeabur-council[bot]`, not the author's PAT |
| **Per-role scoped tokens** — chair gets a write token, reviewers read-only | **OCP side ✅** (consumption → OAB) | OCP's whole job here is **mint + offer + revoke**, and it is done: `/v1/sessions/:id/github-token` mints per-(session,role) tokens (chair `pull_requests:write`, reviewer `pull_requests:read` — role from the bot record, not the caller) and **purges them from its store on session close** (never served again; OCP does not call GitHub's `DELETE /installation/token`, so a token already handed to a pod lapses at its short ~1h expiry). **Bot-side consumption is OAB's, not OCP's** (decided 2026-06-28): a bot fetching its scoped token, configuring `gh`, and dropping the static shared `GH_TOKEN` from its pod env are bot/pod concerns — the "credentials stay in each pod" boundary (see [design.md](docs/design.md)). Tracked on the OAB side. |

## Phase 2 — Angles & Automation

Goal: preset-based angle assignment, auto-trigger, structured output.

| Item | Status | Notes |
|------|--------|-------|
| **Angle assignment in trigger** — `--preset quick\|standard\|full`, assignment table in trigger message | ✅ | `open-council.sh --preset` (manual/PAT path) **and** the webhook path (`council.rs`, env `OABCP_COUNCIL_PRESET`) both round-robin angles onto reviewers (extras trimmed, quorum = participants); table rendered into the trigger; each bot covers its row. Live-proven on canyugs/openab#14 |
| **Self-fetch review (pointer trigger)** — bots fetch the PR themselves (`gh pr diff` / read files) instead of the diff being embedded in the broadcast trigger | ✅ Block B | `open-council.sh --self-fetch` sends a pointer trigger (`pr-review-trigger-pointer.tmpl`, no `{{DIFF}}`). Solves huge-diff trigger bloat **and** diff-only bias — the reviewer reads surrounding context. Live-proven on canyugs/openab#14 (reviewer cited the `lib.rs` re-export — info absent from the diff). Default stays diff-in-trigger (stock-template reviewers have no gh). **Write-safety (reviewer read-only, no PR writes) is an OAB concern, not OCP's** — OCP offers the read-only scoped token (above); the bot consuming it and not holding a write PAT is the pod's job. Webhook path (`council.rs`) still plane-fetches + embeds; switching it to a pointer trigger waits on the OAB-side token consumption |
| **Preset selection** — default standard; PR label `review:quick`/`review:full` overrides | ✅ | A per-PR `review:<preset>` label (`lite`/`quick`/`standard`/`full`) overrides the `OABCP_COUNCIL_PRESET` default on the webhook convene path (v0.1.10, #23). `--preset` flag also on the CLI path |
| **Auto-trigger** — webhook shim: PR opened / `/review` comment → open session | ✅ | The GitHub App webhook (`POST /api/v1/github_webhooks`) convenes a real council on PR `opened`/`reopened`/`ready_for_review` or a `/review` comment (v0.1.6, #14). The `council-review.yml` Action is now manual-only (`workflow_dispatch`). Pre-prod gaps: no per-repo allowlist / `/review` perm gate |
| **Decision→review-state** — chair approve/request-changes as source of truth + label | TODO | Depends on GitHub App identity |
| **Post-review actions** — chair posts action menu, compact summary (🔴×1 🟡×10 🟢×5) | TODO | Phase 3 of presets |
| **Commit status target_url** — Checks tab "Details" links to the review comment | TODO | Phase 4 of presets |
| **Conversational follow-up** — `@mention` / `/ask` the bot on a PR → it answers in the thread | **✅ v1** (#30) | The CodeRabbit `@coderabbitai …` gap. A `/ask` or `@OABCP_BOT_HANDLE` comment from a write-ish user (`author_association`) convenes a **solo** self-fetch session that answers as a new PR comment; comment-id idempotency; opt-in `OABCP_ALLOWED_REPOS`. Plane stays out of GitHub ([ADR 006](docs/adr/006-conversational-followup.md)). Deferred: inline review-thread replies (`pull_request_review_comment`), `/resolve`, single-session streaming |

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
| **Liveness / timeouts** — heartbeat-driven stall detection, per-step timeouts | TODO | Richer detection beyond the Phase 1 session-close watchdog (which guarantees termination). `last_seen` recorded but unused |
| **Targeted addressing / handoff** — first-class A→B direct send | TODO | Currently broadcast + @mention-gate |

## Phase 4 — Platform

Goal: multi-tenant, discoverable, extensible. Most items here build out the
**membership plane** (who exists / who's alive / who may join) — the weakest of
the three planes today; see [ADR 001](docs/adr/001-three-planes.md).

| Item | Status | Notes |
|------|--------|-------|
| **Bot discovery (membership plane)** — dynamic registration, capability advertisement, health-aware roster; join/leave as first-class events | TODO | Moves membership from boot-time-static `OABCP_BOTS` to a dynamic registry. `add_to_roster` + backfill is the start |
| **Admission gate** — every roster add passes one chokepoint guaranteeing a bounded, valid roster (registered bot + `OABCP_MAX_ROSTER` quota) | ✅ | `orchestrator::admit` (pure) + `add_to_roster` → `Admission`; `POST .../roster` returns `409` on reject. Fixed a real bug (could add an unregistered bot that hangs the roster). The gate self-recruit will ride |
| **Self-recruitment + per-bot authz** — a bot adds a member via `[[recruit:<id>]]` in a message; the plane decides admission, never honors it just because a bot asked | ✅ | `orchestrator::maybe_recruit` → the admission gate; authz = `may_recruit` (chair-only v1). No new wire command (text convention like `[[reply_to:]]`). A reviewer's recruit is denied. Widen authz (role/allow-list) in one place when needed |
| **Fleet provisioner** — recruiting a bot type that has no pod yet spins one up (Zeabur) | seam ✅ / impl external | OCP side done: a recruit of an unregistered bot emits `provision_requested` (`orchestrator::recruit_event`) instead of failing. The actual pod-spinning is an **external** provisioner holding the Zeabur token — separate trust domain, off the hot path. Contract: [docs/provisioner.md](docs/provisioner.md). See ADR 001 |
| **Hooks** — `on_session_open`, `on_quorum`, `on_verdict`, `on_bot_connect` | TODO | Plane-native (Rust trait) vs external (webhook) TBD |
| **OAB Father / external controllers** — installable control-layer plugins declare events/actions/scopes, then drive sessions through signed events and declarative actions | proposed | ADR 007 defines the boundary: OCP core keeps runtime guarantees; OAB Father owns bot/controller management; PR review becomes the first bundled controller instead of a special path |
| **Multi-tenant auth** — per-org API keys, OAuth/OIDC for humans | TODO | Currently single bearer key |
| **HA / scale** — Postgres/libSQL store, multi-process | TODO | Store trait seam exists, untested |
| **Session-scoped credentials** — plane mints `session × role` tokens at open, expires at close | TODO | Completes [Agent Identity](#principle-agent-identity); current tokens are static |
| **Audit log** — every side-effect (verdict, comment, label) tagged to the bot identity that did it | TODO | Plane is the identity registry → single place to audit |
| **Central revoke** — kill a bot/session token once → access ends everywhere | TODO | Pairs with audit; no per-system cleanup |
| **/bot-config token leak** — move `token_plain` to env/pre_seed | TODO | Spike convenience, not production-safe |

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

- **At-least-once delivery** — ack = handed-to-channel, not socket-confirmed; OAB has no `event_id` dedup → rare reprocessing on redelivery.
- **Backfill is active** — OAB responds to in-thread history (no silent-context-load mode).
- **Trailing `…` stub** — status stub can momentarily appear mid-stream (fills via edits).
- **Large diffs untested** — demo PR was tiny; deep repo context (clone/`pr checkout`) untested.
- **Broad PAT in 5 pods** — v1 reuses personal PAT; rotate after demos, move to per-bot App tokens.

## Live Infra

Zeabur project `openab-hub` = `6a3abba9e41f9f1d193022cb`

| Service | ID | URL |
|---------|-----|-----|
| plane | `6a3ca6cde5f256c9f3d43e01` | `https://openab-control-plane.zeabur.app` (internal `:8080`, volume `/data`) |
| chair (gandalf-red) | `6a3cb4d3e5f256c9f3d440bb` | bot_3d9d… |
| rev1 | `6a3cf6a4bdba1c7a91f8c1a3` | — |
| rev2 | `6a3cf6a6bdba1c7a91f8c1a6` | — |
| rev3 | `6a3cfcb5bdba1c7a91f8c461` | — |
| rev4 | `6a3cfcbdbdba1c7a91f8c466` | — |

5 OAB pods still running — stop when not demoing.

Redeploy: `npx zeabur@latest deploy --project-id 6a3abba9e41f9f1d193022cb --service-id 6a3ca6cde5f256c9f3d43e01`
