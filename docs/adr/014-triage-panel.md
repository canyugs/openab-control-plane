# ADR 014 — Second panel: incident/ticket triage council

Status: proposed · 2026-07-03

## Context

Track B's premise: prove the coordination substrate generalizes beyond code
review before building any Phase 3 primitive (blackboard, targeted addressing)
or the ADR 007 plugin boundary. The second panel is deliberately chosen to be
the cheapest probe that still produces *new information* about where the
plugin boundary sits.

Candidates considered:

- **Q&A panel** — effectively shipped as the `/ask` solo session (#30); too
  close to review to teach anything.
- **Research panel** (roadmap Future) — same broadcast+synthesize shape, zero
  new primitives, but its real risk is pod *web-research capability*, which is
  an OAB/pod concern orthogonal to the plane. Deferred, not rejected.
- **Doc/design review** — review with different input; only teaches one new
  thing (non-PR artifact homes).
- **Coding panel** — highest value, but immediately pulls the Phase 3
  blackboard and write-side-effect policy. Third panel material.
- **Incident/ticket triage** — broadcast+synthesize shape, real daily value
  for the first dogfood operator, and it exercises exactly the two dimensions
  PR review does *not*: a **non-GitHub trigger surface** and **per-role tool
  bindings**. Chosen.

## Decision

Ship a **triage council**: N investigators examine a reported incident/ticket
from assigned angles, a chair synthesizes a structured triage report. The
plane performs no side effect; the caller owns the ticket system.

### 1. Trigger — the generic `POST /v1/sessions`, no new endpoint

*(Amended 2026-07-03 before implementation: the first draft proposed a
dedicated `POST /v1/triage` mirroring `/v1/review`. Dropped — the generic
session API already carries everything triage needs.)*

The shim renders the triage trigger text (from the packaged template) and
calls the existing north primitive:

```json
POST /v1/sessions
{
  "title": "triage",
  "trigger_ref": "triage:forum:zeabur/12345",   // idempotency key
  "quorum_n": 2,
  "chair_bot": "chair",
  "roster": ["chair", "rev1", "rev2"],
  "mode": "triage_council"
}
```

then posts the rendered trigger as the opening message. The store's active
`trigger_ref` uniqueness gives the same dedup `/v1/review` has. **The plane
gains no endpoint and no triage logic** (the whole plane diff is the one
done-semantics guard arm in §3). This is the ADR 007 plugin model exercised
for real: a panel is a template + steering + an external shim, and the kernel
barely knows it exists. `/v1/review` stays as
the historical exception (it earns its keep as the one-curl GitHub Action
surface); it is not the pattern new panels copy.

No generic inbound-webhook adapter in v1. The **caller adapts** (a forum bot,
an alertmanager hook, a support-queue shim) — the same reasoning that keeps
the plane out of GitHub (ADR 004/011). A dedicated convenience endpoint is
deferred until a second shim demonstrates template drift the manifest layer
(ADR 007) should solve instead.

### 2. Roster — existing council mechanics, triage angle set

Chair + investigators from the standing roster; angles round-robined with the
existing preset machinery. Default angle set:

- `symptoms` — reproduce/localize from the report; what exactly fails
- `config` — deployment/config/version drift; what changed
- `account` — plan/billing/quota/permission causes
- `history` — known issues, prior tickets, recent incidents

Angles are a **request parameter with a default**, not plane code — the angle
set is the first field of the future plugin manifest (ADR 007).

### 3. Mode — `triage_council` (QuorumCouncil mechanics, text-done chair)

`recipient_text` only specializes for `review_council`; other modes deliver
the trigger as-is, so the triage template embeds the per-role tasks (chair vs
investigator sections + angle assignment), the same way `open-council.sh`
free-text councils already work.

*(Amended after the first dogfood run.)* The draft claimed generic `council`
mode with zero orchestrator changes. The first live run disproved it: a
prompt-driven chair (CLI bot) auto-🆗s the system quorum prompt, and in
generic `council` a chair 🆗 **is** the native OAB done-signal (`set_done`),
so the session closed with a "still waiting" verdict. The two chair kinds
have genuinely different done semantics, and mode is where that lives:

- `council` — native contract unchanged: chair `set_done`/🆗 closes.
- `triage_council` — rides QuorumCouncil via the dispatch default; the chair
  closes only with the explicit text `[done]` (same guard review councils
  already needed). One `matches!` arm in `reaction_counts_as_done` is the
  entire plane diff.

### 4. Tool bindings — pod profile, not plane config

Investigators use whatever read tools their pod profile mounts (kubectl, log
CLIs, vendor admin CLIs…). The plane neither knows nor mints these in v1.
This is a deliberate gap: it is the second concrete datum (after the chair's
`gh`) that **tool/secret requirements are plugin-declared, pod-satisfied** —
the ADR 007 manifest must carry them. Session-scoped credential minting for
non-GitHub tools stays with the Phase 4 identity quartet.

### 5. Output artifact — structured report as the session verdict

The chair's final message is the report (severity, likely cause, evidence,
suggested next actions, confidence, what was not checked). It reaches
consumers over the existing surfaces: north `verdict` event / sessions API /
ADR 012 close webhook. The triggering shim posts it back to the ticket system
**as a draft for a human** — the plane and pods never write to the ticket
system in v1.

ADR 013's structured-verdict columns (`decision`, `findings_*`) are
review-shaped; triage keeps a text verdict in v1. Lesson recorded for B4: the
structured-verdict schema is per-plugin, not a core table.

### 6. Done-signal — investigators unchanged; chair gated on the report

Investigators end with `[done]` (text or 🆗); quorum prompts the chair.
*(Amended after dogfood rounds 2/5.)* Prompt-driven chairs habitually append
`[done]` to acknowledgments ("noted, standing by. [done]") no matter how the
prompt forbids it. So in `triage_council` the chair's `[done]` counts only on
a message that starts with `TRIAGE` — the report literally is the
done-signal. Ignored acks leave the session open (warn logged); the watchdog
stays the backstop. Liveness (watchdog + A3 sweep) applies as-is.

## What this deliberately does not need

- **No shared blackboard (B2)** — investigators broadcast findings in-thread;
  the chair reads the thread. Same as review.
- **No targeted addressing (B3)** — fanout + chair synthesis suffices.
- **No new store columns, no webhook adapters.**
- ~~No new coordinator~~ — *contradicted by dogfooding (recorded per this
  section's own rule): the review-flavored quorum prompt sent triage chairs
  PR-hunting, so `TriageCouncil` exists as a thin `QuorumCouncil` wrapper
  whose only delta is the chair synthesis prompt. The lesson for B4: the
  quorum-prompt text is a plugin manifest field, not core copy.*

If implementation contradicts this section, that finding — not this ADR —
decides whether B2/B3 get built. (B2/B3 remain un-needed.)

## What this teaches B4 (plugin boundary)

The complete diff between `pr-review-council` and `triage-council` is the
draft plugin manifest field list:

| Field | pr-review | triage |
|---|---|---|
| Trigger surface | GitHub webhook + `/v1/review` | generic `/v1/sessions`, shim-rendered trigger |
| Angle set | correctness/security/… presets | symptoms/config/account/history |
| Recipient tasks | mode-specialized (`review_council`) | embedded in trigger (`triage_council`) |
| Steering doc | pr-review.md | triage section/file |
| Side-effect owner | chair pod (`gh`) | triggering shim (draft reply) |
| Verdict schema | decision + r/y/g columns | text (schema per-plugin) |
| Tool/secret needs | `gh` + App/PAT | pod-profile read tools |

## Dogfood plan

First instance: Zeabur support-forum shim (lives outside this repo) — ticket
body → shim renders trigger → `/v1/sessions` → close webhook → draft reply attached to the ticket for
a human to edit/send. Output is always a draft in v1; auto-posting is out of
scope until precision is measured (A4 applies to triage too).

## Deferred

- Triage-specific structured trailer (severity/confidence columns)
- Generic inbound webhook adapter / event-source framework (Argo-style)
- Auto-posting replies; any write side effect from pods
- Per-angle scoped credentials (identity quartet, Phase 4)
