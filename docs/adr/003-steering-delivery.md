# ADR 003 — Steering delivery: how standing review rules reach the bots

Status: **proposed** (undecided) · 2026-06-27

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
while the choice is still open.

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

| Axis | ① Trigger-embed (today) | ② `pre_seed` ← external R2/OSS | ③ `pre_seed` ← OCP-hosted S3 origin |
|---|---|---|---|
| External infra | none | provision/maintain a bucket | none (folded into the plane) |
| Per-pod credentials | none | **real cloud keys** (new secret surface + rotation debt) | dummy static creds (not a real secret) |
| Trust domains | one (plane) | +1 external | one (plane) |
| OAB change | no | no | **yes** — `force_path_style` first |
| OCP change | done (template extract) | bot_config emits `pre_seed` + package/upload | bot_config + **new minimal S3-GetObject origin** |
| Steering location | stuck in the trigger/launcher | boot-time bot property (`$HOME` file) | same as ②, archive is an OCP build artifact pinned to the plane version |
| Role-split layers | impossible (same trigger to all) | native (layers) | native (layers) |
| Per-trigger cost | rules resent every run | diff only | diff only |
| Integrity | none | built-in SHA256 | built-in SHA256 |
| One-click UX | simplest (nothing to set up) | worst (user supplies storage + keys) | good (plane self-contained) |
| design.md discipline | steering is the app's job, but lives in the launcher | **cleanest** — "file seeding belongs to OAB hooks / the deployer" | mild tension (plane becomes a blob origin — but serves opaque bytes, never injects) |
| Time / risk to ship | now, zero risk | medium (ops only, no code dep) | longest (gated on OAB PR + new endpoint), lowest ongoing ops |

### Rejected

- **`pre_boot` + `curl` from the plane's HTTP endpoint** — the OAB image ships no
  downloader by design (the reason `pre_seed` exists); fragile.
- **Plane injects steering into the served `config.toml`** — design.md explicitly
  forbids it ("we don't `[agent.steering]`-inject, don't serve `CLAUDE.md`").

## Leaning (not decided)

- **Now:** keep ① — it works, and the trigger steering is already extracted to
  `scripts/pr-review-trigger.tmpl`. No rush.
- **Target:** ③ — OAB is ours, so the `force_path_style` prerequisite is a cheap
  in-house PR, and it yields the cleanest end-state: zero external infra, no real
  per-pod secrets, one-click self-contained. The cost is a small new S3-GetObject
  surface on the plane and a mild design.md tension (hosting opaque steering bytes).
- **Fallback:** ② — if we decide OCP should not be a blob origin, an external
  bucket is the orthodox path at the price of ops + a per-pod credential surface.

## Consequences / sequencing

- If we pursue ③, the OAB `force_path_style` change lands **first** — it is the
  precondition for the plane origin to be reachable at all.
- ② and ③ share the same OCP-side work (bot_config emits `[hooks.pre_seed]` with
  per-role `sources`; trigger template drops the rules); they differ only in the
  `endpoint_url`/credentials and who hosts the archive. So the OCP work is not
  wasted regardless of which storage backend wins — decide the backend late.
- Revisit when there's a concrete driver (a third-party deploy that needs role-split
  steering, or trigger bloat that actually bites). Until then this stays *proposed*.
