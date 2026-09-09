# Frozen model evaluation

`review_model_evaluation.py` evaluates supplied review findings against one
clean, frozen Git revision. It is an offline controller: it does not change
verdicts, retries, rosters, GitHub writes, routing, checkout, patches, merge
state, or production state. It uses only Python's standard library.

The capability-first route is intentional. There is no price table, cache,
token cap, cheaper fallback, model substitution, or stage omission. Requested
identity, observed identity, usage, and actual cost are separate fields. If a
CLI does not emit trusted transport metadata, observed model identity and
actual cost remain `unavailable`/`unknown`.

## Command

Run from the repository root with a full 40-hex revision and base SHA:

```text
python3 scripts/review_model_evaluation.py run \
  --repo /path/to/clean-repository \
  --revision 0123456789abcdef0123456789abcdef01234567 \
  --base fedcba9876543210fedcba9876543210fedcba98 \
  --findings /path/to/findings.json \
  --evidence /path/to/evidence \
  --models /path/to/models.json \
  --environment /path/to/environment.json \
  --output /path/to/new-evaluation-output
```

`--environment` is optional. The output must be new or be a matching
restart directory outside the repository and all input paths. A dirty tree,
short SHA, changed input identity, conflicting artifact, or output overlap is
a visible error. The controller writes `snapshot.json` before its first model
CLI call. It uses direct `git` argv calls only; it never runs project hooks.

## Exact input schemas

### `findings.json`

```json
{
  "schema_version": "review-model-findings/v1",
  "findings": [
    {
      "finding_id": "opaque-id",
      "title": "bounded title",
      "severity": "high",
      "claim": "bounded claim text",
      "location": {"path": "src/example.py", "start_line": 10, "end_line": 12},
      "evidence_ids": ["E-1"]
    }
  ]
}
```

`finding_id`, title, severity, and claim are bounded UTF-8 strings. The path
is a normalized relative POSIX path. Evidence IDs are non-empty strings.
An input array of finding objects is also accepted for compatibility, but the
object form above is the recorded artifact form.

### `evidence/manifest.json`

```json
{
  "schema_version": "review-model-evidence/v1",
  "entries": [
    {
      "id": "E-1",
      "utf8": "the captured evidence bytes\n",
      "sha256": "<64 lowercase hex>",
      "allowed_ranges": [
        {"path": "src/example.py", "start": 10, "end": 12}
      ]
    }
  ]
}
```

The digest is SHA-256 over the exact UTF-8 bytes. An entry may use a safe
relative `file` instead of `utf8`; the file must still be UTF-8 and its exact
bytes must match `sha256`. A citation is valid only when its path and complete
line range are inside both the frozen source packet and an allowed evidence
range declared for that finding. Invented, out-of-range, or undeclared
citations invalidate the assessment.

### `models.json`

The five roles are mandatory. `judge_a` and `judge_b` must have distinct
literal `model_id` values. A same-family pair is allowed but recorded as a
correlation warning. Every profile must be strong.

```json
{
  "roles": {
    "judge_a": {"adapter": "claude", "model_id": "claude-model-a", "family": "claude", "strength": "strong"},
    "judge_b": {"adapter": "claude", "model_id": "claude-model-b", "family": "claude", "strength": "strong"},
    "synthesis": {"adapter": "claude", "model_id": "claude-synthesis", "family": "claude", "strength": "strong"},
    "discovery": {"adapter": "claude", "model_id": "claude-discovery", "family": "claude", "strength": "strong"},
    "validation": {"adapter": "claude", "model_id": "claude-validation", "family": "claude", "strength": "strong"}
  }
}
```

`adapter` is `claude` or `codex`; an optional `executable` selects a literal
installed executable path. There is no user-written adapter contract.

The shipped Claude argv is direct and has no shell or model tools:

```text
claude --print --bare --no-session-persistence --output-format json \
  --json-schema '<literal serialized JSON schema>' \
  --model <configured-model-id> \
  --permission-mode dontAsk --permission-prompts none --tools '' \
  --strict-mcp-config --system-prompt '<fixed role prompt>' -
```

The schema is one literal argv value, not a filename. The adapter parses only
the CLI JSON envelope's `structured_output`; prose and model-authored
`model_id` fields do not define identity. Raw stdout/stderr and transport
metadata are retained separately. Codex has a command builder for inspection,
but its adapter reports `unavailable` unless an externally verified
no-tools/process-confinement attestation exists; read-only sandbox mode alone
is explicitly insufficient. There is no fallback from Codex to Claude.

### Optional `environment.json`

```json
{
  "source_limits": {
    "max_files": 10000,
    "max_file_bytes": 2097152,
    "max_total_bytes": 33554432,
    "max_diff_bytes": 8388608
  },
  "oci": {
    "image": "python:3.12-slim@sha256:<64 lowercase hex>",
    "docker_executable": "docker",
    "probe_daemon": true
  },
  "operator_checks": []
}
```

Limits are hard completeness limits. Omitted or unreadable tracked regular
files and over-limit diff/source bytes are listed in the source packet and
make the scope incomplete; no result may claim whole-scope support. The
default OCI image is a documented placeholder digest and therefore normally
produces `environment_blocked` until an operator supplies a locally present,
digest-pinned image. No credential or daemon setup is automated.

## Role isolation and stages

Every role gets the same exact source packet digest. Judge packets contain one
finding and only its declared evidence. `judge_a` and `judge_b` use fresh
empty CLI working directories and cannot read each other's artifacts.
Synthesis receives anonymized `judge_1`/`judge_2` assessments, source/evidence,
and observed validation results; disagreement is preserved and `unknown` is
allowed. Discovery receives source/evidence only: no original finding,
assessment, or author-model identity is serialized. A candidate is reconciled
with originals by normalized claim and overlapping path/range, with the
duplicate relation retained rather than silently dropped. Candidate judges and
synthesis are fresh calls after discovery.

For every original finding and candidate, validation returns exactly:

```json
{
  "item_id": "opaque-id",
  "files": [{"path": "generated/check.py", "utf8": "..."}],
  "runs": [
    {"name": "baseline", "argv": ["python3", "/work/generated/check.py"], "cwd": "/work", "expect": {"exit": 0, "observation": {"claim_present": true}}, "evidence_ids": ["E-1"]},
    {"name": "counterexample", "argv": ["python3", "/work/generated/check.py", "safe"], "cwd": "/work", "expect": {"exit": 0, "observation": {"claim_present": false}}, "evidence_ids": ["E-1"]}
  ],
  "claim_observed": "bounded description"
}
```

Generated paths must stay under `generated/`; files are UTF-8 and bounded.
Runs use literal argv arrays, `/work`, structured observation objects, and
distinct argv. Shell interpreters, `-c`, traversal, links, undeclared
evidence, ordinary crashes, timeout, and assertion-false harnesses are
rejected. A controller check also requires the literal plan to reach the
read-only `/source` mount or name the cited source path; printing arbitrary
JSON is not source evidence. Generated files are materialized only inside the
container and discarded from the host. Their raw content, digests, and actual
observations remain in the plan/result artifacts.

The OCI executor uses a new non-root container with a digest-pinned image,
`--network none`, read-only root and source bind mount, writable tmpfs
`/work`, cleared runtime environment, temporary HOME, dropped capabilities,
`no-new-privileges`, bounded memory/PIDs/CPU/output/time, and `--rm` cleanup.
It mounts no credentials, host socket, repository checkout, or project hook.
An unavailable daemon is an explicit `environment_blocked` result.

An item is classified as exactly one of `static_evidence`,
`executed_reproduced`, `executed_refuted`, `environment_blocked`, or
`unproven`. Model judgments remain `provenance: model_assessment`; they are
not human correctness, recall, or product verdicts. A failed or incomplete
model stage cannot score. Automatic omission candidates never imply recall or
human-confirmed escapes.

## Output and restart

The immutable output tree is:

```text
snapshot.json
run.json
source-packet.json
findings.json
omissions.json
invocations/<id>/{packet.json,argv.json,raw.stdout,raw.stderr,final.json,result.json}
validation/<item>/{plan.json,generated-file-digests.json,runs/<name>.json}
summary.json
summary.md
```

The snapshot records full revision/base, archive and diff digests, input
digests, source omissions, and completeness. `run.json` records requested
model IDs, capability/observed identity status, invocation attempts, unknown
usage/cost, state, and summary digest. A restart rechecks all completed
invocation packet/argv/raw/final bytes and refuses tampering or changed input
identity; it does not repeat a completed call. A partial process failure is
retained as a failed invocation and cannot become a score.

## Deterministic checks

```text
python3 -m unittest tests.test_review_model_evaluation tests.test_review_model_oci_executor
python3 -m compileall -q scripts/review_model_evaluation.py scripts/review_model_adapters.py scripts/review_model_oci_executor.py
```

These checks use fake CLI transports and an OCI boundary seam. They do not
claim live provider authentication, model success, or a running Docker daemon.
The final `run.json` contains SHA-256 entries for every other retained output
artifact and restart verifies those bytes before reusing a completed stage.
