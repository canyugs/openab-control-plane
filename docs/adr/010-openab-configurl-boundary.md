# ADR 010 — OpenAB configUrl boundary

Status: accepted · 2026-07-01

## Context

OCP currently serves `/bot-config/:id`, which assembles an OpenAB `config.toml`
for stock OpenAB pods. That was useful for early dogfood and Zeabur templates:
the plane could seed bot identities, generate per-bot gateway tokens, and let a
pod start with one stable URL.

OpenAB is now moving its deployment model toward `configUrl` / `configFile` as
the primary configuration path. In that model, the bot process starts with:

```sh
openab run -c s3://bucket/path/to/config.toml
```

or an HTTPS config URL. Secrets are referenced from the final config, for
example with `aws-sm://`, and resolved by OpenAB at runtime. Helm, kubectl,
ecsctl, oabctl, and operators should only describe runtime posture: image,
security context, persistent home, service account / IRSA, and the config URL.

This changes the right boundary for OCP. If OCP continues growing
`/bot-config` into a general OpenAB config renderer, it will duplicate OpenAB and
recreate the same maintenance problem OpenAB is deliberately removing from Helm:
every new OpenAB config feature would need an OCP PR.

## Decision

OCP does **not** own OpenAB runtime configuration.

The production direction is:

- OpenAB owns `config.toml` schema, `configUrl`, `configFile`, `s3://` fetch,
  `aws-sm://` secret resolution, `pre_seed`, `ambient`, hooks, agent command,
  pool settings, and agent runtime behavior.
- Bot deployment tooling owns image, filesystem posture, persistent home,
  service account / IAM, and the `openab run -c <configUrl>` command.
- OCP owns bot identity, inventory, sessions, rosters, admission, fanout,
  quorum/coordination policy, liveness, north APIs, and controller events.

`/bot-config/:id` remains a bootstrap and local-dogfood compatibility path. It
must not become the primary configuration surface for new OpenAB features.

### Amendment 2026-07 (B2): `/bot-config` renderer freeze

`bot_config()` rendering is frozen: bugfix-only, no new OpenAB runtime fields.
The one post-ADR drift was commit `957a547` / #60, which pinned the `allow_*`
trust fields in the rendered config ahead of OpenAB's trust flip; that is exactly
the kind of trust/gateway field growth this ADR's "should not add" list targets.
Future production work must move through OpenAB-owned profiles, `pre_boot`, and
deployment-owned `configUrl` artifacts instead of expanding this renderer.

## Scope Rules

OCP may keep doing these things:

- register and list bot identities;
- mint, validate, and revoke OCP gateway credentials;
- expose inventory and admission APIs;
- coordinate sessions and rosters;
- offer a legacy `/bot-config/:id` that is sufficient for the current quickstart;
- provide local development scripts that wire temporary Kubernetes Secrets or
  ConfigMaps for dogfood convenience.

OCP should not add these things:

- new OpenAB `config.toml` schema rendering;
- OpenAB `pre_seed`, `ambient`, hook, trust, gateway, platform-adapter, or agent
  runtime fields in OCP env vars;
- OCP-hosted S3-compatible object storage for steering files;
- product installation flows that require OCP to understand agent-specific
  steering locations beyond local development helpers.

If a new OpenAB feature can be expressed in final `config.toml`, the user or
deployment tool should put it in that external config. OCP should stay unaware.

## Consequences

### Positive

- OCP stays a coordination plane instead of becoming a second OpenAB config
  renderer.
- OpenAB config features no longer create follow-up work in OCP.
- Zeabur templates and future deployment tools can converge on the same shape as
  Helm's configUrl direction: runtime posture plus `openab run -c <configUrl>`.
- Steering becomes a bot property delivered by OpenAB `pre_seed`, a bot image, or
  the deployment tool, not a plane responsibility.

### Negative

- The existing quickstart remains split-brain for a while: `/bot-config` still
  works, but it is not the long-term production path.
- A clean migration requires solving bot gateway token externalization. Today
  `/bot-config` can serve `token_plain`; production configUrl needs either an
  external secret reference or an explicit registration flow that never requires
  OCP to render the whole config.
- Local dogfood scripts may still use Kubernetes ConfigMaps and Secrets because
  they optimize iteration speed, not production architecture.

### Neutral

- OCP can still return a `config_url` from bot discovery for legacy bootstrap.
  That URL should be understood as a compatibility aid, not proof that OCP owns
  config delivery.
- OCP may still mint session-scoped GitHub tokens. Consuming those tokens and
  configuring `gh` remains a bot/OpenAB concern.

## Migration Direction

1. Keep `/bot-config/:id` stable for current templates and local development.
2. Stop expanding `OABCP_AGENT_*` and `/bot-config` for new OpenAB features.
3. Design gateway token externalization so a bot can use an external
   `config.toml` without OCP rendering that config.
4. Move formal templates toward per-bot OpenAB `configUrl` once token
   externalization is available.
5. Trim PR-review trigger text after steering is reliably delivered through
   OpenAB-native mechanisms.

## References

- OpenAB PR #1271: `docs(adr): configUrl as primary config path over Helm rendering`
- [ADR 001 — Three planes](001-three-planes.md)
- [ADR 003 — Steering delivery](003-steering-delivery.md)
- [ADR 009 — Quota failover through bot discovery and explicit admission](009-quota-failover-bot-discovery.md)
- [Design discipline](../design.md)
