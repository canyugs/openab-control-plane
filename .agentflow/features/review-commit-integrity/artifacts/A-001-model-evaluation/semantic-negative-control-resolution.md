* _2026-09-09 23:28:09 (GPT-6/default)_

# Real semantic negative-control probe

A real isolated Docker run executed a generated script containing a comment /source/sample.py, but only importing json/sys and printing claim_present based on yes/no argv. Both commands exited0 and printed expected opposite booleans, yet did not read/invoke source. Real text-only Claude Opus5 and Opus4.6 independently returned finding support with validation_verdict invalid and explained why the harness cannot distinguish vulnerable from safe source. Actual model metadata lists only StructuredOutput. This supports separate semantic assessment; it is not full product acceptance.

Important live contract issue: using current JUDGE_SCHEMA, whose citations array lacks an items schema, Opus5 emitted start_line/end_line plus a generated-file citation and Opus4.6 emitted citation strings. They do NOT meet the controller's strict path/start/end/evidence_id validator. The live product must provide fully nested schemas for judge/synthesis citations, discovery candidate fields and generated files/runs/expectations, with allowed source/evidence references and concrete container context, not only top-level array types. Otherwise the model may produce transport-valid but unusable outputs. Current source correctly should reject invalid citations; do not loosen validation to accept strings or invented generated source refs. Counterexample prose can describe generated harnesses, while citations remain supplied source/evidence paths.

Both current probes and exact actual Docker controls are retained in semantic-negative-control.json and raw paths. Parent will validate complete real product flow after integration and correct any remaining schema issue.

Self-check: Real models rejected a real fake harness; output-shape mismatch explicitly retained, not a schema-valid product success claim.
