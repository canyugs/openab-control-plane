# ADR 007 — Control plugins and OAB Father

Status: proposed · 2026-06-30

## Context

OCP started with one concrete product shape: a PR-review council. That was the
right proof point because it exercises the hard parts of the plane — webhook
triggering, roster fanout, deterministic coordination, liveness, identity, and a
visible side effect on GitHub.

The risk is that the proof point becomes the architecture. PR review has
application-specific concerns: GitHub webhooks, PR diff fetching, verdict comment
format, labels, PAT vs GitHub App installation, and Zeabur template packaging. Per
[ADR 004](004-bot-identity-shared-app-pod-local.md) and
[design scope](../design.md), those concerns should not harden into OCP core.

At the same time, the useful abstraction is now visible:

- OCP core is a **runtime kernel**: sessions, roster, fanout, delivery, state,
  auth, and the `Coordinator` seam.
- A use case such as PR review is a **control plugin**: trigger adapters,
  prompts, coordination policy, tool bindings, secret requirements, and install
  templates.
- Operators need a management surface to create, install, configure, and publish
  those plugins. The working name is **OAB Father**, by analogy to Telegram
  BotFather: it manages bot/control-plugin lifecycle; it is not itself the
  runtime that executes sessions.

## Decision

1. **Keep OCP core as the runtime kernel.** Core owns deterministic guarantees:
   session lifecycle, routing/fanout, admission, durable delivery, auth, audit
   hooks, and the `Coordinator` mechanism. Core does not own PR-review semantics,
   GitHub comment formatting, Zeabur template variants, or model steering.

2. **Define PR review as the first control plugin.** The existing council should
   be treated as `openab/pr-review-council`: a plugin/preset built on top of OCP
   core, not the shape all OCP applications must copy.

3. **A control plugin is a packaging unit, not just a Rust trait.** The plugin
   boundary sits above `Coordinator`. It may include a coordinator mode, but it
   also includes trigger surfaces, prompts, tool/secret bindings, side-effect
   policy, and deployment templates.

4. **Introduce OAB Father as the management layer.** OAB Father creates and
   configures control plugins, binds identities and secrets, installs webhooks,
   validates deployment requirements, and publishes templates. It is an operator
   tool/control surface, not the hot-path orchestrator.

5. **Do not build a plugin platform before the current dogfood path is stable.**
   The GitHub App PR-review council remains the reference deployment. The first
   implementation work should extract the current hardcoded assumptions only
   when they block packaging, installation clarity, or another real plugin.

## Target Shape

```text
GitHub / Slack / Telegram / API / Cron
        |
        v
+----------------------+
| Control Plugin       |
| - trigger adapter    |
| - manifest           |
| - prompts            |
| - coordinator policy |
| - tool bindings      |
| - install template   |
+----------+-----------+
           |
           v
+----------------------+
| OCP Runtime Kernel   |
| - sessions           |
| - roster/admission   |
| - fanout/delivery    |
| - coordinator seam   |
| - auth/audit hooks   |
+----------+-----------+
           |
           v
+----------------------+
| OpenAB Pods / Tools  |
| - chair/reviewers    |
| - gh / deploy CLIs   |
| - pod-local secrets  |
+----------------------+
```

OAB Father sits beside this path:

```text
OAB Father
  |
  +-- create plugin
  +-- configure identities and secrets
  +-- install webhooks / triggers
  +-- bind tools and templates
  +-- publish Zeabur or other deploy artifacts
```

## Plugin Manifest Sketch

A future plugin manifest should be declarative enough for installation and
validation, while leaving runtime policy implementation free to evolve.

```yaml
name: openab/pr-review-council
version: 0.1.0

triggers:
  - kind: github.pull_request
    events: [opened, reopened, ready_for_review]
  - kind: github.issue_comment
    commands: [/review]

coordinator:
  mode: council
  default_preset: lite

actors:
  - id: chair
    role: writer
    required_tools: [gh]
  - id: reviewer
    role: reader

prompts:
  review: scripts/pr-review-trigger-pointer.tmpl

secrets:
  - name: github_app_private_key
    scope: chair
    required_for: github_app

side_effects:
  - kind: github.pr_comment
    actor: chair
    exactly_one: true

templates:
  - zeabur-template-app-1E1Y97.yaml
  - zeabur-template-pat-Z7TQIR.yaml
```

This is a sketch, not the committed schema. The important decision is the
boundary: the manifest describes what must be installed and authorized; OCP core
continues to enforce runtime invariants. See
[ADR 008](008-external-controller-protocol.md) for the proposed external
controller event/action protocol.

## Boundaries

- **Coordinator vs control plugin:** a `Coordinator` decides in-session policy
  (`done`, quorum, prompts, close). A control plugin packages the whole product
  surface around it (triggers, prompts, secrets, side effects, templates).
- **OCP vs OAB Father:** OCP executes sessions. OAB Father prepares and manages
  installations. OAB Father can call OCP APIs, but OCP should not depend on OAB
  Father to run an already-installed plugin.
- **OCP vs pods:** pods consume tools and pod-local credentials. OCP may record
  requirements and enforce identities at the session boundary, but it should not
  become the PR-commenting or deployment CLI runner.
- **Plugin vs template:** a Zeabur template is one deployment artifact for a
  plugin. It is not the plugin itself.

## Consequences

- The PR-review council remains the reference plugin and dogfood path.
- PAT and GitHub App deployments become two install profiles of the same plugin,
  not two product concepts.
- Future work such as release gates, deploy approvals, incident triage, and
  research panels can reuse OCP core without copying PR-review assumptions.
- Documentation should distinguish **runtime setup** (OCP), **plugin setup**
  (control plugin manifest/install profile), and **operator management** (OAB
  Father).
- A future plugin registry or marketplace can publish manifests/templates
  without changing OCP's hot path.

## Deferred

- Final manifest schema and validation rules.
- Whether plugin policy is compiled Rust, external HTTP controllers, WASM, or a
  constrained declarative policy language. ADR 008 proposes external HTTP as the
  first stable boundary.
- A first-class plugin registry.
- OAB Father UI/API shape.
- Migration of the current PR-review implementation into a fully packaged
  `openab/pr-review-council` plugin.

## Rejected Alternatives

- **Make PR review the core product model.** This is simpler short-term, but it
  bakes GitHub and verdict-comment behavior into the runtime.
- **Make every plugin a new OCP fork.** This avoids designing a boundary, but it
  loses the shared guarantees OCP exists to provide.
- **Make OAB Father the runtime.** This conflates installation/management with
  session execution. Keeping it beside OCP preserves a smaller hot path.
