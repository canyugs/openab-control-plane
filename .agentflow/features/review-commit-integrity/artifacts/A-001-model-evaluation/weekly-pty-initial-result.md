# Review-round weekly report — 2026-W37

Snapshot: `2026-09-09T08:10:00+08:00`  
Report ID: `46eb88fd3aff71e1ae679f771d35cd1ee14749d4365224fbd98814796b5c7296`  
Source: frozen local bundle; no live collection or model call.

## Cohort

| metric | count |
|---|---:|
| accepted denominator | 7 |
| eligible review sessions | 7 |
| ask exclusions | 0 |
| eligibility unknown | 0 |
| plan-only ingress accepted | 0 |

## Reliability

| category | count |
|---|---:|
| reliable | 2 |
| visible_failure | 2 |
| superseded | 1 |
| pending_or_unknown | 2 |
| denominator | 7 |
| unknown denominator | 0 |

## Latency

Observed exact: 6; reconciliation upper bound: 0; unknown: 1; clock-invalid: 0.

## Human and cost

Human coverage: `unknown`; useful: 0; not useful: 0; invalid: 0; unknown: 0; confirmed escapes: 0. No recall claim.
Cost coverage: `unknown`; actual total status: `totalunknown`; missing/unknown sessions: 7; partial sessions: 0; pricing table used: `False`.

## Model evaluation (separate denominator)

Availability: `not_supplied`; denominator: 0; supported: 0; refuted: 0; unresolved: 0; usefulness denominator: 0; disagreement items: 0; automatic omission candidates: 0; human-confirmed escapes: 0.

## Session drilldown

| session | repo | PR | eligibility | category | latency |
|---|---|---:|---|---|---|
| `normal-approve` | `org/repo` | 1 | review | reliable | original_receipt |
| `normal-request-changes` | `org/repo` | 2 | review | reliable | original_receipt |
| `pending` | `org/repo` | 7 | review | pending_or_unknown | incomplete_receipts |
| `superseded` | `org/repo` | 1 | review | superseded | original_receipt |
| `timeout-noop` | `org/repo` | 1 | review | pending_or_unknown | original_receipt |
| `timeout-visible` | `org/repo` | 1 | review | visible_failure | original_receipt |
| `visible-diagnostic` | `org/repo` | 3 | review | visible_failure | original_receipt |

## Definitions and limits

- Reliability is source-bound and mutually classified; missing joins, conflicts, and incomplete capture remain pending_or_unknown.
- Formal review proof uses provider_receipt.review_id/state/commit_id/reconciled; provider event is not a source field.
- Human usefulness, model evaluation, omissions, confirmed escapes, and delivery reliability have separate denominators.
- Costs are source minor units only; no price table, estimate, allocation, float-dollar conversion, or cross-currency total is made.
- No model, OCI executor, network, database, provider, or live API is invoked by this report.
