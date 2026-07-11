# ADR 020 — Review audit and effectiveness ledger

Status: proposed · 2026-07-09

## Context

OCP already records enough to reconstruct a council session at the conversation
level:

- `sessions` stores trigger identity, mode, state, created/closed timestamps,
  chair, quorum, `decision`, and red/yellow/green finding counts.
- `messages` stores client/system/bot transcript content.
- `reactions` stores done signals and other reaction evidence.
- `/v1/session-log` rebuilds a text timeline without relying on Discord or
  GitHub APIs.

That is useful for support triage, but it is not enough for two product needs:

1. **Audit:** answer "what exactly happened, in what order, and who/what caused
   it?" after the fact, even if no SSE client was connected.
2. **Effectiveness:** answer "did this PR review help?" with finding-level
   evidence: what was raised, whether it was fixed, whether it reappeared, and
   which reviewers/presets produced useful findings.

ADR 017 already identifies the durable event-log gap: `emit_north` broadcasts
structured lifecycle events but does not persist them. ADR 013 added
session-level structured verdict counts. ADR 018 intentionally leaves those
review-shaped columns as temporary residue until M4 moves new review state into a
plugin-owned table.

The missing piece is the product ledger between those two layers: durable
runtime events explain the control plane; a PR-review findings ledger explains
the review's actual effect.

## Decision

Adopt a two-layer audit model:

1. **Core audit events** — persist OCP lifecycle and decision events, as scoped
   by ADR 017.
2. **PR-review effectiveness ledger** — add plugin-owned PR review tables for
   review rounds, stable findings, and finding status history.

The two layers are related but not merged. Core events answer "what did OCP do?"
The PR-review ledger answers "what did the review find, and what happened to
those findings?"

## Layer 1: Core audit events

Implement ADR 017 before or alongside the first ledger write path:

- Add append-only `events` storage for emitted north events.
- Persist the same structured shape OCP already emits:
  `type`, `session_id`, `payload`, `ts`.
- Add query API:
  `GET /v1/events?session_id=&kind=&since=&until=&limit=`.
- Include OCP control events that matter for audit:
  state transitions, quorum, timeout, supersede, roster changes, close webhook
  attempts, token mint/revoke outcomes, and webhook admission decisions.

Do not block on complete tool-call capture. Tool-level payloads stay out of the
core event table. A coarse "bot activity happened / succeeded / failed" event is
acceptable when the pod can report it without leaking arguments, stdout, secrets,
or repository-private bulk content.

## Layer 2: PR-review effectiveness ledger

Add PR-review plugin-owned tables. Do not add new `sessions` columns for M4-era
review state.

### `pr_review_rounds`

One row per PR-review council round, keyed to the OCP session.

Minimum fields:

| Field | Purpose |
|---|---|
| `id` | Stable round id |
| `session_id` | OCP session id |
| `trigger_ref` | Existing idempotency key, e.g. `github:pr/owner/repo#123` |
| `trigger_fingerprint` | Head SHA or command fingerprint that opened the round |
| `repo`, `pr_number` | Query dimensions without parsing `trigger_ref` |
| `head_sha` | Reviewed head SHA |
| `base_sha` | Prior reviewed SHA for delta rounds, nullable |
| `preset` | `lite` / `quick` / `standard` / `full`, when known |
| `roster_json` | Roster snapshot at open/close |
| `decision` | `approve` / `request_changes`, nullable until close |
| `findings_red`, `findings_yellow`, `findings_green` | Count snapshot |
| `comment_url` | Final verdict comment URL, if the chair reports it |
| `github_review_url` / `github_review_id` | Optional, if available |
| `status_url` | Optional commit-status target URL |
| `opened_at`, `closed_at` | Timing metrics |

### `pr_review_findings`

One row per stable finding identity on a PR.

Minimum fields:

| Field | Purpose |
|---|---|
| `id` | Internal id |
| `repo`, `pr_number` | Finding scope |
| `stable_id` | User-facing id such as `F1` |
| `first_round_id` | Round that first introduced it |
| `first_seen_sha` | Head where it was first observed |
| `severity` | `red` / `yellow` / `green` |
| `title` | Short finding title |
| `path`, `line` | Primary cited location, nullable for repo-wide findings |
| `raised_by` | Reviewer id or `chair`, when known |
| `source_message_id` | OCP message that introduced the evidence |
| `status` | `open` / `resolved` / `dismissed` / `stale` |
| `resolved_sha` | SHA where fixed, nullable |
| `current_round_id` | Last round that touched the finding |

### `pr_review_finding_events`

Append-only history for status changes and re-review evidence.

Minimum fields:

| Field | Purpose |
|---|---|
| `id` | Event id |
| `finding_id` | Stable finding row |
| `round_id` | Review round where this event happened |
| `event_type` | `raised` / `still_open` / `resolved` / `dismissed` / `severity_changed` |
| `actor_kind`, `actor_id` | `bot`, `user`, `system`, or `controller` |
| `from_status`, `to_status` | Status transition |
| `sha` | Relevant head/fix SHA |
| `note` | Short reason or author fix note |
| `created_at` | Audit timestamp |

This table is what makes "never re-raise a resolved finding" enforceable by
state instead of reviewer memory.

## Input format

Do not parse arbitrary Markdown tables as the source of truth. Markdown remains
the human-facing report. The machine source should be a small, hidden structured
block in the chair final comment/message, for example:

```markdown
<!-- openab-findings
{
  "round": {
    "head_sha": "...",
    "base_sha": "...",
    "comment_url": "..."
  },
  "findings": [
    {
      "id": "F1",
      "severity": "red",
      "status": "open",
      "title": "...",
      "path": "src/lib.rs",
      "line": 42,
      "raised_by": "rev1"
    }
  ]
}
-->
```

The legacy `[[verdict:approve r=0 y=2 g=5]]` trailer remains supported during
migration. When both exist, the structured block is authoritative and the trailer
is a compatibility summary.

## Query surfaces

Add read APIs before building a dashboard:

- `GET /v1/review/rounds?repo=&pr=&trigger_ref=&limit=`
- `GET /v1/review/findings?repo=&pr=&status=&severity=`
- `GET /v1/review/findings/:id/events`
- Extend `GET /v1/sessions/:id` with links/ids to review-round rows, not by
  embedding the entire finding ledger by default.

Keep `/v1/stats` as the small live rollup. Long-horizon effectiveness queries
should read the review ledger, not accumulate more fused counters in the kernel.

## Metrics enabled

The ledger should support these first-order questions:

- Time to verdict by preset, roster, repo, and PR size bucket.
- Findings per review and severity mix.
- Resolution rate: open → resolved within N pushes / N days.
- Re-review churn: same finding still open across rounds.
- Rediscovery rate: a resolved finding reappears or is re-raised.
- Reviewer contribution: which reviewers raise findings that survive chair
  synthesis and later resolve.
- Actionability proxy: red/yellow findings that receive author fix notes or
  resolution events.

Precision/recall against a benchmark remains the eval harness's job (ADR 015).
The ledger supplies production evidence; it is not itself ground truth.

## Privacy and retention

- Store finding summaries, source links, statuses, and short notes.
- Do not store raw tool stdout/stderr, full diffs, environment variables, tokens,
  or private key material in the ledger.
- Message transcripts already store bot reports; the ledger should reference
  `source_message_id` rather than duplicating long explanations.
- Define retention separately once volume is measured. Initial implementation
  may keep SQLite append-only history, matching ADR 017's stance.

## Migration plan

1. Ship ADR 017 core `events` table + query API.
2. Add plugin migration hook for PR-review-owned tables, per ADR 018.
3. Add `pr_review_rounds` writes at session open/close.
4. Teach chair steering/task template to emit the hidden findings block.
5. Parse the hidden block on close and populate findings/events.
6. Add `status`, `resolve F<n>`, and `dismiss F<n>` comment commands as ledger
   mutations.
7. Deprecate direct dependence on `sessions.findings_*` for new features; keep
   the fields on wire surfaces until a versioned API change replaces them.

## Consequences

### Positive

- Audit and effectiveness become queryable from OCP without scraping PR comments
  or Discord/GitHub history.
- Stable finding ids become real state, so re-review can reason against a ledger.
- The runtime kernel remains generic: new review state lives in PR-review plugin
  tables, not in more `sessions` columns.

### Negative

- Requires a machine-readable chair output block; steering and tests must make
  this reliable.
- Adds append-only data volume to SQLite.
- Some effectiveness metrics still require external signals, such as merge,
  revert, production incident, or human dismissal.

### Neutral

- Existing session counts and `[[verdict:…]]` trailers remain compatible.
- Existing PR comments stay human-readable; the hidden block is a machine
  companion, not a replacement for the report.

## Open questions

1. Should finding ids be scoped per PR (`F1`) or globally namespaced
   (`owner/repo#123/F1`) in the API? Storage should keep both internal ids and
   PR-scoped stable ids.
2. Should `green` observations live in `pr_review_findings`, or should the table
   be red/yellow only with green as round metadata? Initial design keeps all
   three severities for parity with current counts.
3. How much GitHub metadata should the chair report directly versus OCP deriving
   it through future GitHub APIs? Current boundary says the chair/pod reports it.

## References

- [ADR 013 — Decision→review-state](013-decision-review-state.md)
- [ADR 015 — Eval harness](015-eval-harness.md)
- [ADR 017 — Message observability / audit layer](017-message-observability-audit-layer.md)
- [ADR 018 — Stage 3 extraction rulings](018-stage3-extraction.md)
- [route.md M4](../route.md)
