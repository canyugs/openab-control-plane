# ADR 004 — Bot identity: one shared GitHub App, pod-local; GitHub I/O stays in the pod

Status: accepted · 2026-06-28

## Context

A review bot must post to the PR as a *bot* identity (clean `[bot]` attribution), not
as a human's PAT. The question is **where the GitHub identity lives** — and it kept
getting re-evaluated, so this ADR pins it.

Two prior data points:

- **Previous gen** (`zeabur/multi-agent-review-ops/github-apps`): one GitHub App **per
  bot** (5 apps: Gimli/Legolas/Aragorn/Boromir/Gandalf), each pod holding its own key
  and minting an installation token at bootstrap (`get-token.sh` → `gh auth login
  --with-token`). Identity was **pod-local**; the plane (Discord, then OCP) never
  touched it. The per-bot model is now **deprecated** (the `apps.json` checked into
  that repo is stale).
- **OCP PR #8** built a *plane-centralized* alternative: the plane holds one App key,
  mints per-role scoped installation tokens, and serves them via
  `/v1/sessions/:id/github-token`. This was validated (issue #9, L3) but is heavier:
  to wire the chair to it cleanly we explored four designs (chair-fetches-via-REST,
  plane-proxies-the-post, bot-token auth, credential-over-south) — all either crossed
  the north/south flow, fattened the plane, or leaked a key to the agent.

## Decision

1. **One shared GitHub App, not per-bot.** The single App is `zeabur-council`
   (App ID 4146119). One App → one identity (`zeabur-council[bot]`). OCP councils post
   **only the chair's verdict** to the PR (reviewers deliberate in the session thread,
   they don't write to the PR), so a single posting identity is exactly right — the
   per-bot multiplicity that justified 5 apps in the Discord era no longer applies.

2. **Identity is pod-local and is OpenAB's job, not the plane's.** The posting pod
   (the chair) holds the shared App key as env (`GITHUB_APP_KEY` base64 / `GITHUB_APP_ID`
   / `GITHUB_INSTALLATION_ID`) and bootstraps a token with the existing
   `get-token.sh` → `gh auth login --with-token`. Then its ordinary `gh pr comment …`
   posts as `zeabur-council[bot]`. **OCP needs no code change for this.**

3. **The plane does NOT mint or distribute GitHub tokens for posting.** PR #8's
   `/v1/sessions/:id/github-token` + per-role scoped minting is **not** the path for
   chair posting. It remains a validated, available capability (north/operator use),
   but the chair-posting use case is served pod-local. The control plane stays thin.

4. **GitHub I/O belongs to the pods — both write and read.** Posting is pod-local
   (above); reading the PR diff should also move to the pod (the **self-fetch** pointer
   trigger added in `open-council.sh`), so the plane never calls GitHub. `src/council.rs`
   currently fetches the diff in the plane and inlines it; that should migrate toward a
   pointer trigger so the bot self-fetches. Tracked as a follow-up.

## Consequences

- **Dropped:** all plane-side identity-wiring designs (chair-fetch-via-REST,
  plane-proxy, bot-token-auth, credential-over-south). They solved a problem we don't
  have once identity is pod-local with a shared App.
- **To enable App-identity posting:** give the chair pod the `zeabur-council` App key +
  the `get-token.sh`/`gh auth login` bootstrap (ops/pod config, e.g. the
  `multi-agent-review-ops/github-apps` mechanism) — no OCP change.
- **`src/council.rs` plane-side diff fetch** is against the "plane out of GitHub"
  direction; reconcile toward self-fetch (pointer trigger). Until then it reads the
  diff via an App read-token or `GH_TOKEN` — works, but is the thing to retire.
- **The plane's `GITHUB_APP_*` env** (set for PR #8 / the diff read) and the
  `/github-token` endpoint are superseded for posting; revisit whether to keep them as
  a north capability or remove once self-fetch lands.
- Relationship to [ADR 001](001-three-planes.md): GitHub identity + I/O live in the
  pod/membership layer, not the control plane — the plane guarantees coordination, not
  "how a PR comment gets written."
