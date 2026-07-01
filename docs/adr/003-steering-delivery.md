# ADR 003 — Steering delivery: how standing review rules reach the bots

Status: superseded for OCP-hosted delivery by [ADR 010](010-openab-configurl-boundary.md) · 2026-06-27

## Context

A council trigger today carries two different things in one message: the
**runtime payload** (which PR, the diff) and the **standing steering** (reviewer
posts in-thread only, chair alone holds `gh` + maintains one `--edit-last`
comment, the `[done]` close protocol, plus the output format in
[docs/steering/pr-review.md](../steering/pr-review.md)). The steering half is
identical every run; only the diff changes.

ROADMAP Phase 1 ("Shared steering via `pre_seed`") wants the steering moved out of
the trigger into a boot-time layer so the trigger shrinks to just the diff. The
open question is *where the steering archive lives and who serves it* — which is an
architecture/trust-domain decision, not a code detail. This ADR freezes the options
while the choice was still open. ADR 010 later tightened the boundary: OCP should
not become an OpenAB config or blob renderer. Steering should be delivered as a
bot/OpenAB property, not by growing `/bot-config`.

**One thing this is NOT about: token savings.** The diff dominates trigger size;
the rules are small. The real wins of moving steering out are architectural —
steering becomes a first-class **bot property** (a file in `$HOME`), supports
**role-split layers** (base + chair-override), and isn't re-paid per round. If the
only goal were fewer tokens, the status quo would be fine.

## Mechanism constraint (verified)

OAB `pre_seed` is an **S3 client**, not a generic downloader: `sources` must be
`s3://…` (`parse_s3_uri` rejects anything else, `config.rs`), and it builds the AWS
S3 SDK with an optional `endpoint_url` override (`pre_seed.rs:44`) — the
LocalStack/MinIO path. Two consequences:

- A plane that wants to *serve* steering to `pre_seed` must speak **S3 GetObject**,
  not plain HTTP. (`pre_boot` + `curl` is out — the OAB image deliberately ships no
  downloader; that is *why* `pre_seed` exists.)
- OAB's `pre_seed` sets `endpoint_url` but **not `force_path_style`** (zero hits in
  the openab repo). The AWS Rust SDK then uses **virtual-host addressing**
  (`bucket.host`), so a path-style-only origin like the plane
  (`steering.control-plane.zeabur.internal`) won't resolve. **Option ③ is blocked
  on a one-line upstream OAB change** (`force_path_style(true)`, or auto-enable when
  `endpoint_url` is set).

## Options

> The table covers the three *candidate* options. A fourth transport —
> `include_str!` ("bake into the artifact") — is already shipped as ①'s current
> implementation, not a peer choice; it lives in the **Note** after *Rejected*.

| Axis | ① Trigger-embed (today) | ② `pre_seed` ← external R2/OSS | ③ `pre_seed` ← OCP-hosted S3 origin |
|---|---|---|---|
| External infra | none | provision/maintain a bucket | none (folded into the plane) |
| Per-pod credentials | none | **real cloud keys** (new secret surface + rotation debt) | dummy static creds (not a real secret) |
| Trust domains | one (plane) | +1 external | one (plane) |
| OAB change | no | no | **yes** — `force_path_style` first |
| OCP change | done (template extract) | none beyond documentation / packaging | not allowed by ADR 010 |
| Steering location | stuck in the trigger/launcher | boot-time bot property (`$HOME` file) | same as ②, archive is an OCP build artifact pinned to the plane version |
| Role-split layers | impossible (same trigger to all) | native (layers) | native (layers) |
| Per-trigger cost | rules resent every run | diff only | diff only |
| Integrity | none | built-in SHA256 | built-in SHA256 |
| One-click UX | simplest (nothing to set up) | worst (user supplies storage + keys) | good (plane self-contained) |
| design.md discipline | steering is the app's job, but lives in the launcher | **cleanest** — "file seeding belongs to OAB hooks / the deployer" | rejected by ADR 010 |
| Time / risk to ship | now, zero risk | medium (ops only, no code dep) | longest (gated on OAB PR + new endpoint), lowest ongoing ops |

### Rejected

- **`pre_boot` + `curl` from the plane's HTTP endpoint** — the OAB image ships no
  downloader by design (the reason `pre_seed` exists); fragile.
- **Plane injects steering into the served `config.toml`** — design.md explicitly
  forbids it ("we don't `[agent.steering]`-inject, don't serve `CLAUDE.md`").

## Note — `include_str!` is a fourth transport ("bake into the artifact")

Trigger-embed (①) is what ships today, and its steering source is a **single file**
consumed two ways:

- **manual path** (`open-council.sh`) reads the `.tmpl` at **runtime** — edit the
  file, it takes effect on the next run, no rebuild.
- **webhook path** (`council.rs`) embeds it with **`include_str!`** at **compile
  time** — the template is baked into the plane binary as a `const &str`. (Since
  v0.1.8 / ADR 004 the webhook posts the **pointer** template
  `pr-review-trigger-pointer.tmpl`, so the plane makes zero GitHub calls and the
  bots self-fetch — the `include_str!` mechanism is unchanged, only *which* template.)

So there is already a fourth steering-delivery transport beyond ①②③ — **bake into
the artifact** — and it is the lightest: single source of truth (one `.tmpl` feeds
both paths; the webhook carries a *compile-time snapshot*, so at any given release the
two renders are identical — editing the file without rebuilding lets them diverge
until the next build, which is exactly the rebuild cost named next), a self-contained
binary (no runtime file, no external store, no credentials), version-locked to the
release. Its cost is the mirror of pre_seed's benefit: **changing the webhook's
steering needs a rebuild + redeploy**, not a hot edit. For rules that are small and stable that is an acceptable
trade — part of why ① stays the *Now* choice. pre_seed (②/③) only earns its keep once
steering must change often, or be **shared across more than one trigger builder** /
delivered as a per-`$HOME` file with role-split layers — which `include_str!` can't do.

## Leaning (not decided)

- **Now:** keep ① — it works, and the trigger steering is already extracted to
  `scripts/pr-review-trigger.tmpl`. No rush.
- **Target after ADR 010:** ② or an equivalent OpenAB-native delivery path. The
  steering artifact belongs in external object storage, a bot image, `configFile`,
  or another deployment-owned layer. OCP may package or document the file, but
  should not host it or emit `pre_seed` config.
- **Rejected after ADR 010:** ③. A plane-hosted S3 origin would make OCP a blob
  origin and invite it back into OpenAB config delivery.

## Consequences / sequencing

- Do not make `/bot-config` emit `[hooks.pre_seed]`; that duplicates OpenAB
  config rendering and conflicts with ADR 010.
- If steering is moved out of the trigger, the deployment should point OpenAB at
  final config through `configUrl` / `configFile`, and that config can reference
  OAB-native `pre_seed` sources.
- Revisit external steering delivery when there's a concrete driver (a third-party
  deploy that needs role-split steering, or trigger bloat that actually bites).
  Until then, keep trigger-embedded steering for compatibility and avoid adding
  OCP-owned config delivery.
