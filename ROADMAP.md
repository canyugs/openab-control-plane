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

## Phase 1 — Usable (now)

Goal: from one template deploy (plane **+ OAB pods**) to a code review that runs
end-to-end — start it, deliberate, reply through to the final verdict — in under
5 minutes, on your own keys. The whole loop, OAB setup included, not just "deploy
succeeds."

| Item | Status | Notes |
|------|--------|-------|
| **BYOK** — accept user-provided `CLAUDE_CODE_OAUTH_TOKEN` / `ANTHROPIC_API_KEY` | TODO | AI Hub keys are opt-in add-on, not default |
| **Shared steering via `pre_seed`** — review output format + rules delivered to bots as an object-store layer (`shared/default.tar.gz`), not stuffed in every trigger | TODO | OAB's job, not the plane's (see [design scope](docs/design.md)). Mechanism: OAB `[hooks.pre_seed]` layer concept — base shared layer + per-agent override. Use **Cloudflare R2 / any S3-compatible store via `endpoint_url`** (our users are on Zeabur/Cloudflare, not AWS). Ref: OAB `docs/hooks.md` (beta.3). Package `docs/steering/pr-review.md` into the shared archive |
| **Preset-driven roster** — quick=2, standard=3, full=5; idle bots don't join | TODO | Solves slowness: fewer bots for small PRs |
| **Application shim** — code-review logic (gh pr diff/comment/label) lives outside the plane | TODO | Shim options: GitHub Action, standalone service, or chair responsibility (current) |
| **Clean template** — TEMPLATE.md + one-click deploy, no manual bot registration | TODO | `seed_bot` on boot ✅; template needs polish |
| **GitHub App identity** — bot acts as itself, not the author (see [Agent Identity](#principle-agent-identity)) | TODO | Old 5 apps are gone; build one fresh. App perms: `pull_requests:write`, `contents:read` |
| **Per-role scoped tokens** — chair gets a write token, reviewers read-only | TODO | Enforces "only chair writes" + prevents duplicate comments (×3) |

## Phase 2 — Angles & Automation

Goal: preset-based angle assignment, auto-trigger, structured output.

| Item | Status | Notes |
|------|--------|-------|
| **Angle assignment in trigger** — `--preset quick\|standard\|full`, assignment table in trigger message | TODO | Phase 1 of angle-based presets |
| **Preset selection** — default standard; PR label `review:quick`/`review:full` overrides | TODO | Phase 2 of presets |
| **Auto-trigger** — webhook shim: PR opened / `/review` comment → open session | TODO | Removes manual session creation |
| **Decision→review-state** — chair approve/request-changes as source of truth + label | TODO | Depends on GitHub App identity |
| **Post-review actions** — chair posts action menu, compact summary (🔴×1 🟡×10 🟢×5) | TODO | Phase 3 of presets |
| **Commit status target_url** — Checks tab "Details" links to the review comment | TODO | Phase 4 of presets |

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
| **Liveness / timeouts** — heartbeat-driven stall detection, step timeouts | TODO | `last_seen` recorded but unused |
| **Targeted addressing / handoff** — first-class A→B direct send | TODO | Currently broadcast + @mention-gate |

## Phase 4 — Platform

Goal: multi-tenant, discoverable, extensible.

| Item | Status | Notes |
|------|--------|-------|
| **Bot discovery** — dynamic registration, capability advertisement, health-aware roster | TODO | Currently static `OABCP_BOTS` |
| **Hooks** — `on_session_open`, `on_quorum`, `on_verdict`, `on_bot_connect` | TODO | Plane-native (Rust trait) vs external (webhook) TBD |
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
