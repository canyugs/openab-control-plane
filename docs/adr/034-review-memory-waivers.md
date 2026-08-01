# ADR 034 — Review memory: a waiver ledger applied at synthesis, not at recall

Status: proposed · 2026-08-01

## Context

The council's experience loop runs in one direction only: misses become
steering probes (`docs/steering/pr-review.md`, verified by the ops
`m4-recall.py` harness — ADR 030 lineage). Nothing records the opposite
decision: trade-offs the org has already accepted. The consequence is that
every new PR re-litigates known compromises — the same noise regime that made
CodeRabbit's chatter ignorable, arriving from the other side. CodeRabbit's
answer to this is its "learnings" feature; we have no equivalent.

The raw material already exists and is currently discarded:

- **OVERRIDDEN merges** (final round said `request_changes`, a human merged
  anyway — flagged by the ops retro tooling): each one is an explicit,
  recorded human disagreement with the council that today changes nothing
  about the next review.
- **Accepted disputes** (`@bot review` with refuting evidence, council
  concedes on re-review): the conclusion lives only inside that PR's thread.
- **Adoption data** (ADR 021): finding classes that are consistently ignored
  are candidates for demotion, not stronger enforcement.

Two constraints shape where memory may live. First, quorum independence
(ADR 030's finder/verdict separation): anything that pre-suppresses reviewer
output degrades the recall the multi-model roster exists to provide. Second,
the untrusted-input boundary (ADR 019): PR authors must not be able to teach
the reviewer to ignore their own defects.

A static variant — waiver entries appended to the steering doc and
pre-seeded to every pod (ADR 003 channel) — was considered and rejected:
org-wide entries burn every reviewer's context on every session, go stale
until pod restart, pollute the constitution's measurement (`m4-recall.py`
asserts steering carries no incident-specific content), and inject
suppression at the recall side.

## Decision

Memory is a **precision filter at the output, not a recall suppressor at the
input**. It is implemented as a kernel-owned waiver ledger, injected by the
controller into the chair's context at convene, applied and cited by the
chair at synthesis, and validated by the controller at close.

1. **Kernel: waiver ledger, sister to the ADR 020 findings ledger.**
   `waiver { id (W-<n>), repo, scope (path glob and/or finding-class),
   reason, source (PR#/dispute/override), approved_by (a human),
   created_at, expires_at (mandatory), status (active|expired|revoked) }`.
   North API: `GET /v1/review/waivers?repo=&paths=&active=1` readable by
   bots; `POST`/`DELETE` require the operator key. Expiry is enforced by the
   plane — expired entries are simply not returned. Revocation keeps an
   audit row. A per-repo cap on active waivers forces curation. The findings
   ledger gains `waived_by` for downstream joins.

2. **No GitHub-facing surface can create a waiver.** Not `@bot` commands,
   not PR content, not any bot identity. Writes are operator-key only. This
   is the memory-poisoning line, and it is structural rather than
   procedural (ADR 019 posture).

3. **Controller: inject at convene, chair-only.** In external mode the
   controller authors the session tasks; it resolves the PR's changed files,
   queries the waiver API, and includes matches in the **chair's task
   payload only**. Reviewer tasks are untouched — reviewers never see
   waivers. The session record stores the injected waiver-id set, so "what
   the chair saw" is reproducible.

4. **Chair contract: a waiver changes a finding's form, never its
   visibility.** Suppressing or downgrading against a waiver requires citing
   its id, and the annotation appears in the PR comment:
   `F3 🟡→⚪ waived (W-12: <reason>, expires 2026-09-15)`. The author sees
   the trade-off and its expiry; the audit trail is the PR itself. Silence
   remains the violation.

5. **Controller: validate at close, fail toward noise.** Every waiver id the
   chair cites must exist and be active; otherwise the finding is processed
   at its original severity. If the waiver API is unreachable at convene,
   the council runs without memory. Memory failure degrades the system to
   *noisier*, never to *blinder*.

6. **One-way reference between memory and constitution.** The steering doc
   never cites waivers. A waiver that keeps recurring and deserves
   permanence goes through the front door: abstracted into a probe class,
   recall-verified, landed in `pr-review.md`. Waivers are the temporary
   layer; the constitution is the permanent one.

Intake and measurement stay human-gated and live in ops (ADR 021 posture:
tools report, humans decide): the weekly quality report's OVERRIDDEN/dispute
list is the intake queue; approval becomes one API call. The same report
measures waiver hit rate, lists soon-to-expire entries, and performs
**masking detection** — an escaped defect landing on a waived path flags
that waiver for mandatory re-review. Compromises get verified too.

## Consequences

- The chair's synthesis gains a deterministic input (active waivers for the
  touched paths) and a new obligation (citation format); steering's Chair
  section grows a few lines. Reviewer behavior and prompts are unchanged.
- Re-litigation noise drops without lowering recall: the original findings
  still exist in the session record and the PR comment, in waived form.
- The org acquires an explicit, expiring, auditable inventory of accepted
  risk — readable in one API call. That inventory is sensitive; waiver
  CONTENT stays in the plane DB and the private ops repo, while this repo
  (public) carries only the mechanism.
- Expiry pressure is real: when a waiver lapses, the finding returns at full
  severity on the next touch of that path. That is the designed nudge to
  either fix the debt or consciously re-approve it.
- New failure surface: a stale waiver can mask a real regression until
  masking detection or expiry catches it. Bounded by mandatory expiry, the
  per-repo cap, and escape-triggered re-review.
- Depends on: ADR 020 ledger (schema sibling), external-mode
  controller-authored tasks (injection point), and the ops weekly quality
  loop (intake queue). Sequencing: kernel ledger → controller
  inject/validate → steering contract → ops intake tooling.
