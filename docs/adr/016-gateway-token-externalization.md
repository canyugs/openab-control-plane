# ADR 016 — Gateway token externalization

Status: proposed · 2026-07-03

## Context

`/bot-config/:id` renders a stock OpenAB pod its `config.toml`, including the
bot's **gateway token** (`oabct_…`, the bot↔plane WebSocket credential) as a
plaintext `[gateway] token = "…"`. The endpoint has **no client auth** — the
comment at `api.rs` says "the token IS the bot's credential" — and it is served
on the internal network, fetched by OpenAB at pod boot (`openab run -c
<url>/bot-config/<id>`).

The problem (ADR 010 negative consequence "`/bot-config` can serve
`token_plain`"; `AGENTS.md`: "not production-safe"): **anything on the internal
network can retrieve any bot's long-lived gateway token by guessing its id**
(`chair`, `rev1`, `rev2` are trivial). This is a lateral-movement gap — one
compromised pod reads every bot's credential.

### What the fix is constrained by (OpenAB capabilities, source-verified 2026-07-03)

- **Config-fetch auth: none.** `openab run -c <https-url>` fetches config with a
  bare `reqwest` GET — no Authorization header, no env-sourced auth
  (`openab-core/src/config.rs`). So we **cannot** protect the endpoint with a
  bootstrap key: the only client physically can't present one.
- **`${VAR}` env expansion: yes.** OpenAB expands `${VAR}` over the entire raw
  config text (via `std::env::var`) before parsing, for any value including
  `[gateway] token` (`config.rs` `expand_env_vars`).
- **`[gateway] token`** is consumed literally as the WS `token=` query param
  (`gateway.rs`), after the text-level `${VAR}` expansion.

**Consequence:** because the endpoint cannot be authenticated, the only way to
close the retrieval hole is to **stop serving the secret**. And not serving it
requires the pod to obtain its token elsewhere — its own env, via `${VAR}`.
There is no smaller fix; externalization *is* the minimal fix.

## Decision

Deliver the gateway token to the pod through its **environment**, not the
config body. `/bot-config` renders an env reference; the operator provisions the
token as an ordinary deploy secret — the same posture any generic deployment
(docker-compose, k8s, Helm, bare metal) already uses for a shared DB password.
**No Zeabur-specific secret-generation is assumed.**

### Token flow

1. **Operator provisions** one gateway token per bot (any random string) as a
   deploy secret, and places the same value in two env vars:
   - the plane's env: `OABCP_BOT_TOKEN_<NAME>` (e.g. `OABCP_BOT_TOKEN_CHAIR`),
   - the bot pod's env: `OABCP_BOT_TOKEN`.
2. **Plane seeds from env.** `identity::seed` (driven by `OABCP_BOTS`) looks up
   `OABCP_BOT_TOKEN_<NAME>` for each bot. If present, it seeds the bot with
   `hash(that token)` and stores **no plaintext**. If absent, it falls back to
   today's behavior (generate a random token, store plaintext) — see Migration.
3. **`/bot-config` renders the reference.** When externalized, it emits
   `token = "${OABCP_BOT_TOKEN}"` verbatim. OpenAB expands it from the pod's own
   env at boot. The response body carries **no secret**, so an unauthenticated
   fetch returns nothing sensitive.
4. **WS auth is unchanged.** The pod connects with the expanded token; the plane
   validates by hash (`bot_by_token_hash`) exactly as today.

### The switch

A single env flag `OABCP_EXTERNALIZE_TOKENS=1` turns on both behaviors together
(seed-from-env + render-the-reference). Off (default) keeps the legacy plaintext
path bit-for-bit, so existing deployments are untouched until they opt in. A
per-bot switch was rejected as over-engineering: a deployment is either wired
for externalization or it isn't.

`OABCP_BOT_TOKEN` is a **fixed** name (not per-bot) in each pod's env — a pod
only ever carries its own token, so one name is enough and keeps the rendered
config identical for every bot.

## Scope Rules

This is deliberately the minimum that closes the hole:

- **Do** externalize the gateway token via env reference behind a flag.
- **Do** keep the legacy plaintext path as the default and as fallback.
- **Do not** add endpoint auth (OpenAB can't use it), token rotation, an admin
  token-fetch API, or `exec://`/`aws-sm://` secret-resolver wiring — all are
  either impossible here or heavier than the problem.
- **Do not** move the token's source of truth: the operator owns it, the plane
  learns only the hash. This matches ADR 010's "explicit registration flow that
  never requires OCP to render the whole config."

## Consequences

### Positive

- The unauthenticated `/bot-config` retrieval stops leaking a live credential:
  the body is `token = "${OABCP_BOT_TOKEN}"`, useless without the pod's env.
- Generic across orchestrators; no Zeabur dependency.
- Aligns the token with how `CLAUDE_CODE_OAUTH_TOKEN` / `GH_TOKEN` are already
  provisioned in the templates (deploy-time env), so the deploy story converges.

### Negative

- Gives up the `/bot-config` convenience that "no human copies the token"
  (`identity.rs`): the operator now provisions a per-bot secret in their
  manifest. This is the standard posture for any shared credential and is the
  right trade for a generic, production-safe deploy.
- The same token value must be set in two places (plane seed var + pod var). A
  mismatch fails the WS handshake loudly (bad-token), not silently.
- `token_plain` still exists in the schema for legacy-mode bots. Encrypting or
  dropping the column is out of scope (tracked separately as the DB-at-rest gap).

### Neutral

- `OABCP_BOTS` remains the roster source; only the token source moves.
- Discovery-registered and API-registered bots are unaffected (they already
  receive their token in the registration response, not via `/bot-config`).

## Migration Direction

1. Ship the flag **off by default** — no behavior change for current deploys.
2. Update both Zeabur templates and the local dogfood deploy to set
   `OABCP_EXTERNALIZE_TOKENS=1`, the per-bot `OABCP_BOT_TOKEN_<NAME>` on the
   plane, and `OABCP_BOT_TOKEN` on each pod.
3. Once the templates are externalized, the plaintext path is legacy-only;
   consider flipping the default and removing `token_plain` in a later ADR.

## References

- [ADR 010 — OpenAB configUrl boundary](010-openab-configurl-boundary.md)
  (migration step 3: "Design gateway token externalization")
- [ADR 001 — Three planes](001-three-planes.md)
- OpenAB `openab-core/src/config.rs` (`expand_env_vars`, unauthenticated fetch),
  `gateway.rs` (literal `token=` use)
