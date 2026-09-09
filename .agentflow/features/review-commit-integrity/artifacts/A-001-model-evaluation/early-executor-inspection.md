* _2026-09-09 22:30:46 (GPT-6/default)_

# Early real OCI inspection (implementation still active)

Observed current worker clone Oh8luI, not a final source verdict. Real Docker call to current OCIExecutor returned container_failed: docker run has no -i, so JSON plan stdin is not attached. A host test wrapper adding literal -i only (no source edit) allowed both generated programs to execute in OCI.

Negative evidence control: generated/check.py imports only json/sys and prints argv as a JSON value, never opens or invokes immutable source. Baseline and counterexample use differing constant arguments; current executor classified this executed_reproduced. This violates E-6's requirement for reproduced code behavior rather than arbitrary harness output. Both raw result objects are in /var/folders/sd/lyvwlbld52j4b4bptd8yfr9w0000gn/T/ocp-evaluation-live-i4bgehnf/early-executor-result.json and early-executor-stdin-result.json. Correct final workflow must preserve execution-versus-semantic-validation distinction and reject this fixture as proof; two independent judges/synthesis need full generated plan and observed controls to validate claim relevance, with explicit controller promotion rules. Mere nonempty source refs or different stdout isn't proof. Positive real source behavior must still become reproduced when evidence supports it.

Additional inspection candidates (verify final implementation before judging): process.communicate accumulates unbounded output before checking limit; timeout Docker CLI termination does not remove surviving named container; successful JSON runs retain only output hashes, not raw stdout/stderr required for audit. These are same approved untrusted execution/retained-evidence boundary, not added scope.

Self-check: Real probe and exact negative control recorded; active worker source untouched; final conformance pending.
