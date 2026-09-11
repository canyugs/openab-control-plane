* _2026-09-10 01:11:42 (gpt-5.6-terra/high)_

# Binding-contract review

## Spec judgment: PASS

The frozen design and correction contract already make this a routine correction
within the original Design Go, not an owner-only architecture decision. This is
not final acceptance of the current source. The current AST signal is an
authoritative false-negative gate in three places and should be made
diagnostic-only.

## Recommended concrete contract

`executed_reproduced` or `executed_refuted` requires all of the following:

1. A frozen, complete source packet and hash-bound generated plan are retained.
2. The literal baseline and counterexample run only in the isolated OCI
   workspace; both exit zero, have bounded structured actual observations that
   meet the plan, and have different boolean `claim_present` observations.
3. The actual plan, frozen source/evidence bytes, and raw OCI captures go to
   two fresh, isolated judges with no peer output. Each returns a citation-valid
   assessment, `validation_verdict: valid`, and the matching `support` or
   `refute` direction.
4. A fresh anonymous synthesis receives those assessments and execution
   evidence, has the matching direction, and reports no execution conflict.

The outcome remains `provenance: model_assessment`, never human truth or proof
of arbitrary program semantics. The captured plan/source bytes and OCI
observations make the assessment auditable; they do not by themselves prove
that the observed Boolean was caused by the cited source.

The Python AST result may remain recorded as
`controller_source_binding` (documented precisely as a heuristic signal), but
it must not determine validation status, execution direction, run completeness,
classification, or scoreability. A `false` signal is not an execution failure;
a `true` signal is not semantic evidence. The semantic judges, not the
heuristic, decide whether source interaction is meaningful.

This fulfills the already-approved source-interaction intent without requiring
a new deterministic source-interaction proof. The frozen design requires
generated programs to read/import supplied source and the correction contract
places the qualification of whether they actually exercise the cited source and
distinguish the claim with the two independent `validation_verdict`s. Neither
document approves a Python-AST pattern catalogue as an additional promotion
authority.

## Evidence read and conclusion

I read the approved `design.md`, `design-resolution.md`, and
`correction-contract.md`; the current evaluation controller, OCI executor,
documentation, and relevant tests; and the three named runtime artifacts. No
tests or generated programs were run.

The correction contract is explicit: raw OCI controls stay `unproven` until
independent assessment; both valid assessments, passed nonblocked controls,
matching claim observation, and nonconflicting synthesis are required. It also
states that source relevance must not treat a source-path substring as proof.
The design resolution independently requires raw generated files and actual
outputs to be available to fresh judges, while saying source references alone
prove neither truth nor execution.

The current `_python_source_binding_signal` recognizes a limited syntactic set:
literal/bound `open`, selected `Path` reads, selected importlib loading, or a
`sys.path` setup plus direct import. `_run_validation` changes a successful
control result to `unproven` when that signal is false;
`_execution_classification` refuses a direction unless it is true; and `run`
uses it to prevent a complete state.

The supplied `live-omission-1-e456c6d814336027-plan.json` is the concrete
false negative: `oracle.SOURCE_PATH` is passed to helper functions;
`read_source_text(path=SOURCE_PATH)` opens that argument and `load_module`
passes its argument to `importlib.util.spec_from_file_location`; callers invoke
both helpers with `oracle.SOURCE_PATH`. The plan directly reads and imports
`/source/sample.py`, but `live-oci-normalization-failure.json` records
`controller_source_binding: false`. That artifact's OCI failure is separately
an `OCIError` normalization failure with no runs, so it is not evidence that
the helper plan executed or that it is semantically valid.

Conversely, the AST can return true for a harness that opens/imports the source
then prints a constant or argv-derived Boolean. The actual
`actual-adapter-semantic-negative.json` is stronger evidence for the intended
contract: two distinct successful judge invocations independently marked the
comment-path, print-only argv-echo harness `validation_verdict: invalid` and
explained that deleting or correcting `sample.py` would not change its
observations. This preserves rejection without a heuristic gate.

## Necessary and rejected concepts

Necessary:

- Frozen complete source/evidence bytes, plan/generated-file digests, bounded
  raw OCI captures, literal OCI controls, and the current no-tools/OCI
  isolation boundary.
- Mechanical rejection of malformed plans, shell/traversal/links, false
  assertions, nonzero or timed-out controls, output-limit/unstructured-output
  failures, equal/mismatched claim observations, and blocked environments.
- Two fresh independent semantic validation verdicts, citation validation,
  matching judge direction, fresh synthesis, and visible disagreement/conflict.

Rejected as a gate:

- The current Python AST source-binding heuristic. It has repeated real false
  negatives and cannot establish dataflow or meaningful testing on true.
- A source-path string/comment/literal, a successful process, or mechanical
  control differentiation as semantic proof.

Parked new safety ambition:

- Kernel/runtime read tracing could at most establish that a process opened a
  mounted source inode. It cannot show that the bytes influenced
  `claim_present`, distinguish an irrelevant read from a meaningful one, or
  prove the claim. Supporting it reliably would add OCI/runtime architecture,
  portability and capture-policy decisions. It is not an approved E6
  requirement and must not be made a mandatory blocker in this correction.
  It could later be proposed as non-authoritative diagnostic telemetry.

## E6 / E11 / E12 mapping

| Obligation | Retained invariant after correction |
|---|---|
| E6 | Each item remains exactly classified; generated code runs only through OCI. Crashes, assertion-false, failed/mismatched controls, blocked execution, invalid/missing judges, disagreement, or synthesis conflict cannot produce `executed_*`. |
| E11 | No change to OCI-only generated-code materialization, read-only source mount, no network/credentials/host checkout/hooks, or to the prohibition on product-state effects. |
| E12 | Missing/invalid stages remain `partial` or `failed`, never scoreable. Completed artifacts and raw observations remain hash-bound; judge absence/disagreement and synthesis conflict remain visible rather than repaired by a heuristic. |

## Exact correction scope

Only these implementation paths need change:

- `scripts/review_model_evaluation.py`
  - Retain `_python_source_binding_signal`, `_plan_source_binding_signal`, and
    the existing persisted field only as diagnostics; clarify their docstrings
    if needed.
  - In `_run_validation`, do not change a successful, mechanically passed OCI
    result to `unproven` merely because the signal is false. Continue recording
    raw execution, signal, controls, and baseline observation.
  - In `_execution_classification`, remove the
    `controller_source_binding is True` prerequisite. Keep the successful
    controls and consistent boolean-observation prerequisites.
  - In `run`, remove the same heuristic prerequisite from
    `all_validations_complete`; retain successful status and passed controls.
  - `_item` remains the sole promotion point: its existing two valid matching
    judgments and nonconflicting matching synthesis are still required.
- `tests/test_review_model_live_evidence.py` and
  `tests/test_review_model_evaluation.py`
  - Preserve direct-import/open heuristic tests only as diagnostic tests.
  - Add the realistic helper/direct-import plan from the named omission artifact:
    its diagnostic signal may be false, but genuine passed OCI observations plus
    two valid, matching semantic assessments and matching synthesis may reach
    `executed_reproduced`/`executed_refuted`.
  - Retain/add the print-only argv-echo plan with actual mechanically distinct
    observations and the known invalid semantic judgments; it must remain
    `unproven` and unscoreable. Do not use a mock verdict that calls a bogus
    harness valid merely to manufacture promotion.
  - Retain missing-judge, judge-disagreement, synthesis-conflict, failed-control,
    mismatched-observation, and environment-blocked tests.
- `docs/model-evaluation.md`
  - Replace language saying independent verdicts are required “in addition to
    the signal” and “source-bound controls” with the contract above: the signal
    is audit-only, while judges assess meaningful source interaction from the
    retained complete evidence.

No change is recommended to `scripts/review_model_oci_executor.py` or its raw
control semantics. In particular, the OCI normalization failure in
`live-oci-normalization-failure.json` remains a separate approved correction
and must not be reassigned, hidden, or weakened here. No new dependency,
runtime tracer, authority, live model/auth/network/Docker action, or scope
expansion is needed.

## Limits

Two model assessments can agree yet be wrong, correlated, or misread the plan;
they do not become human truth. OCI isolation and mechanically matching
observations establish controlled execution, not arbitrary semantic causation.
Likewise, a runtime read trace would only strengthen a narrow operational fact,
not solve semantic validity. The recommended contract is sound enough for the
approved E6 qualification because it preserves auditable actual evidence and
requires independent explicit semantic review, while honestly leaving these
limits visible.

Self-check: Exact source, authority, scope, model, effort, result and evidence contract frozen.
