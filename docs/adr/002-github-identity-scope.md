# ADR 002 — GitHub identity scope: multi-repo now, multi-installation / multi-app deferred

Status: accepted · 2026-06-27

## Context

OCP's GitHub App identity (see PR #8, `src/github_app.rs`) lets the plane act on
GitHub as itself with per-role scoped installation tokens. As soon as that landed,
three "multi-X" axes got conflated in discussion — and conflating them leads to the
wrong build order (e.g. building tenant isolation before it's needed, or assuming a
second *installation* is how you add a non-review capability). They are independent
dimensions; this ADR fixes the vocabulary and the decision for each.

The single fact that disambiguates them: **a GitHub App installation already spans
many repositories.** Installing an App on an org with "all repositories" (or a
selected set) is *one* installation covering *N* repos. An installation access token,
minted without a `repositories` filter, can act on every repo in that installation;
the `permissions` body scopes the *permission level* (chair=write, reviewer=read),
not *which repos*.

## Decision

Recognize three orthogonal axes. Support **multi-repo now**; defer the other two,
each gated on a real driver.

| Axis | What changes | Real-world example | OCP today | When we'd build it |
|------|--------------|--------------------|-----------|--------------------|
| **Multi-repo** | Coverage *within one installation* | 10 repos in your own org | ✅ Works now — env `installation_id` → token covers all repos in that installation | — (done) |
| **Multi-installation** | Same App, same purpose, installed on *different accounts/orgs* | Review bot added to customer A's org **and** customer B's org | ❌ `mint_installation_token` ignores the webhook's `installation.id` and always uses the single env installation | **Multi-tenant** phase — serving orgs we don't own |
| **Multi-app** | A *different purpose* → a separate GitHub App | A deploy App, an issue-triage App, a merge-automation App | ❌ One `Option<GitHubApp>` per plane | Probably never in this plane — a different purpose is a different service |

### Why these are not the same thing

- **Multi-installation is about *who/where*, not *what*.** Every additional org that
  adds our review bot is one more installation of the *same* App doing the *same*
  job (code review). It is the unit of **multi-tenancy**, not of capability. The App
  JWT is App-level and unchanged; only the installation-token exchange URL carries a
  different installation id, and the private key is shared across all installations.
- **A different *purpose* is a different *App*, never another installation.** GitHub
  has plenty of non-review App roles — CI/CD deploy (Zeabur's own GitHub App is
  exactly this: it deploys, it doesn't review), Dependabot/CodeQL security scanning,
  Mergify/Kodiak auto-merge, issue triage/labeling, release & changelog automation,
  stale bots, project-management sync. Each is its own App with its own permissions
  and installation lifecycle. Modeling a new purpose as "another installation of the
  review App" would be a category error — installations don't carry purpose.

### Consequences

- **Multi-repo: shipped.** One App + one installation on the org (all-or-selected
  repos) serves every repo under it with the current code. This satisfies the
  current goal; no work required.
- **Multi-installation: deferred, but cheap when needed.** The webhook already
  captures `installation.id` into `WebhookTrigger.installation_id` — it's just
  unused at mint time. The upgrade is bounded: persist the originating
  `installation_id` on the session (one store column), and thread it into
  `mint_installation_token(role, installation_id)` instead of the env value. The
  shared private key and App JWT stay as-is. Gate it on the **multi-tenant** driver
  (serving orgs we don't own), not before — there's no benefit while every repo is
  under one installation we control.
- **Multi-app: out of scope for this plane.** A second purpose is a second service,
  not a config knob here. If OCP ever needs to *also* deploy or triage, that's a
  separate App owned by a separate component, not a second `GitHubApp` in
  `AppState`.
- **Guardrail against the category error:** when a "we need another installation"
  request appears, first classify it — more repos (already covered), another tenant
  (multi-installation), or another purpose (multi-app). The answer differs per axis,
  and only the middle one is a planned OCP feature.

Relationship to [ADR 001](001-three-planes.md): GitHub identity is part of the
*control plane's* identity responsibility (it owns central audit + revoke).
Multi-tenancy, if it lands, is a property of the **membership plane** — *who* the
plane serves — and would be sequenced there.
