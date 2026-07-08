# Route — the code review product, two phases (2026-07)

Status: active · 2026-07-08 · This is the **ordering** document. It coordinates
three plans that each own their own detail — it adds no new items:

- [roadmap.md](roadmap.md) — item status source of truth (engineering phases 1–4)
- [boundary-review-2026-07.md](boundary-review-2026-07.md) — why: verified
  gaps + staged fixes (Stages 0–4, findings A/B/C/D/M)
- [pr-mention-plan.md](pr-mention-plan.md) — how: the author-loop feature plan
  (P0–P3, CodeRabbit parity map)

The rule, set 2026-07-08: **Phase 1 ships nothing beyond what CodeRabbit
already does — it makes that loop run smoothly on our stack. Phase 2 is where
new capability lands.** "Smoothly" is a feature: most of Phase 1 is
reliability work on defects dogfood already hit, not visible surface.

## Phase 1 — run CodeRabbit's loop, smoothly

Definition of done — an author lives the full CodeRabbit loop with no holes:

> open PR → review appears; push fixes → the stale round is superseded and the
> new head is re-reviewed; `@handle review <fix notes>` → re-review; `@handle
> <question>` → answer in thread; every verdict cites the head it reviewed;
> runaway cost is bounded; nothing hangs, double-posts, or reviews stale code.

### 1a. Interaction surface (pr-mention-plan P0–P2)

| Item | Source | Status |
|------|--------|--------|
| Auto review on open / `/review` / preset labels | roadmap Phase 1–2 | ✅ shipped |
| `/ask` + `@mention` Q&A (solo) | ADR 011 | ✅ shipped |
| P0 steering belts: `Reviewed at <sha>`, ledger preservation vs `--edit-last`, marker check, rebase fallback; Q4 single-source pre-task | pr-mention-plan §4–5, §9 | ✅ shipped (#113, #125) |
| P1 supersede mechanism (M1): `trigger_fingerprint`, atomic close+open via interpreter, bot-author guard, hourly cap + round budget | pr-mention-plan §3 | ✅ shipped (#126; A5 dep landed as #117) — live-verified on prod (push mid-round superseded the stale session, PR #149) |
| P2 `@handle review [notes]` / `full review` + carried delta strings | pr-mention-plan §2, §7 | ✅ shipped (#127) — live-verified end-to-end from GitHub (`cmd:<comment_id>` fingerprint convene + supersede, PR #148) |

### 1b. Reliability substrate (boundary-review Stages 0–1)

Every Stage 1 item closes a defect already observed or measured on dogfood —
this is the "smoothly", none of it is new feature surface:

- Stage 0 paper items (declare invariants, fix false doc claims, arm the
  split-test metrics, supersede-vs-dedupe rule, `/bot-config` freeze + sunset
  condition — the B2 *execution* stays Phase 2).
- Stage 1 in full: anchored done/verdict/recruit parsing (A1/A7/A8), closed
  outbox purge (A5 — P1's dependency), per-bot flush serialization +
  delivered-marker (A2), WS ping (A3), audience-aware capped backfill (A4),
  monotonic votes (A6), trigger re-delivery (A9), explicit fanout multiplicity
  (A10), boot `connected` reset + replace-path re-check (C7/C3), WAL +
  messages index (C1/C2), SSE Lagged resync (C4), `/v1/sessions` through the
  interpreter (B3 — P1 lands on top of it), membership runbook + DELETE route
  (C6).

### 1c. Explicitly NOT in Phase 1

| Deferred item | Where it lives |
|---------------|----------------|
| Findings ledger, `status` / `resolve Fn` / `help` commands (M4) | Phase 2 (pr-mention P3) |
| Controller extraction (`plugins/pr_review`, B1/B2 demotion) | Phase 2 (boundary Stage 3) |
| Typed OAB wire extension (dedup, `context` flag, typed directives) | Phase 2 (boundary Stage 2) |
| Inline review-thread replies, `pause`/`ignore` | Phase 2, named triggers (pr-mention §8) |
| Multi-agent panels beyond review, act-as-user, HA/scale (Stage 4) | Phase 2+ / trigger-gated |
| Forum support north client | separate track — not gated by this route, but its build fires Stage 3's trigger |

**Phase 1 exit:** pr-mention P0 + P1 + P2 exit criteria live-verified on
dogfood, and the Stage 1 checklist merged. At that point the product does what
CodeRabbit does — reliably — and nothing more.

> **EXIT REACHED 2026-07-08.** All 16 Stage-1/P0–P2 issues (#112–#127,
> milestone "Phase 1 — CodeRabbit parity") merged via PRs #128–#143; every
> loop segment live-verified: PR-open auto-convene and push-supersede on
> PR #149, mention grammar + `cmd:` supersede on PR #148, verdict-cites-head
> and cost valves throughout. Follow-ups also landed: #144/#147 chair
> baseline + angle expansion, #148 trust wording, #145 legacy-DB upgrade fix,
> 0.1.15 published with steering preloaded into bot pods. Phase 2 is next,
> starting at Stage 3 extraction.

## Phase 2 — make review solid

Ordering within Phase 2 is deliberate: extraction first (it is the enabler),
then the features that need controller state, with eval keeping score.

1. **Stage 3 extraction** — `plugins/pr_review` bundled controller behind
   `controller.rs`; `/bot-config` freeze/demotion; ADR 016 finish. Trigger:
   forum north-client build or nuphos Gen-1 migration (both are queued
   consumers). Plan: [stage3-extraction-plan.md](stage3-extraction-plan.md)
   (S1–S17 sequence, proposed 2026-07-08).
2. **Structured delta review (M4)** — findings table, machine findings block
   (Q3), `status` / `resolve Fn` / `help`, comment-by-ID lifecycle closing M3
   structurally. This is the "更扎實" core: never-re-raise becomes a diff
   against ground truth instead of reviewer memory (Gen-1: 69% multi-round,
   findings re-discovered across 5 rounds).
3. **Typed OAB wire upstream (Stage 2)** — event dedup, passive backfill,
   typed done/verdict; submit the ADR early, land whenever accepted (outside
   our control — Stage 1's text fallback holds regardless).
4. **Eval-driven quality (eval-harness track, ADR 015)** — delta-review recall/precision
   through the Martian bench + dogfood precision ledger; reduced re-review
   panel when round-N spend measures > 50% of round-1; angle tuning from
   evidence, not taste.
5. **Deferred parity + scale** — inline replies and `pause`/`ignore` on their
   named triggers; Stage 4 store/membership work only when the Stage 0 armed
   metrics trip.

## Doc ownership

| Question | Doc |
|----------|-----|
| What is the status of item X? | [roadmap.md](roadmap.md) |
| Why does gap X exist and who owns the fix? | [boundary-review-2026-07.md](boundary-review-2026-07.md) |
| How does the author loop work / what exactly ships in P0–P3? | [pr-mention-plan.md](pr-mention-plan.md) |
| What order, and what is deliberately not now? | this file |

When these disagree on ordering, this file wins; on item detail, the owning
doc wins; on status, roadmap.md wins.
