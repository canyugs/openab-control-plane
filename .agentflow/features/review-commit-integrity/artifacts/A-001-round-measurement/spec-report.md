* _2026-09-09 13:48:56 (gpt-5.6-terra/high)_

# Stage 2 design — offline weekly council report

## Authority and bounded outcome

This is an implementation-ready Stage 2 design only. Stage 1 Result Go is recorded; no Stage 2 source change, runtime change, production claim, schedule, live query, credential use, Agentflow action, delegation, commit, or push is authorized by this report. Stage 3 is excluded. The only authorized current change is this report.

Implement one user-invoked Python-standard-library tool that reads an explicit, already-captured local evidence bundle and writes a weekly Markdown report plus matching JSON. It is an observation artifact, not an evidence platform: no Rust change, service/schema/database/API, daemon, scheduler, dependency, network, posting, or live DB access. It never changes verdicts, retries, roster selection, GitHub writes, or routing (R-18 / INV-7).

## Exact future change set

- `scripts/review_round_weekly_report.py` — offline validator, joiner, classifier, and Markdown/JSON renderer.
- `tests/test_review_round_weekly_report.py` — focused `unittest` synthetic in-memory fixtures; no Rust retest.
- `docs/review-round-weekly-report.md` — the usage and input-contract document below, including synthetic-only examples.

No existing files change. Fixtures belong inside the Python test module unless a future test needs an opaque malformed input file; then add only `tests/fixtures/review_round_report/<case>.ndjson` named by that test.

## Concepts deliberately added

| Concept | Observable need | Rejected smaller alternative |
| --- | --- | --- |
| Evidence bundle manifest | Proves exactly which captured files, coverage declarations, and hashes produced a report. | Reading a directory opportunistically makes omissions and revision drift invisible. |
| Session cohort | Deduplicates delivery retries and includes accepted work that never reaches a round. | Counting terminal rounds silently drops unmatched accepted work. |
| Source-backed expected-write set | Establishes completion against the terminal path selected, not against rows that happened to be enqueued. | “All observed writes succeeded” can call an empty/enqueued subset successful. |
| Human annotation/escape records | Measures quality with declared human judgement and identity. | Dismissal, waiver, absence of action, or structural gates are not quality ground truth. |
| Immutable report artifact | Enables reproducibility and safe R-13/R-17 drilldown without runtime state. | Regenerating a mutable `latest` report loses the exact observation. |

## Operator journey and files

The operator obtains documented, complete exports from the existing signed controller audit endpoint (`/api/v1/audit/events`, cursor-paginated) and findings read endpoint, plus a local product-table export for targets/rounds. This design does not automate those exports or promise production coverage. The operator places only these regular files in an otherwise empty evidence directory:

```text
bundle/
  evidence-manifest.json                 required
  audit.ndjson                           required: controller audit export
  targets.ndjson                         required: SessionTarget export
  rounds.ndjson                          required: ReviewRound plus terminal branch export
  findings.ndjson                        required: ReviewFinding export
  human.ndjson                           optional: annotations and confirmed escapes
  cost.ndjson                            optional: reconciled per-session actual cost
```

`targets.ndjson` and `rounds.ndjson` are distinct because a target is created at action opening while a round is terminal evidence; this is the smallest physical separation that prevents a report exporter from inventing their join. No evidence file is overwritten. The tool creates a new output directory supplied by the operator and refuses it if either target filename already exists:

```sh
python3 scripts/review_round_weekly_report.py \
  --input /path/to/bundle --week 2026-W37 --as-of 2026-09-14T23:59:59+08:00 \
  --output /path/to/new-report-directory
# creates review-round-weekly-2026-W37-asof-20260914T155959Z.{md,json}
```

It resolves the ISO week in `Asia/Taipei`: Monday 00:00:00+08:00 inclusive to the next Monday exclusive. `--as-of` is required, offset-bearing RFC 3339, not later than report creation, and filters observations by their persisted audit `recorded_at`; late receipts recorded after it are excluded and reported as later/unobserved, never back-dated. The manifest may declare a narrower capture end; the tool fails if it predates `as_of` without `completeness.audit: "partial"`.

## Strict v1 input contract

All JSON is UTF-8, objects only, no duplicate JSON keys, no unknown top-level fields, no line over 1 MiB, and `schema_version` is exactly `ocp.review-round-weekly-evidence.v1`. NDJSON has one complete record per line, no blank lines. IDs are opaque strings matching `[A-Za-z0-9:_-]{1,200}`; repository is `owner/repo`; SHA, path, title, reason, provider messages, and human notes are data only and must never be executed or rendered as Markdown/HTML.

`evidence-manifest.json` has exactly `schema_version`, `bundle_id`, `created_at`, `as_of`, `timezone` (`Asia/Taipei`), `files`, `completeness`, and `retention`. `files` maps every present filename to `{sha256,records,exported_at}`; required files must be present, hash-match, and have exact line count. `completeness` declares `audit`, `targets`, `rounds`, `findings` as `complete|partial|unknown`, plus `audit_cursor_complete` boolean. `retention` declares each source’s `retained_until` RFC 3339 or `unknown`; the contract documents the current audit defaults (90 days; 365 days for eligible extended events), but neither promises preservation nor deletes evidence.

Required audit records are captured existing audit envelopes with `event_id`, `event_key`, `kind`, `outcome`, `occurred_at_ms`, `recorded_at_ms`, and `correlation` (`delivery_id`, `action_id`, `trigger_ref`, `trigger_fingerprint`, `session_id`, `write_id`, nullable only where the source omits them), plus bounded `detail`. Required kinds include captured `action.accepted`, `action.completed`, `github.write.enqueued`, `github.write.succeeded`, `github.write.reconciled`, retry/failed evidence, and runtime terminal/supersede evidence when available. Timestamps are integers in Unix milliseconds; no seconds are accepted in audit records.

Each target record is `{session_id,repo,pr_number,head_sha,reason,created_at_s,required_valid_reviewers}`. Each round record is `{session_id,repo,pr_number,terminal_event,terminal_branch,decision,integrity_disposition,verified_commit_id,head_sha,created_at_s}`. `terminal_branch` is one of `normal_verified`, `integrity_failed`, `unparseable`, `insufficient_valid_reviewers`, `superseded`, `timeout`; it is exported from the durable controller terminal/closing path, not guessed from text. Product timestamps are Unix seconds and converted exactly once to ms.

Each finding record is `{session_id,repo,pr_number,head_sha,stable_id,severity,status,raised_by,angle,created_at_s}`. Optional `human.ndjson` discriminates either `annotation` with `{session_id,repo,pr_number,head_sha,stable_id,annotation_version,scope,verdict,annotated_at,annotator_id,evidence_ref}` or `escape` with `{repo,pr_number,head_sha,review_window_start,review_window_end,annotation_version,scope,disposition,confirmed_at,confirming_identity,evidence_ref}`. Annotation `verdict` is `valid_useful|valid_not_useful|invalid|unknown`; escape `disposition` is `confirmed_escape|not_escape|unknown`. Scope/version/identity/evidence reference are mandatory. An escape must overlap the stated reviewed window; it is not attributed to an individual finding.

Optional `cost.ndjson` records `{session_id,provider_account,currency,amount_minor,usage_interval_start,usage_interval_end,reconciliation_ref,reconciled_at}`. Currency is uppercase ISO 4217 (or `XXX` only when the source explicitly calls it unknown); `amount_minor` is an integer and no cross-currency total is calculated. Broad account totals, tokens, prices, estimates, and allocations without a reconciled per-session attribution are rejected as cost evidence.

Duplicate records with byte-identical canonical JSON are deduplicated by natural identity (`event_id`; session target; session round; full finding identity; annotation/escape identity; cost reconciliation identity). Conflicting duplicates, duplicate manifest file names, cross-file identity contradictions, malformed paths, hash mismatch, bad timestamp/unit, unknown schema, or a non-unique accepted-to-session join make the affected unit `unknown` and emit a safe validation diagnostic; manifest/hash/schema corruption fails the run before output.

## Cohort, joins, and four metric definitions

The session-scope cohort starts only from unique real `action.accepted` records inside the Taipei week, paired by delivery/action/trigger correlation to one successful `action.completed` carrying one session ID. Redelivery is deduped by action/session identity. `ingress.accepted` is plan-only and is explicitly excluded; `ask` and all plan-only reasons are excluded from real council cohorts and reported in `excluded_by_reason`. An accepted action lacking a unique completed-session/target/round join remains in the accepted denominator as `unknown`, with coverage; it is never dropped. A session may have at most one target and one round.

1. **Human quality (R-13):** For scoped findings, report annotation counts by the four explicit verdicts and `annotation_coverage = annotated_non_unknown / findings`. Report confirmed escapes separately as `confirmed_escape / all escape records`, plus unknown/unconfirmed count and review-window coverage. This says only what named humans annotated at the stated scope/version; it does not claim correctness, precision, recall, usefulness beyond the annotation, or infer a dismissal/waive as invalid/useful. Structural `verified`, SHA failure, parseability, and reviewer sufficiency appear as integrity/reliability context only.
2. **Trigger-to-projection latency (R-14):** Start is `action.accepted.occurred_at_ms`; endpoint is the latest `occurred_at_ms` among successful/reconciled receipts for every required terminal projection. A negative duration, receipt earlier than acceptance, missing original receipt timestamp, or cross-clock contradiction is `clock_invalid`; do not clamp. If only reconciliation timing is available, report it separately as `reconcile_observation_upper_bound_ms`, not original completion latency. Pending/missing writes are `pending_or_unknown`, never session-close latency.
3. **Actual cost (R-15):** For each cohort session, sum only its reconciled cost records by currency, separately listing retries where records say they are included. Missing, invalid, duplicated-conflicting, unallocated, or `XXX` values yield `unknown` with reason and coverage; zero is actual only when a valid record says zero. No aggregate across currencies and no inferred zero.
4. **Terminal projection reliability (R-16):** Derive a required-write set before looking at receipts: `normal_verified` requires `comment`, `status` (success for approve, failure for request-changes), and `review`; `integrity_failed`, `unparseable`, and `insufficient_valid_reviewers` require `comment` plus `status:error` only if target SHA is valid; explicit `superseded|timeout` requires `comment_abandon`. Cross-check every expected item with a matching immutable enqueue and a success/reconciled receipt; unexpected writes are diagnostic, never requirements. All required confirmations means `reliably_completed` (a complete formal `REQUEST_CHANGES` is reliable, not an operational failure). `visible_failure` requires success/reconciled receipt of the terminal diagnostic/error projection; a failed GitHub write alone is never visible failure. `superseded` is separate; report `tombstone_visible` only from its `comment_abandon` receipt. Everything else is `pending_or_unknown` and retains the reason.

`expected_write_set_missing`, absent enqueue, receipt without matching request identity, receipt conflict, partial set, missing target/round, incomplete required export, and corrupt optional evidence lower applicable coverage and prohibit a reliable result. Normal expected identities include session ID, write kind, repo/PR, status state or review event, and verified commit where authority-bearing; they use the persisted branch/round plus `github.write.enqueued` payload, never an empty/all-success shortcut.

## Report artifact, provenance, and safe rendering

The JSON artifact has `definition_version: "review-round-weekly/v1"`, `report_id`, `week`, `timezone`, `as_of`, `generated_at`, `input_manifest_sha256`, exact input file hashes/record counts, completeness/retention declarations, all category counts, denominators, coverage fractions, units, validation diagnostics, and a sorted session drilldown. Markdown is a deterministic rendering of that JSON. `report_id` is `sha256(definition_version + week + as_of + input_manifest_sha256)`; filenames include the week/as-of and report-id prefix. A rerun with the same bytes must reproduce byte-identical JSON apart from no field; `generated_at` is therefore the manifest `created_at`, not wall time.

Drilldowns expose only `report_session_id = sha256(bundle_id + session_id)[:20]`, `report_finding_id = sha256(bundle_id + session_id + stable_id + head_sha)[:20]`, and the same opaque write/event hashes. Raw IDs, repo/PR, paths, finding titles, comment bodies, actor identity, audit error text, tokens, and cost references never enter Markdown or JSON output. The output contains zero-valued categories for every enum, including no observations, and writes no HTML, links, or untrusted strings. This is durable local persisted output for R-13/R-17, with raw-reference drilldown safety but no new runtime state.

## Red-first fixture suite and commands

First write failing synthetic fixtures for: a complete verified approve; complete request-changes; integrity diagnostic; superseded tombstone; accepted-but-unmatched; duplicate redelivery; conflicting duplicate; missing/partial required audit page; missing receipt; late receipt; reconciliation-only receipt; negative/mixed-unit clock; ask/plan-only exclusion; dismissed/waived finding without annotation; each human verdict and escape state; absent/zero/unknown/multi-currency/retry cost; corrupt hash; malformed path; and overwrite refusal. Fixtures must label themselves synthetic; current/live metrics are never claimed from them.

Future implementation checks, all offline:

```sh
python3 tests/test_review_round_weekly_report.py
python3 -m py_compile scripts/review_round_weekly_report.py
python3 scripts/review_round_weekly_report.py --input tests/fixtures/review_round_report/complete --week 2026-W37 --as-of 2026-09-14T23:59:59+08:00 --output /tmp/ocp-weekly-report-check
```

The test invokes the tool twice against a fresh temporary output to prove deterministic output and invokes it again against the same output to prove refusal. No Cargo command is required for a script/docs-only change.

## Requirement ledger and derived invariants

R-13 is human annotations/confirmed escapes with explicit unknown coverage; R-14 is accepted-to-actually-confirmed projection latency; R-15 is reconciled currency-specific actual-or-unknown cost; R-16 separates reliable completion, public failure, supersession, and pending/unknown; R-17 is the versioned weekly Markdown/JSON artifact with safe drilldowns and zero categories; R-18/INV-7 is enforced by a local read-only CLI boundary. Derived invariants: (a) no plan-only or ask session affects a real metric denominator; (b) every accepted session is counted once in its coverage denominator; (c) no missing evidence becomes zero, success, valid, useful, failure-visible, or escape; (d) reliable completion entails all source-backed expected confirmations; (e) visible failure entails a confirmed public diagnostic/error receipt; (f) a supersession remains superseded even when its tombstone is visible; and (g) same validated bytes yield the same report ID and counts.

Self-check: Frozen scope, output, model, effort and authority declared.
