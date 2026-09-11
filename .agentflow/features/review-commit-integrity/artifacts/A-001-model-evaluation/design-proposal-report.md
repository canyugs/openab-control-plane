* _2026-09-09 21:03:53 (gpt-5.6-terra/high)_

# Stage 2 amendment and replacement design — automatic review evaluation

## Authority and two sign-offs

This is a read-only design proposal. It replaces the prior Stage 2
three-file, no-model-call/human-prerequisite design; it preserves approved Stage 1
result `d6e96c3` and all unrelated source. It authorizes no source work, model run,
network/live-service access, credential use, Agentflow action, commit, or rollout.
The planning worker model is `gpt-5.6-terra/high`; it is not a product evaluator choice.

Parent sign-off A — requirements amendment: accept E-1..E-12 below as the owner-authorized
scope, superseding only the conflicting Stage 2 portions of the prior design/resolution.
Parent sign-off B — implementation specification: after A, approve the exact paths,
contracts, controls, and acceptance plan below before any source work. A is not Design Go.

Scope discipline — implement exactly the ask; park everything else as a proposal. The ask's scope is what the user wrote plus tests, commits, the notebook, STATUS, and any records required by the active route. Do not refactor, rename, reformat, add dependencies, or repair adjacent behavior unless the Ask requires it. Pass this paragraph verbatim in every worker brief.

## Amended observable requirements

- E-1 (R-13 replacement): one invocation automatically assesses every supplied finding
  against a frozen local revision and explicit evidence; no human annotation is needed to
  finish. Its provenance is `model_assessment`, never human ground truth or correctness.
- E-2 (R-13 extension): two fresh, independently prompted evaluator sessions with distinct
  configured `model_id`s assess each finding as `support`, `refute`, or
  `insufficient_evidence`, with severity, usefulness, exact evidence/counterexample, and
  references. Strong profiles are required; prefer distinct families and record an allowed
  same-family correlation. Duplicate/cheap substitutions fail preflight, never silently run.
- E-3 (R-13 extension): validate every returned file/line range and evidence ID against the
  immutable manifest; reject invented references and mark that assessment invalid.
- E-4 (R-13 extension): a separate fresh synthesis session receives evidence plus both
  validated assessments, not evaluator identities or original-author model identity. It
  preserves disagreement, can say `unknown`, and cannot fabricate evidence or turn a failed
  check into support. Evaluators never receive synthesis output.
- E-5 (R-13 extension): a separate fresh discovery session sees the same frozen code/diff,
  but no supplied findings or judge output. It proposes scoped omission candidates; the tool
  then deduplicates/reconciles them and independently validates/reproduces them before
  labelling `automatically_supported_omission`. They are not human-confirmed escapes and make
  no recall claim. Discovery is first-delivery functionality, not a savings TODO.
- E-6 (R-13 extension): every finding/candidate has static and execution evidence state:
  `static_evidence`, `executed_reproduced`, `executed_refuted`,
  `environment_blocked`, or `unproven`. Relevant declared checks execute in a disposable,
  credential-free, no-network environment; blocked/failed checks cannot pass by inference.
- E-7 (R-14 unchanged in meaning): the weekly output derives accepted-to-terminal-projection
  latency only from existing controller/GitHub evidence, retaining unavailable/upper-bound and
  clock qualifications. Model judging is not its clock source.
- E-8 (R-15 amended): record per-invocation model/profile/source, attempts, usage supplied by
  that provider, and reconciled actual cost where supplied; otherwise state actual total is
  unknown. No price table, token budget, cache, or stage skip is introduced to save money.
- E-9 (R-16 unchanged in meaning): reliable, visible-failure, superseded, and unknown remain
  the prior source-bound controller/GitHub classifications, separate from model assessment.
- E-10 (R-17 amended): weekly Markdown/JSON automatically consumes machine outputs and shows
  supported/refuted/unresolved findings, usefulness coverage, disagreement, test-validation
  coverage, and automatic omission candidates; optional human ground truth is separately named.
- E-11 (R-18 unchanged): no evaluation result can issue a GitHub write, alter a verdict,
  retry, roster, routing, patch, merge, or production checkout. No scheduler/dashboard/fleet
  or Stage 3 council-versus-independent experiment is included.
- E-12 (new operational integrity): identity/content conflicts fail visibly; completed effects
  survive restart without replay; all-failed/incomplete judging yields a failed/partial run,
  never an apparently valid quality score.

## Source facts, boundary, and assumptions

`controller-protocol/src/audit.rs` supplies validated, millisecond `AuditEventRecord`s and
cursor pages. `github-pr-controller/src/store.rs` is the controller's durable boundary:
session targets, rounds, findings, outbox, runtime receipts and audit journal. SQLite/Postgres
persist product timestamps mostly in seconds; `runtime_event_receipts.occurred_at` is envelope
time. `lib.rs` emits correlated `action.accepted`/`action.completed`, records terminal rounds
and findings, and `perform_write_with_receipt` binds persisted payload SHA to provider receipts.
Thus existing data/clock/join/receipt corrections remain the weekly-report input contract.

`dispatch_terminal` and `dispatch_abandoned` prove a round need not exist for timeout/supersede;
`comment_abandon` with null `comment_id` is a successful no-op, not a visible tombstone. The
current chair/reviewer templates are live council prompts and controller is the only GitHub
side-effect owner. ADR 021/032 call their quality signals human-gated/adoption-oriented; this
amendment adds offline model evidence only and does not auto-tune behavior or recast it as truth.

Search found agent profiles/OpenAB pod commands and deployment credentials, but no local model
executor, generic model API client, or safe evaluation runner. The product therefore adds an
offline Python-stdlib command boundary; it does not reuse `/Users/can`, Agentflow, pod configs,
the controller outbox, or live-service credentials. Assumptions: the caller has a clean Git
checkout containing the requested commit; configured providers can satisfy the contract below;
and a digest-pinned OCI runtime/image is available for execution validation. Failed assumptions
are explicit preflight failures, not fallbacks.

## End-to-end design and normal journey

The command is `python3 scripts/review_model_evaluation.py run --repo <clean-checkout>
--revision <full-sha> --base <full-sha> --findings findings.json --evidence evidence/ --profiles
profiles.json --checks checks.json --output new-run/`. It requires a new empty output directory.
It resolves commit/tree/base, rejects dirty checkout, produces `git archive <revision>`, hashes
the archive/diff/every regular supplied input, and writes `snapshot.json` before dispatch.
Only tracked files in that archive and explicit evidence are disclosed; no neighbouring files,
credentials, Git config, or provider-independent host environment is collected.

`findings.json` contains opaque `finding_id`, supplied severity/title, anchors
`{path,start_line,end_line}`, and declared `evidence_ids`. `evidence/manifest.json` hashes each
bounded UTF-8 evidence file and defines its ID/path/ranges. `checks.json` declares immutable
`check_id`, literal argv, working directory, timeout, applicable paths/evidence IDs, and optional
positive/negative control argv and expected exit. Models may nominate only declared `check_id`s;
their prose is never a shell command. Paths cannot escape the archive and IDs/ranges must match.

`profiles.json` names `judge_a`, `judge_b`, `synthesis`, `discovery`, and `validation` profiles:
`profile_id`, exact provider `model_id`, family, `strength: strong`, transport, tool identity,
timeout, and either literal CLI argv or HTTPS endpoint/auth-environment name. It rejects reused
judge IDs, absent profiles, non-strong judges, unsupported transport, missing executable/env,
or a model ID returned by transport that differs from its profile. Same family is permitted only
with a visible `same_family_correlation` record; no hard-coded unverified model name exists.

`review_model_executor.py` is the new project-local transport module. For `cli-json-v1` it calls
the configured argv without a shell, writes one bounded JSON request to stdin, captures bounded
JSON stdout/stderr/raw bytes, and requires `{invocation_id, model_id, result}`. For `http-json-v1`
it POSTs the same bounded request only to the configured provider, using the named environment
credential solely for that request. Each request has a role, prompt-template SHA, tool/profile
identity, snapshot/evidence identity, allowed file list and stage payload. Raw response, usage,
provider cost reference, exit/HTTP status, timestamps and redacted error are durable artifacts.

For each finding, the orchestrator builds two equivalent evidence packets but strips origin model
identity and every other assessment. It dispatches fresh judge sessions (stable findings may
batch, but each gets its own output record), validates their schemas/references, then runs only
applicable declared checks. `validation` invokes the same transport only to explain existing
check/evidence results; it cannot authorize a command. The OCI validation executor runs literal
declared argv in a new archive extraction with `--network none`, a digest-pinned image, cleared
environment, temporary HOME, no mounted credentials, and a writable disposable work directory.
Preflight verifies runtime/image digest/argv/timeout; no sandbox availability means
`environment_blocked`. Archive/work directories are deleted only after results are retained.

Discovery gets the archive/diff/evidence manifest and allowed evidence, but neither finding IDs
nor judge/synthesis outputs. Candidate anchors are range-validated, reconciled after discovery
against supplied findings by same path/range/claim scope, then receive the identical static and
declared-check process. A fresh synthesis receives validated judge records, check results and
source evidence for one finding/candidate; its strict result is `supported|refuted|unknown`,
reason/evidence IDs/disagreement, severity/usefulness, and cannot cite anything else.

The immutable output tree is `snapshot.json`, `run.json`, `invocations/<id>/{request.json,raw.*,
result.json}`, `checks/<finding-or-candidate>/<check-id>.json`, `findings.json`,
`omissions.json`, and `summary.{json,md}`. `run.json` has input identity, invocation/attempt IDs,
state (`running|successful|partial|failed`), completed artifact hashes, retry lineage, source,
usage and cost status. Restart accepts only an identical input identity and reuses hash-verified
completed effects; mismatch/conflicting duplicate records fail. A retry has a new invocation ID;
no completed request is resent. There is no automatic caching across changed snapshot, prompt,
model, or token budget.

The operator runs this one command, reads a complete or plainly partial report, and may later run
the weekly command against its immutable output plus the existing captured controller bundle.
`scripts/review_round_weekly_report.py` retains the frozen audit/product joins, receipt-binding,
Taipei week and exclusive delivery classifications, but adds `--evaluation-root`. It emits its
deterministic Markdown/JSON with stage availability, model result classes, disagreement, tests,
cost/usage source/unknown totals, and optional human records distinctly. It never calls a model.

## Exact future paths and acceptance plan

- `scripts/review_model_evaluation.py` — one-command snapshot, stage orchestration, schema/state
  validation, OCI check dispatch, restart logic, and immutable render.
- `scripts/review_model_executor.py` — the CLI/API JSON-v1 transport and raw-artifact boundary.
- `scripts/review_round_weekly_report.py` — prior source-correct weekly joins plus evaluation
  aggregation; it never judges or mutates production.
- `tests/test_review_model_evaluation.py` — deterministic fake-runner tests and synthetic Git
  repository creation; fake runner resides at `tests/fixtures/model_evaluation/fake_runner.py`.
- `tests/test_review_round_weekly_report.py` — preserves prior timestamp/join/receipt controls
  and adds machine-output aggregation/all-failed visibility fixtures.
- `docs/model-evaluation.md` — schemas, provider/OCI preflight, data disclosure, operator run and
  recovery; `docs/review-round-weekly-report.md` documents the added aggregate inputs/limits.

Tests must prove: two distinct records receive identical evidence but no identities/responses;
invalid/made-up anchors and evidence IDs are rejected; same-family warning and duplicate-ID
failure; fresh synthesis/discovery isolation; finding batching still emits every record; candidate
dedupe; support/refute/unknown disagreement; failed/blocked checks; positive/negative controls;
literal argv only; no network/credentials in OCI; retry/restart/mismatch behavior; no all-failed
score; and weekly controller delivery metrics remain separate. Use fake-runner transcript hashes
for determinism, including provider error/oversize/malformed output controls.

End-to-end acceptance is additionally a real one-command live-model smoke run on a synthetic,
clean local Git repo using caller-configured two distinct strong model IDs, synthesis and discovery
profiles, and a digest-pinned no-network check image. It retains raw artifacts and proves both
models actually returned their configured identity; mocked tests alone do not satisfy this gate.
A real interactive PTY journey for a selected CLI is required later, because its provider-specific
login/TTY behavior cannot be claimed by JSON-v1 noninteractive smoke. No generic spend approval
is requested: the owner already authorized use/cost. Functional completeness comes first; later
work may measure and optimize expense only after this journey exists.

Remaining limits: models can be wrong, correlated, unavailable, or refuse input; model agreement
is neither proof nor calibrated confidence. OCI isolation is a declared operational prerequisite,
not a magical sandbox; unavailable runtime/image/check remains blocked/unproven. The first version
has no fleet, scheduler, dashboard, live integration, deployment, automatic patch/merge/review
authority, human-ground-truth substitute, or council-vs-independent experiment.

Self-check: Frozen scope, output, model, effort and authority declared.
