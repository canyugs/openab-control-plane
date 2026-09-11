* _2026-09-09 21:08:49 (gpt-5.6-terra/high)_

# Frozen Stage 2 amendment — executable model evaluation and weekly evidence

## Authority, scope, and sign-off

This is a read-only design for the disposable OCP clone. It replaces only the
conflicting Stage 2 model-evaluation proposal, preserves approved Stage 1 result
`d6e96c3`, its immutable 40-hex target/findings-SHA equality and no-later-head
substitution invariants, and all unrelated source. It authorizes neither source,
config, model/runtime/service action, credential access, Agentflow, commit nor rollout.
The planning model is `gpt-5.6-terra/high`, not a product model choice. Owner authorization
includes the cost of functioning evaluation; measure cost first and optimize later.

Parent sign-off A accepts E-1..E-12. Parent sign-off B accepts the exact implementation
below. Both are required before Design Go; this report itself is not Design Go.

Scope discipline — implement exactly the ask; park everything else as a proposal. The ask's scope is what the user wrote plus tests, commits, the notebook, STATUS, and any records required by the active route. Do not refactor, rename, reformat, add dependencies, or repair adjacent behavior unless the Ask requires it. Pass this paragraph verbatim in every worker brief.

## Amended observable requirements

- E-1 (R-13): one run automatically assesses every supplied finding on one frozen revision;
  its provenance is `model_assessment`, never human correctness.
- E-2 (R-13): two fresh judge invocations with distinct configured model IDs return
  `support|refute|insufficient_evidence`, severity, usefulness, cited evidence and a
  counterexample when applicable. A duplicate ID is a preflight error; same-family use is
  allowed only with a recorded correlation warning. No fallback, cheaper substitution, or
  stage skip is permitted.
- E-3 (R-13): the trusted controller range- and digest-validates every cited source/evidence
  reference; invented or out-of-scope references invalidate that assessment.
- E-4 (R-13): a fresh synthesis sees source evidence, executed evidence, and both valid,
  anonymous judge assessments. It preserves disagreement and may say `unknown`; it cannot
  invent evidence or promote a failed/blocked control.
- E-5 (R-13): a blind fresh discovery invocation sees the same frozen code/diff but no
  original findings, assessments, or author-model identity. Reconciled candidates receive
  two independent judge assessments and reproduction before becoming an
  `automatically_supported_omission`; they are not human-confirmed escapes and imply no recall.
- E-6 (R-13): every item records `static_evidence`, `executed_reproduced`,
  `executed_refuted`, `environment_blocked`, or `unproven`. Generated or supplied checks run
  only in the disposable OCI executor; a crash or an `assert false` is never demonstration.
- E-7 (R-14): trigger-to-terminal-GitHub-projection latency remains based only on captured
  controller/GitHub evidence, with unavailable and reconciliation-upper-bound qualifications.
- E-8 (R-15): retain requested profile/model, CLI version, invocation attempts, trusted CLI
  metadata when available, usage/cost metadata when available, and `unknown` actual totals
  otherwise. No price table, token cap, cache, or budget-based omission is introduced.
- E-9 (R-16): reliable, visible-failure, superseded, timeout and unknown delivery remain the
  existing source-bound, mutually classified operational outcomes, separate from judgement.
- E-10 (R-17): weekly Markdown/JSON shows model-supported/refuted/unresolved findings,
  usefulness/disagreement and validation coverage, automatic omission candidates, distinct
  human-confirmed escapes, their separate denominators/unknowns, and delivery metrics.
- E-11 (R-18 / INV-7): results cannot alter verdicts, retries, roster, GitHub writes, routing,
  checkout, patch, merge, or production state. No fleet, scheduler, dashboard, service or
  Stage 3 council comparison is included.
- E-12: snapshot/input/response conflicts fail visibly; restart never replays a completed
  invocation; all-failed or incomplete model stages yield `partial|failed`, never a score.

## Source facts and frozen boundary

`crates/controller-protocol/src/audit.rs` supplies bounded opaque audit IDs and millisecond
`AuditEventRecord`s. `crates/github-pr-controller/src/store/{sqlite,postgres}.rs` persist
session targets, rounds, findings, `github_writes`, runtime receipts and audit records; most
product timestamps are seconds, while `runtime_event_receipts.occurred_at` is envelope time.
`lib.rs` correlates `action.accepted`/`action.completed`, handles terminal/abandoned events,
and binds a persisted write payload SHA to provider receipts in `perform_write_with_receipt`.
A timeout/supersession can lack a round; `comment_abandon` with null `comment_id` is a
successful no-op, not a visible tombstone. ADR 021/032 keep quality outcomes human-gated.

The weekly bundle remains `review-round-weekly-evidence/v1`: hash-bound raw audit pages and
current product export, capture spans/coverage, Taipei ISO week and `snapshot_at`. It uses
actual `github_writes.payload_json`/receipt SHA for delivery binding, not enqueue audit detail;
never reconstructs historic product state from mutable rows. Reliable precedence is explicit
superseded, then complete verified normal projection, then confirmed integrity/unparseable/
reviewer/timeout diagnostic visible failure, else pending/unknown. Human and cost observations
after cutoff are excluded; incomplete evidence prevents the affected claim. Local output keeps
bounded escaped repo/PR/session/finding/source-row references but omits bodies, tokens and
unneeded free text. These clocks, joins, classifications and zero/unknown categories are
unchanged by model evaluation.

The evaluation creates `git archive <revision>` only after resolving full `revision` and `base`,
rejecting a dirty checkout and changed tracked tree. It hashes archive, diff, findings, evidence,
models file, controller/prompt/schema version and every output artifact. `snapshot.json` is
written before any CLI call. A retry accepts only the same input identity and completed-artifact
hash; a different identity or conflicting duplicate is a hard conflict. The production checkout
is never written.

## One command and concrete source delivery

The future command is:

```text
python3 scripts/review_model_evaluation.py run --repo <clean-repo> --revision <full-sha> \
  --base <full-sha> --findings <findings.json> --evidence <evidence-dir> \
  --models <models.json> --environment <environment.json> --output <new-empty-dir>
```

`findings.json` supplies opaque ID, title/severity, claim, `{path,start_line,end_line}`, and
evidence IDs. `evidence/manifest.json` supplies bounded UTF-8 evidence bytes, digest and
allowed ranges. `environment.json` is optional and may name an existing OCI image and literal
operator checks; it is never required to hand-author checks. The controller derives the default
scope from every tracked regular file in the archive plus the base diff, embeds exact bytes
(UTF-8 or base64) with path/digest in `source-packet.json`, and supplies the same packet digest
to every role. A configured maximum is a hard completeness check: any omitted, unreadable or
over-limit file records its path and makes scope `incomplete`; no result may claim whole-scope
support. Thus a path/archive name alone is never model context. Discovery receives that identical
source packet minus findings/assessments; judge packets contain the original finding and only its
allowed evidence. Separate fresh working directories contain only a role schema and no other
role artifacts. A cloned directory is not asserted to be an OS sandbox or secrecy boundary.

`models.json` has roles `judge_a`, `judge_b`, `synthesis`, `discovery`, and `validation`, each
with `{adapter: codex|claude, model_id, family, strength:"strong"}`. The operator supplies
installed CLI authentication/profile and actual model IDs, not a wrapper. Preflight requires all
roles, two different judge IDs, executable path, `--help` feature probe, bounded packet/schema,
and a digest-pinned OCI image when execution is requested. It records the CLI `--version` and
rejects unavailable or unsupported adapters; it never selects a named "latest" model itself.

## Real adapters, identity, and strict packets

Only these two project-local adapters ship; there is no JSON-v1 endpoint, HTTP transport, or
user-written executor contract. The implementation passes argv directly, never through a shell,
sets a unique empty `sessions/<invocation>` working directory, bounds stdin/stdout/stderr, and
captures raw bytes and redacted process metadata.

```text
codex exec --ephemeral --ignore-user-config --ignore-rules --sandbox read-only \
  --cd <empty-session-dir> --model <configured-model-id> \
  --output-schema <role-schema.json> --output-last-message <final.json> -

claude --print --bare --no-session-persistence --output-format text \
  --json-schema <role-schema.json> --model <configured-model-id> \
  --permission-mode dontAsk --permission-prompts none --tools "" \
  --strict-mcp-config --system-prompt <fixed-role-system-prompt> -
```

The inspected CLIs advertise these flags. The implementation assumes their installed versions
continue to accept them and that Codex `--output-last-message` / Claude print text contain the
schema-conforming final JSON; preflight and live smoke fail visibly if that is false. It parses
only that final JSON against the controller-owned role schema, never a made-up provider envelope.
It records `requested_model_id` from the literal argv and any CLI/provider metadata actually
emitted outside model prose. In these text/final-message modes backend `observed_model_id` may be
unavailable, and is stored exactly as `unavailable`; a requested alias can therefore remain
provider-alias-uncertain. No model-returned `model_id` is trusted. Smoke proves each real CLI
process was invoked with its distinct requested `--model` argv and produced a schema-valid result,
not that a provider alias resolved to a particular backend. Claude is available for family
diversity; two distinct Codex IDs are also supported. The commands make no fallback request.

Every role receives a controller-built JSON packet with `invocation_id`, source/evidence packet
digests and bytes, allowed references, role rules, and one schema. Judge schema is:

```json
{"finding_id":"…","verdict":"support|refute|insufficient_evidence","severity":"…",
 "usefulness":"useful|not_useful|unknown","citations":[{"path":"…","start":1,"end":1,"evidence_id":"…"}],"counterexample":"…"}
```

Discovery schema contains a bounded array of `{claim,path,start,end,evidence_ids}` and no finding
identifier. Synthesis schema contains `{item_id,verdict:"supported|refuted|unknown",
citations,disagreement,reason}`. The controller checks item IDs, paths, ranges, evidence digests
and permitted enum/size limits before an output becomes an assessment. Model prose cannot define
commands, identity, costs, scope, or evidence.

## Generated reproduction and independent candidate validation

For each original finding and each post-discovery candidate, a new validation CLI session receives
only its evidence packet and must return this controller schema:

```json
{"item_id":"…","files":[{"path":"generated/…","utf8":"…"}],
 "runs":[{"name":"baseline|counterexample","argv":["literal","arg"],"cwd":"/work",
 "expect":{"exit":0,"observation":{"key":"value"}},"evidence_ids":["…"]}],
 "claim_observed":"…"}
```

The controller permits relative `generated/` UTF-8 files only, rejects traversal, links, binary
payloads, oversized files, shell interpreters/`-c`, undeclared evidence, missing one baseline and
one counterexample, duplicate argv, or non-structured expectations. It writes accepted generated
files only into a per-item OCI workspace—never the repository or host—and executes the literal
argv arrays there. An optional operator check profile is merged only after identical validation;
it is additional evidence, not a prerequisite.

`scripts/review_model_oci_executor.py` creates a new non-root container with the immutable
archive mounted read-only, a writable tmpfs `/work`, `--network none`, cleared environment,
temporary HOME, no credential/Docker-socket mounts, read-only root, dropped capabilities and
`no-new-privileges`; it records image digest, argv, stdout/stderr, timeout and structured
observation. Docker being installed but its daemon currently stopped is a later implementation
validation setup issue: preflight records `environment_blocked`, never omits the executor.
No generated shell string runs on the host. `executed_reproduced` requires the claimed behavior
in baseline and the declared differing control observation in counterexample; an ordinary crash,
false assertion, or blocked execution is `unproven`/`environment_blocked`.

Blind discovery is reconciled with originals by normalized claim plus overlapping path/range,
retaining an explicit duplicate relation rather than silently deleting it. Candidate `judge_a`
and `judge_b` are fresh calls after discovery, see no discovery rationale, and each gets the
same packet and generated-test results. Candidate synthesis sees both validated assessments and
controls. Therefore discovery never self-confirms, and all synthesis records both independent
candidate assessments.

## Artifacts, future paths, and acceptance

The immutable output tree is `snapshot.json`, `run.json`, `source-packet.json`,
`invocations/<id>/{packet.json,argv.json,raw.stdout,raw.stderr,final.json,result.json}`,
`validation/<item>/{plan.json,generated-file-digests.json,runs/*.json}`, `findings.json`,
`omissions.json`, and `summary.{json,md}`. Generated contents are materialized only in the OCI
workspace then discarded; retained plans record paths/digests and raw model artifacts remain
auditable. `run.json` records state, retry lineage, completeness, requested/observed
identity status, usage/cost status and artifact hashes. A later weekly invocation adds
`--evaluation-root` to the frozen offline report and exposes evaluation availability separately
from the controller delivery cohort; it never invokes a model.

- `scripts/review_model_evaluation.py` — command, immutable snapshot, packets, orchestration,
  schemas, restart and render.
- `scripts/review_model_adapters.py` — the exact Codex and Claude adapters and trusted metadata.
- `scripts/review_model_oci_executor.py` — generated-file validation and literal OCI execution.
- `scripts/review_round_weekly_report.py` — frozen source joins plus evaluation aggregation.
- `tests/test_review_model_evaluation.py` and `tests/test_review_model_oci_executor.py` —
  synthetic positive/negative controls, fake CLI transcripts, schema/isolation/restart/security
  tests and Docker command construction without a daemon.
- `tests/test_review_round_weekly_report.py` — source clock/join/receipt controls and separate
  model/omission/human/unknown denominators.
- `tests/fixtures/model_evaluation/` — synthetic Git repository, model transcripts and controls.
- `docs/model-evaluation.md` and `docs/review-round-weekly-report.md` — input schemas, CLI
  assumptions, disclosure/identity limits, OCI preflight, recovery and report definitions.

Acceptance builds synthetic vulnerable and safe snapshots. It proves all required stages run,
judges receive equal source bytes but no cross-role output, discovery has no originals, candidate
judges are independent, invalid citations/plans fail, generated baseline/control distinguishes
the fixtures, OCI has literal argv/no network/no credentials, and failed stages cannot score.
It proves Stage 1 and weekly source classifications remain observational and intact. A later real
PTY smoke, with two configured distinct judge IDs and selected adapters, runs the one command
from snapshot/findings/models/environment through generated evidence, discovery and
weekly-compatible artifacts; raw argv/version/result prove real adapter invocation while
observed-identity limitations remain visible. It does not automate provider login.

Remaining limits: models can be correlated, wrong, unavailable, or provider-alias-uncertain;
agreement is neither correctness nor calibrated confidence. OCI isolation is an operational
executor boundary, not a promise about arbitrary host clones. Static-only and blocked results
remain unresolved. Efficiency tuning, fleet operation, human adjudication and production use are
future proposals.

Self-check: Frozen scope, output, model, effort and authority declared.
