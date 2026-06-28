# skills/

Standing **bot steering** as droppable Agent skills — the "how to behave" half that
[design.md](../docs/design.md) says is the bot's (OAB's), not the plane's.

Housed in this repo **for now** (single source of truth, version-controlled), but
each skill is self-contained so it can be dropped into a bot's `~/.claude/skills/<name>/`
(or the equivalent for other agent CLIs) and the bot then carries the behaviour as its
own property — independent of who convened it.

## Why here, where it's going

Today the review steering is baked into the OCP trigger templates
(`scripts/pr-review-trigger*.tmpl`, embedded via `include_str!` — see
[ADR 003](../docs/adr/003-steering-delivery.md)). That works but smudges the
OCP/OAB boundary: standing review *behaviour* is agent steering, which belongs to
the bot, not the plane.

The target is **clean separation**:

- **OCP trigger** → a minimal per-session pointer: *what* to review now (`owner/repo#N`, preset/angle, "self-fetch").
- **This skill** → delivered to the bots as a property: *how* to review.

Sequencing: the skill is authored here first (this is its home for now). Slimming the
trigger templates to a pure pointer happens **after** the skill is actually delivered
to the bots (an OAB-side concern — pre_seed / bake into the bot image / git clone),
so the live council never loses the rules mid-cutover.

## Skills

- **[pr-review](pr-review/SKILL.md)** — how to review a PR as a council reviewer or chair.
