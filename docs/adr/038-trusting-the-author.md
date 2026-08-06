# ADR 038 — Trusting the author: dismiss, waive, and where a review decision lives

Status: proposed · 2026-08-07

Builds on: [ADR 013](013-decision-review-state.md) (the counts are the decision),
[ADR 019](019-untrusted-pr-input-boundary.md) (untrusted PR content),
[ADR 020](020-review-audit-effectiveness-ledger.md) (findings ledger),
[ADR 021](021-review-effectiveness-feedback-loop.md) (adoption over counts),
[ADR 025](025-degraded-council-status-notice.md) (silence is the violation),
[ADR 031](031-provider-neutral-kernel.md) (the controller owns ingress and
every GitHub write), [ADR 035](035-review-memory-waivers.md) (waiver ledger —
**amended here**), [ADR 036](036-first-party-investigation-journal.md) (audit
journal and causal correlation).

## Context

Since CodeRabbit was retired on 2026-08-01 the council is the org's only code
reviewer. The instinct that follows — hold the gate harder, because there is
no second net — is wrong, and this ADR records why.

A reviewer that cannot be overruled is not safer. It gets routed around, or
switched off, and then there is no net at all. Adoption is a safety property,
not a comfort. The correct posture is the opposite of a gate: accept the
author's judgement immediately, and make the record complete enough that the
decision can be audited afterwards.

Today an author has no way to say either thing:

- **"Your finding is wrong."** The findings ledger has a `dismissed` status.
  On prod it holds 2 rows out of 1099 (883 open, 214 resolved). That is not
  evidence the council is precise; it is evidence that **nobody can say it**.
  ADR 021's adoption loop and ADR 032's calibration both assume this signal
  exists. It does not.
- **"It is a real defect and we accept it."** ADR 035's waiver ledger exists
  and works — dev has fired one — but the only write path is an operator
  running a signed request from the ops repo. Prod has **zero** waivers since
  the ledger shipped.

The concrete trigger: `zeabur/backend#2382` finding F1, a 🔴 SSRF-via-DNS-
rebinding claim the author argues is not a defect at all. Under today's rules
the repo cannot declare it away (the boundary floor forbids waiving security),
a Review Contract cannot either (same floor), and the only remaining lever is
an operator waiver — a path the author cannot reach, for a claim that is not
even about accepting risk.

ADR 035 forbade this deliberately: "No GitHub-facing surface can create a
waiver … this is the memory-poisoning line." That line was drawn before
ADR 031 gave the controller sole ownership of ingress, and before the
org-membership trust probe that replaced the unreliable `author_association`
field. It conflated **GitHub-facing** with **untrusted**, which are no longer
the same thing.

## Decision

1. **The default is trust.** A verified org member's judgement is accepted
   immediately — including on 🔴 security findings. No approval queue, no
   second signature, no severity exception. **Auditability comes from the
   record, not from blocking.**

2. **Two verbs, because they are two different claims.** The ledger already
   has both values; only the vocabulary was missing.

   | Claim | Verb | Ledger | Expiry | Feeds |
   |---|---|---|---|---|
   | "This is not a defect" | `dismiss` | `status=dismissed` | none — it judges one finding, it is not a standing suppression | precision measurement (ADR 021 / 032); repeated same-class dismissals are a steering defect, not a user problem |
   | "It is a defect and we accept it" | `waive` | `status=waived` + repo-scoped waiver | **mandatory** | the boundaries evidence loop |

   Conflating them corrupts both datasets: a false positive recorded as an
   accepted risk understates a precision problem, and a real trade-off
   recorded as a false positive loosens steering that was right.

3. **Three homes for a review decision.** A decision's lifetime determines
   where it lives:

   | Nature | Home | Expiry |
   |---|---|---|
   | Rule, architectural decision, standing convention | the repo's `Review Boundaries` section | **none** — changing it goes through that repo's review |
   | This one PR's framing | Review Contract in the PR body | ends with the PR |
   | Temporary acceptance across PRs | waiver ledger | **required** |

   A waiver renewed twice is a rule wearing a waiver's clothes: promote it to
   the boundaries file and revoke the waiver. **Expiry is therefore not only
   garbage collection — it is the classifier** that separates a genuine
   temporary trade-off from a policy that was filed in the wrong place.

4. **The verdict is recomputed, not re-litigated.** ADR 013 already holds
   that the counts decide, and the controller already overrides a chair whose
   word disagrees with its own numbers. Accepting a dismiss/waive therefore
   applies an existing rule to an updated ledger:

   update the finding's status → recompute open counts for the **current head
   SHA** → if nothing blocking remains, rewrite all three GitHub artifacts:
   the verdict comment (the finding moves to a visible `Dismissed`/`Waived`
   row), the `openab/council` commit status, **and a new formal review**.

   The third is not optional bookkeeping: a GitHub `REQUEST_CHANGES` review
   persists until dismissed or superseded, so it is the artifact that actually
   unblocks the merge. This path only ever downgrades blocking; it can never
   turn an approve into a blocker.

   **The recomputation is a compare-and-swap on the reviewed head, not on
   "whatever is current".** A decision names a finding, and every finding
   carries the head SHA it was raised against. If the PR's head has moved
   between the command and the write, **no artifact is rewritten** — a stale
   decision must never unblock code nobody reviewed.

   Nothing is lost when that happens, because the push has already produced a
   new round against the new head: a `waive` is repo-scoped and durable, so it
   applies to that round on its own; a `dismiss` judges one finding on one
   revision, so if the new round raises the same class again the author
   dismisses again — and that repetition is itself the precision signal
   (point 2). The reply says which of the two happened; silence would be the
   violation (point 7).

   Re-convening a full round to reach the same conclusion is rejected: it
   spends three agents to derive what the ledger already knows, and delays the
   unblock the author asked for.

5. **This amends ADR 035 #2.** The line moves from *"no GitHub-facing
   surface"* to *"no unauthenticated surface"*:

   - the command is parsed by the **controller** from the signed webhook
     payload — **never by an agent reading PR text**, so there is no prompt
     to inject;
   - authority is checked **server-side against the repository**, not taken
     from the payload's self-reported `author_association`;
   - the acting identity is recorded before anything changes.

   **The bar is write permission on the repository, not org membership.**
   Waivers are repo-scoped, so org membership is too coarse: a member with no
   access to a repo could otherwise silence its reviews. The minimum is
   `admin`, `maintain` or `write` from
   `GET /repos/{owner}/{repo}/collaborators/{login}/permission`. The rationale
   is that accepting a risk in a repo should require the same standing as
   landing code in it — someone who can push the change can accept its
   trade-off; someone who cannot should not be able to silence its review.
   The org-membership probe (SEI-884) remains the gate for *triggering* a
   review; it is not sufficient for *deciding* one.

   Residual risk, stated plainly: a compromised org-member account can now
   create waivers where previously it could not. It is bounded by mandatory
   expiry, listed in the periodic report, revocable, and attributable. That
   is a better trade than a tool nobody uses.

   ADR 035's kernel-owned ledger and `GET /v1/review/waivers` north route are
   separately superseded by the SEI-895 move of both ledgers to the controller
   (v0.1.68/0.1.69); the operator-key write path remains valid and this PR
   path is additive to it.

6. **Everything is recorded (ADR 036).** Required events: `finding.dismissed`
   and `finding.waived`, each carrying the **actor** (server-verified GitHub
   login), the reason text, the finding identity, and the counts before and
   after. The GitHub writes that follow are linked by `caused_by`, so the
   chain *comment → decision → recompute → three writes* is one query rather
   than a reconstruction. Today `waiver.created` records `created_by` inside
   its detail payload but leaves the audit **actor** unset; that gap closes
   here.

   Nothing is deleted. A dismissed or waived finding keeps its row and its
   history; only its status changes. Trusting the author never means
   pretending the finding did not happen.

7. **The author's prose never enters a prompt.** An operator-written waiver
   is injected into the chair's context as free text, and its block header
   tells the chair the entries are "recorded by the operator — never sourced
   from PR content". An author-written waiver would make that sentence false
   and turn the ledger into a durable, repo-wide, auto-injected channel for
   PR-authored text — up to 180 days of it, which is a different class of
   exposure from the single round an `ask` or `review <notes>` survives. That
   is ADR 035's memory-poisoning concern in its real form: not *who may
   create* a waiver, but *what the chair is told and by whom*.

   The split that resolves it: **a command-created waiver names a finding,
   and the finding's own title, path and severity are council-authored.**
   Those are what get injected:

   ```
   - W-12 [apps/backend/src/lib/agent/cost/]: "Forged sentinel bypasses
     safeDiagnostic" (waived by @author-login, expires in 83d)
   ```

   "Council-authored" means *attenuated*, not *clean*: the path is taken
   verbatim from the diff and is fully author-controlled, and the title is a
   model summary of author-controlled code, so both carry indirect influence.
   The controller therefore length-bounds and normalizes them before
   injection — truncate the path and title, strip control and non-printable
   sequences, keep them on one line. This bounds surface area; it is not a
   content filter, and it is not asked to be one.

   The author's reason is stored in the ledger, carried in the audit event,
   quoted in the PR reply and listed in the periodic report — every
   human-facing surface, and no model-facing one. Matching also gets *more*
   precise, because it keys on the finding the council actually raised rather
   than on someone's prose about it.

   **This bounds the durable channel, not every channel.** The rule above is
   about the waiver ledger: repo-wide, auto-injected into every future round,
   for up to 180 days. A dismissal's reason inside *its own pull request* is a
   different thing — scoped to one PR, gone when the PR closes, and **already
   readable by the chair**, which has PR read tools and the dismissal is a
   comment on the thread. Injecting it there is not a new door; it makes an
   open one deterministic instead of leaving it to whether the model happened
   to look. Abstinence would not keep the text away from the model, it would
   only make its arrival unreliable.

   Operator-created waivers (the ADR 035 path) have no finding to key on and
   keep their free-text field; for those the header's claim stays true. The
   injected block must therefore **label each entry's provenance** — operator
   or author — rather than asserting one source for all of them.

8. **Silence remains the violation (ADR 025).** Every command is answered:
   accepted (what changed, the scope, the expiry, counts before and after, how
   to revoke), accepted-but-still-blocked (what remains), or refused (why).
   The scope line is not decoration — an author typing `waive` is usually
   thinking "stop bothering me on this PR" while the system is recording a
   **repo-wide** acceptance, and the reply is where that gap becomes visible.

## Scope of the first implementation

Points 1–8 describe the destination. They are **not** a precondition for the
first release, and this section exists so the ADR cannot be read as one: a
feature nobody can use yet cannot be iterated on, and today an author on a PR
can do nothing at all.

**v1 ships `dismiss` alone.** It is the verb the triggering case actually
needs (`backend#2382` F1 is "your finding is wrong", not "we accept this
risk"), and it carries almost none of the design load: no expiry, no repo
scope, no injection into future prompts, and therefore none of point 7's
provenance question and none of the periodic report.

In v1: the **repo-write permission probe** from point 5 (`admin`, `maintain`
or `write`, failing closed on a probe error) — see below for why it did not
stay deferred; the head-SHA equality check from point 4 — a single comparison
that prevents unblocking code nobody reviewed; the status update; the
recomputation and the three artifacts; the audit event with actor and reason;
and the reply. The last two are not guards to be traded away — the record is
what makes trusting the author defensible (point 1), and the reply *is* the
feature's user experience (point 8).

**And the within-PR teaching loop.** Unblocking a verdict without teaching
anything is treating the symptom: the next round on the same PR would re-raise
what the author just answered, which reads as the tool forgetting. So when the
controller opens a round it includes the PR's own dismissed findings, with the
author's stated reason, in the chair's task — **framed as the author's claim,
not as fact**:

> The author dismissed F1 ("SSRF via DNS rebinding…") as not a defect, saying:
> "…". Weigh that argument. If it does not hold, raise the finding again and
> say why it does not hold.

The wording deliberately mirrors the Review Contract guard already in
steering — *a contract can launder a real defect; verify it is honest* — so a
dismissal informs the council without being able to silence it. Same bounds as
point 7 apply to the injected text (length-bounded, normalized, one line).

Teaching has three loops and they are not alternatives, only different time
constants: **this PR** (above, automatic), **this repo** (`waive`, then
promotion to the boundaries file — v2), and **every repo** (steering and the
recall probes, corrected from the precision data that `dismiss` finally
produces). Each is fed by the one below it, which is ADR 021's calibration
position with the bottom layer finally supplied.

Deferred: `waive` entirely, and with it expiry, repo scope, prompt injection,
provenance labelling and the tidy-up report; and promote-to-boundaries.

The repo-write probe was on that list and came off it during implementation
(council review of the implementation PR). The deferral had been justified on
usability, and that reasoning does not survive contact with the actual users:
anyone who works on a repository already has write, so the check is invisible
to every legitimate caller. What it prevents is not: an org member with
read-only access to one repository could otherwise make the controller post an
APPROVE there — **merge authority GitHub itself would deny**. A guard that
costs nobody anything is not a usability tax, and "ship it usable first" is
not a reason to ship it wrong.

## Alternatives considered

- **A security floor (🔴 not waivable), matching the boundaries and Review
  Contract channels.** Rejected. It is consistent, but it removes the lever
  exactly where disputes concentrate, and an unusable gate is bypassed or
  disabled. The floor also mis-frames the common case: most security disputes
  are "your finding is wrong", not "we accept this risk", and no floor should
  stop a human from correcting a model.
- **A two-person rule for 🔴 waives.** Rejected at this org's size — it turns
  a five-second correction into a scheduling problem. Revisit if the audit
  record shows the trust is being abused; the record is what makes that
  question answerable.
- **Per-PR silencing instead of repo-scoped waivers.** Rejected: it leaves no
  cross-PR trace, which blinds exactly the adoption data ADR 021 depends on.
- **Keeping ADR 035's operator-only write path.** Rejected as the sole path:
  it puts an operator in the loop of every author's disagreement, which is why
  prod has zero waivers.

## Consequences

- The missing adoption signal appears. ADR 021's loop and ADR 032's
  calibration get real data, and `docs/review-boundaries.md` can finally be
  grown from evidence instead of speculation — its own instruction.
- Waivers accumulate and must decay. Expiry does the bulk; a periodic report
  handles what expiry cannot decide (never fired, fires every round, path no
  longer exists, expiring soon).
- 🔴 waives are permitted but conspicuous: a shorter maximum expiry and a
  separate listing in that report.
- The command path becomes a security-relevant surface. Two things keep it
  safe and both must hold: it stays **controller-parsed** (the day an agent is
  allowed to act on the command, ADR 035's memory-poisoning line is breached
  for real), and **no author-written prose is ever injected** (point 7) — the
  ledger must not become a durable prompt channel just because the write path
  opened. Free text from a PR already reaches a model through `ask` and
  `review <notes>`, but only for one round; a waiver lasts up to 180 days and
  applies to every future round in the repo.
- Precision problems stop hiding as author friction. A finding class that is
  dismissed repeatedly is now visible as a steering defect, which is the
  cheaper thing to fix.
