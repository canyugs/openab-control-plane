# Frozen model evaluation

`review_model_evaluation.py` evaluates supplied review findings against one
clean, frozen Git revision. It is an offline controller: it does not change
verdicts, rosters, GitHub writes, routing, checkout, patches, merge
state, or production state. It uses only Python's standard library.

The capability-first route is intentional. There is no price table, token cap,
cheaper fallback, model substitution, or stage omission. Requested identity,
observed identity, usage, and actual cost are separate fields. Claude's
`modelUsage` `costBasis=list` values are retained as list-price estimates, not
provider-reconciled billing; actual cost remains `unknown`.

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

The default shipped Claude argv is the verified OAuth-safe transport and has
no shell, file, or MCP tools:

```text
claude --print --safe-mode --restricted --disable-slash-commands \
  --no-session-persistence --output-format json \
  --json-schema '<literal serialized JSON schema>' \
  --model <configured-model-id> \
  --permission-mode dontAsk --permission-prompts none --tools '' \
  --strict-mcp-config --setting-sources '' \
  --system-prompt '<fixed role prompt>' -
```

The schema is one literal argv value, not a filename. The adapter accepts
either the Claude JSON result object or event-array envelope and extracts only
its `structured_output`. Assistant transport metadata may provide an observed
backend model, while model-authored fields never define identity. Raw
stdout/stderr and transport metadata are retained separately. Auxiliary
`modelUsage` entries are retained separately from the requested model. A
profile may explicitly select `transport: bare` for the older API-key path;
the default is OAuth-safe mode, with no fallback between model IDs. Codex
reports `unavailable` unless an externally verified no-tools/process
confinement attestation exists; read-only sandbox mode alone is insufficient.

The subprocess receives an explicit allowlist containing only runtime/locale
variables and supported Claude authentication variables. Arbitrary parent
environment variables, source-project settings/hooks, and model tools are not
passed through. Authentication is left to the installed CLI; no credential is
extracted or copied by this controller.

### Model-output contract

The literal JSON schemas sent to each model are the shape contract, and the
controller validators remain authoritative for dynamic membership and range
checks. Judge and synthesis citations are objects with exactly
`path`, `start`, `end`, and `evidence_id`; paths are relative POSIX paths in
the frozen source packet, line ranges are inclusive, and the evidence ID must
be one of the supplied IDs whose allowed range covers the citation. Generated
files, `/source` paths, citation strings, `quote`, and `note` fields are not
valid citation forms.

Discovery candidates are objects with exactly `claim`, `path`, `start`,
`end`, and `evidence_ids`. They carry no `finding_id`; the controller assigns
an opaque candidate ID after validating the source range and evidence
membership. The validation-plan fields are exactly `item_id`, `files`,
`runs`, and `claim_observed` at the top level, with generated file objects
containing only `path` and `utf8` and run objects containing only `name`,
`argv`, `cwd`, `expect`, and `evidence_ids`.

The plan must contain exactly one `baseline` and one `counterexample` run,
both with literal argv, `cwd: "/work"`, `expect.exit: 0`, and a structured
expectation containing boolean `claim_present` values that differ. The
controller additionally rejects shell syntax, undeclared evidence, unsafe
paths, crashes, timeouts, and non-JSON stdout. These model-facing limits mirror
the existing strict validators; they do not make an arbitrary model reference
valid.

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
    "image": "python@sha256:b64631e04e4920160c50fbe8d8df828f7f35f06f425cb44aa09bca53e708a35a",
    "docker_executable": "docker",
    "probe_daemon": true
  },
  "operator_checks": []
}
```

Limits are hard completeness limits. Omitted or unreadable tracked regular
files and over-limit diff/source bytes are listed in the source packet and
make the scope incomplete; no result may claim whole-scope support. The
OCI image is digest-pinned and must be locally present; the integrated
default is the verified Python image above and has no automatic unpinned
fallback. No credential or daemon setup is automated. The validation model
receives the complete source packet. `/source` is a read-only source mount and
`/work/generated` is the writable generated-test workspace; the fixed Python
runner executes only literal argv arrays there.

## Role isolation and stages

Every role gets the same exact source packet digest. Validation is generated
and executed before either independent judge. The validation author is told
the actual OCI context: the digest-pinned Python3 image exposes the frozen
source read-only at `/source`, generated files are materialized below
`/work/generated`, and controls run with literal argv and `cwd: "/work"`.
Generated programs must read or import the supplied source and print actual
JSON stdout containing `claim_present`; no manual test or operator prerequisite
is required. Judge packets contain one finding, only its declared evidence,
the complete source packet, and the generated validation observations. Each
judge adds `validation_verdict: valid|invalid|unproven`. `judge_a` and
`judge_b` are fresh calls with empty CLI working directories and cannot read
each other's output. Each independently assesses whether the executed
controls meaningfully distinguish the claim from its negation. A path printed
as text, a path in a comment, a hard-coded/argv-only result, or a generated-file
citation is invalid or unproven; structural source binding is only a signal,
not proof that the claim is semantically correct.
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
  "files": [{
    "path": "generated/check.py",
    "utf8": "import json\nimport sys\nsys.path.insert(0, '/source')\nfrom src.example import can_read\n\nrequester = 'mallory' if sys.argv[1] == 'yes' else ''\nprint(json.dumps({'claim_present': bool(can_read(requester, 'alice', public=False))}))\n"
  }],
  "runs": [
    {"name": "baseline", "argv": ["python3", "/work/generated/check.py", "yes"], "cwd": "/work", "expect": {"exit": 0, "observation": {"claim_present": true}}, "evidence_ids": ["E-1"]},
    {"name": "counterexample", "argv": ["python3", "/work/generated/check.py", "no"], "cwd": "/work", "expect": {"exit": 0, "observation": {"claim_present": false}}, "evidence_ids": ["E-1"]}
  ],
  "claim_observed": "The generated controls read the frozen source and distinguish the claim from its negation."
}
```

Generated paths must stay under `generated/`; files are UTF-8 and bounded.
Runs use literal argv arrays, `/work`, structured observation objects, and
distinct argv. The example above imports and calls the source function, then
prints actual JSON; the argv selects control inputs and is not itself the
claim. Shell interpreters, `-c`, traversal, links, undeclared evidence, ordinary crashes,
timeout, and assertion-false harnesses are rejected. Source binding is a
structural signal (for example, a source read through `open` or
`Path.read_text`, or an import after adding `/source` to `sys.path`), not
semantic proof: comments, printed constants, and source-path strings alone
remain unproven. Independent judge validation verdicts are required in
addition to the signal. Generated files are materialized only inside the
container and discarded from the host. Their raw content, digests, and actual
observations remain in the plan/result artifacts.

The OCI executor uses a new non-root container with a digest-pinned image,
`--network none`, read-only root and source bind mount, writable tmpfs
`/work`, cleared runtime environment, temporary HOME, dropped capabilities,
`no-new-privileges`, bounded memory/PIDs/CPU/output/time, and `--rm` cleanup.
It mounts no credentials, host socket, repository checkout, or project hook.
An unavailable daemon is an explicit `environment_blocked` result. OCI
execution remains raw `unproven` evidence until both judges qualify the
source-bound controls; the controller derives `executed_reproduced` or
`executed_refuted` only after their matching assessments and nonconflicting
synthesis.

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
validation/<item>/{plan.json,generated-file-digests.json,runs/<name>.json,result.json}
summary.json
summary.md
```

`findings.json` contains the normalized input identity plus the complete result
record for every original finding; `omissions.json` does the same for every
blind-discovery candidate. The snapshot records full revision/base, archive
and diff digests, model profiles, input digests, source omissions, and
completeness. `run.json` records requested model IDs, capability/observed
identity status, invocation attempts, list-price estimates, unknown actual
cost, state, and the SHA-256 ledger for every other output file. Failed
attempts keep their raw partial captures; retries use a new attempt directory.

Before a terminal summary is returned, `verify_evaluation_artifacts(root)`
checks the snapshot/run identity, output schema, symlink/traversal safety,
every artifact hash, every invocation packet/argv/raw/final capture, every
validation plan/run, and the summary digest. A restart reuses completed model
and validation artifacts only after those checks and refuses changed input or
tampered bytes. It does not repeat a completed model or OCI call. A partial or
failed process remains auditable and cannot become a score.

## Deterministic checks

```text
python3 -m unittest tests.test_review_model_evaluation tests.test_review_model_oci_executor
python3 -m compileall -q scripts/review_model_evaluation.py scripts/review_model_adapters.py scripts/review_model_oci_executor.py
```

These checks use captured/synthetic CLI envelopes and a fake OCI boundary
seam. They do not claim live provider authentication, model success, or a
running Docker daemon. The final `run.json` contains SHA-256 entries for every
other retained output artifact and restart verifies those bytes before reusing
a completed stage.
