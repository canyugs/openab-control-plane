# openab-control-plane — progress & TODO

Status: gateway-native multi-agent control plane. Built, deployed, live-verified.
Full design + per-feature notes in `../multi-agent-review-ops/docs/control-plane-design.md`.
Stock OAB pods, unmodified — everything rides the existing gateway wire protocol.

## Done (2026-06-25)

Coordination substrate:
- South `/ws` gateway, North REST+SSE, SQLite store, per-bot token identity.
- Council engine: fanout, quorum via 🆗 (= OAB default `emoji_done`), one-thread-
  per-session, chair synthesizes verdict. Live 1/3/5-bot proven.
- **Streaming content** — edit target resolved from `reply_to` (not quote_message_id).
- **Thread recording** — plane presents `channel_type=supergroup` so OAB opens a topic.
- **Verdict SSE timing** — close+verdict on chair done-signal, emits edit-filled final.
- **Post-close chatter** — `deliver_event` + `handle_reply` gate on closed state.
- **Session isolation** — roster authorization in `handle_reply` (two-way: outbound
  fanout already roster-scoped). Orthogonal to OAB bot-side `allow_*` filters.
- **Durable delivery** — per-bot `outbox`; offline bots replay in order on reconnect.
- **Dynamic join + history backfill** — `POST /v1/sessions/:id/roster`; latecomer
  gets the full backlog via the outbox.
- Persistence: `/data` volume (`OABCP_DB=/data/plane.db`). Generation-tagged conns.

## Backlog

### Coordination primitives (next, in order)
- [ ] **Shared blackboard** — KV/task state agents can read+write (claim a task,
      shared scratchpad / partial results) instead of re-reading the chat log.
      *Paused here — design the model first (KV vs task-list+claim).*
- [ ] **Liveness / timeouts** — `last_seen` is recorded but nothing acts on it.
      Heartbeat-driven "agent X stalled → proceed/reassign"; step timeouts.
- [ ] **Targeted addressing / handoff** — first-class A→B direct send, not just
      broadcast+@mention-gate.

### PR public-review pipeline (to be a real "PR 公審會")
- [ ] **Diff into the trigger** — thin GitHub webhook shim: PR open → `gh pr diff`
      → POST as the session's opening message. Closes "PR in" + "agents see the
      code" together (agents review the diff text; no repo access needed).
- [ ] **Verdict back to PR** — plane image needs `gh` + GH token; verify
      `GithubPrComment` (`GH_OUTPUT=1`, `trigger_ref=github:pr/owner/repo#n`)
      against a real PR. Currently untested / `gh` not in the debian-slim image.

### Hardening
- [ ] **HA / scale** — single plane process + SQLite. Store trait seam exists
      (Postgres/libSQL drop-in) but untested. No HA.
- [ ] **Multi-tenant auth** — only a single bearer API key + per-bot tokens. No
      per-user identity / RBAC / "who may open a council".
- [ ] **/bot-config token leak** — serves `token_plain` inline (spike convenience).
      Move token to env/pre_seed; serve only non-secret config (design §10).

### Residuals (known, not blocking)
- At-least-once delivery: ack = handed-to-channel, not socket-confirmed; OAB has
  **no event_id dedup** (verified) → rare reprocessing on redelivery.
- Backfill/late-join is *active*, not silent: OAB responds to in-thread history
  (no silent-context-load mode in OAB).
- A trailing `…` status-stub can momentarily appear mid-stream (fills via edits).

## Live infra (Zeabur project openab-hub = `6a3abba9e41f9f1d193022cb`)
- plane: `openab-control-plane` svc `6a3ca6cde5f256c9f3d43e01` — `https://openab-control-plane.zeabur.app`, internal `:8080`, volume `data` at `/data`.
- **5 OAB pods still running** (consume resources — stop when not demoing):
  - chair: `oab-gandalf-red` `6a3cb4d3e5f256c9f3d440bb` (bot_3d9d…)
  - reviewers: `oab-rev1` `6a3cf6a4bdba1c7a91f8c1a3`, `oab-rev2` `6a3cf6a6bdba1c7a91f8c1a6`,
    `oab-rev3` `6a3cfcb5bdba1c7a91f8c461`, `oab-rev4` `6a3cfcbdbdba1c7a91f8c466`
- North API key: env `OABCP_API_KEY` on the plane service (also cached `/tmp/oabcp_key.txt`).
- Redeploy: `npx zeabur@latest deploy --project-id 6a3abba9e41f9f1d193022cb --service-id 6a3ca6cde5f256c9f3d43e01` (upload build; ~RUNNING in a few min).
