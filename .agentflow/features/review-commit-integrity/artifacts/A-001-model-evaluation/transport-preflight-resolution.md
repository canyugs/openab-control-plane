* _2026-09-09 22:18:37 (GPT-6/default)_

# Verified transport implementation clarification

Both claude-opus-5 and claude-opus-4-6 completed real authenticated structured-output calls. Installed Claude 2.1.266 supports --safe-mode --restricted --disable-slash-commands --tools empty --strict-mcp-config --setting-sources empty. This disables customizations and all executable/file/MCP tools while preserving existing OAuth authentication; no API key extraction or credential copying needed. Actual init metadata lists only StructuredOutput, empty plugins/skills/MCP. JSON output can be an event array ending in a result object, not only a result object. Raw stdout paths and exact argv in transport-preflight.json. Enable this verified text-only OAuth transport in addition to the approved bare/API-key path; same isolation requirement, no model or execution scope expansion. Prompt/schema alone are not confinement. Reject unexpected tool metadata or any executable tool use; preserve transport failure. Codex remains unavailable until actual no-tools confinement proven.

modelUsage includes CLI internal auxiliary calls and costBasis=list; preserve each emitted model and usage, requested judge identity stays frozen argv, backend identity from assistant transport messages. total_cost_usd is NOT provider-reconciled actual billing here; expose estimate separately and actual cost unknown. The distinct strong requested models worked; no weaker substitution was made.

Docker daemon 29.7.2 started locally. Pulled python:3.12-alpine and pinned digest sha256:b64631e04e4920160c50fbe8d8df828f7f35f06f425cb44aa09bca53e708a35a. Isolated host-authored probe verified UID65534, /work, and no Docker socket with network none/read-only root/drop caps/no-new-privileges. This proves environment readiness, not the unimplemented product workflow. Synthetic vulnerable and fixed Git snapshots prepared outside repository; actual project CLI journey pending.

Self-check: Verified argv/output/environment only; implementation and full acceptance not claimed.
