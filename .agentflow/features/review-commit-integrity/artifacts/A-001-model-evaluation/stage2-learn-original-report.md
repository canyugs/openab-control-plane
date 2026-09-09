* _2026-09-10 01:44:57 (gpt-5.6-luna/max)_*

# Stage 2 learn report

This is a bounded, read-only retrospective of the accepted Stage 2 implementation and recorded runtime evidence. The task checkout/source SHA is `1d64a12d039b55b02041964ba1f3041ade0f735f`. The product implementation SHA is `6e5784bdd19e5e84ccc19637e67de643e6d65920`; the acceptance/review-record SHA is `4b46e16c75430f9654b18599e319f978d5b0c3ac`. Design Go was `1107f73567710159c298ff6a60df393e8e6b9775`. Security was PASS at the implementation SHA; acceptance was PASS for Outcome, Minimality, Conformance, and Overall; the host gate was PASS for the acceptance record. No Result Go or deployment claim is made.

## Shipped corrections and evidence-bound lessons

- Mock transport assumptions were insufficient. Actual OAuth CLI output used event-array envelopes ending in a result object, and transport metadata carried requested/observed identities plus auxiliary model usage. The adapter had to preserve those envelopes and identities instead of inferring them from mocks; list-price estimates and provider billing were kept separate.
- Normalized model plans needed an end-to-end core-to-executor seam. The recorded red run failed with `OCIError` even though the plan was semantically valid; executor-local normalization then accepted canonical byte/sha256 metadata, and the later real OCI journey passed. Shape/unit agreement alone did not establish seam compatibility.
- AST/source-text signals are diagnostic, not semantic authority. A real generated negative-quantity plan had `controller_source_binding: false` while controlled OCI observations and both semantic judges validated it; the actual argv-echo negative was rejected as invalid. Executed classification therefore retained controlled observations and independent semantic judgments.
- Disagreement is a typed metric: count the typed disagreement numerator only among eligible paired judgments, and expose that eligible denominator. Synthesis prose is not a boolean signal, and evaluation count is not the denominator. The corrected evidence reports `0/3` and `0/1` for the vulnerable and safe journeys.
- Nested Git boundaries must be settled before source snapshots. The corrected submodule probe recorded `marker_executed: false`, refusal `repository contains submodule gitlinks`, and unchanged configuration; the source snapshot path therefore does not rely on nested helper execution.
- Partial failures and unknowns are evidence, not blanks to fill. Failed pre-schema/generation attempts remained retained and unscoreable; weekly output preserved partial/unknown session states, human coverage stayed unknown, and `actual_cost_usd` stayed `unknown` while list estimates remained estimates.

## Optional future lessons, not shipped changes

The accepted actual runs used two distinct model IDs from one provider family, so correlation remains an observed limitation. The synthetic vulnerable/safe journeys demonstrate executable workflow, controls, and artifact integrity, not production accuracy or human truth. These are recorded limits only; this closeout proposes no new architecture or repair.

Evidence used: the current Stage 2 notebook, actual journey report and PTY record, security and acceptance reports/resolution, design/transport/binding resolutions, normalization and disagreement correction reports, the corrected submodule-boundary record, and the actual semantic-negative record. This learn pass ran no new tests or live calls; it wrote only this workflow record.

Self-check: Exact source, authority, scope, model, effort, result and evidence contract frozen.
