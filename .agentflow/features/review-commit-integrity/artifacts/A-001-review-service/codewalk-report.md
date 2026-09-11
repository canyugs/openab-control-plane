* _2026-09-09 11:55:08 (gpt-5.6-terra/high)_

# SHA-integrity codewalk — pre-patch boundary report

## Authority, scope, and result

Read-only, source-grounded investigation of Stage 1 only: the path from a signed terminal event through terminal projection, review-round/findings storage, outbox delivery, GitHub formal review/status, and replay reconciliation. I inspected no live service, credential, or path outside this clone; executed no tests and made no source/configuration change. The sole write is this report.

The requirements are explicit: a normal council approval needs a non-empty, syntactically valid findings-block SHA exactly equal to the immutable opened-session target; missing/invalid/mismatched evidence must fail closed, and the formal review itself plus retry reconciliation must be bound to that verified commit ([requirements-report.md:15-28](.agentflow/features/review-commit-integrity/artifacts/A-001-review-service/requirements-report.md#L15-L28)).

Current conclusion: an approving terminal trailer can create both an `openab/council` success status and a formal `APPROVE` when the findings SHA is missing, malformed, or different from the opened target. The status is sent to the opened target, while the formal review request carries no commit identifier and therefore is not proven to be attached to that target. A projection-only repair is incomplete because pre-existing queued rows and the reclaim path can still submit/adopt the unbound approval.

## How a forged or missing reviewed SHA authorizes GitHub state

### Verified facts

1. `SessionTarget.head_sha` is the target persisted after an open action; pull-request webhooks create the plan fingerprint as `sha:<head>`, while comment-triggered reviews fetch the current GitHub head only if the plan lacks one ([planner.rs:328-354](crates/github-pr-controller/src/planner.rs#L328-L354), [lib.rs:977-1013](crates/github-pr-controller/src/lib.rs#L977-L1013)). The terminal event itself is admitted only after HMAC/identity/timestamp/body checks ([runtime_events.rs:61-112](crates/github-pr-controller/src/runtime_events.rs#L61-L112)).
2. The findings parser accepts `head_sha` as an optional `String`; it validates finding fields but applies no non-empty, Git object-ID format, or equality rule to that SHA ([verdict.rs:38-69](crates/github-pr-controller/src/verdict.rs#L38-L69), [verdict.rs:174-196](crates/github-pr-controller/src/verdict.rs#L174-L196)).
3. `plan_close` uses the trusted target only for `status_sha`, but obtains `reviewed_sha` from agent findings and silently falls back to the target when no block/SHA exists ([closing.rs:172-191](crates/github-pr-controller/src/closing.rs#L172-L191)). A parsed approval always queues a success status for the target ([closing.rs:237-247](crates/github-pr-controller/src/closing.rs#L237-L247), [closing.rs:279-284](crates/github-pr-controller/src/closing.rs#L279-L284)); it also queues `APPROVE` unless the trailer is blocking/request-changes ([closing.rs:249-264](crates/github-pr-controller/src/closing.rs#L249-L264)). The claimed SHA is only text in the review body ([closing.rs:302-313](crates/github-pr-controller/src/closing.rs#L302-L313)).
4. The resulting claimed SHA is persisted in `review_rounds.head_sha` and in every finding, even though the status payload retains the opened target ([lib.rs:2150-2186](crates/github-pr-controller/src/lib.rs#L2150-L2186); [sqlite.rs:59-110](crates/github-pr-controller/src/store/sqlite.rs#L59-L110); [sqlite.rs:937-980](crates/github-pr-controller/src/store/sqlite.rs#L937-L980)). The existing test deliberately demonstrates a mismatch (`reviewedsha` vs `openingsha`) remaining a normal round and a queued review/status ([lib.rs:4947-5043](crates/github-pr-controller/src/lib.rs#L4947-L5043)); the unit test also treats no findings SHA as reviewable ([closing.rs:876-882](crates/github-pr-controller/src/closing.rs#L876-L882)).
5. `GitHubClient::submit_review` POSTs only `event` and `body`; it has no `commit_id`/SHA argument. Its success condition checks only returned review state and id ([github.rs:206-232](crates/github-pr-controller/src/github.rs#L206-L232)). Therefore the response does not prove the review was recorded against the verified session target.
6. Reclaim reconciliation finds the first controller-owned review containing the marker and returns only its id ([github.rs:403-459](crates/github-pr-controller/src/github.rs#L403-L459)); the drain accepts it without comparing the queued event or commit identity ([lib.rs:2510-2553](crates/github-pr-controller/src/lib.rs#L2510-L2553)). The current replay test proves marker-plus-controller-identity behavior only; its review fixture contains neither a commit nor event assertion ([lib.rs:5417-5626](crates/github-pr-controller/src/lib.rs#L5417-L5626)).

### Causal answer

- **Missing SHA:** fallback at `closing.rs:187-191` converts a missing findings block or `head_sha` into the opened target for stored provenance/review-body text; a valid `approve` trailer consequently queues success + `APPROVE`.
- **Malformed SHA:** parser and projection retain it as an ordinary string; no validity gate stops the same success + `APPROVE` projection.
- **Forged/mismatched SHA:** the success status is safely targeted at the opened SHA, but this does not prove the council reviewed that commit. The formal review still posts without a commit binding, so GitHub may associate it with its then-current PR head; the body’s forged SHA is non-authoritative text. Thus an approval can satisfy the review gate without proof that its target was reviewed.
- **Crash/retry:** a controller-owned marker-bearing review with the wrong event/commit is presently adopted, because neither is persisted in/recovered from the review intent nor checked on the returned review object.

The last two bullets are a source-based inference from the request/response and reconciliation shapes above. Exact GitHub REST field semantics remain an external API question to verify before coding; this pass did not browse or call GitHub.

## Narrowest complete enforcement boundary

The smallest complete Stage-1 change is a single normal-council integrity decision made before any authority-bearing terminal write is created, plus preservation and verification of that decision at the durable send/reconcile boundary. Changing only `review_body`, only the comment, or only the terminal status is insufficient.

1. **Normal terminal projection — `closing.rs`:** derive one verified target from the immutable `SessionTarget`, classify findings evidence as missing / invalid / mismatch / valid, and allow an approval/success/review intent only for valid equality. The fail-closed projection must produce the specified diagnostic and target error status, and persist the classification with the round/result. Keep the reviewer-evidence gate composed before/with this one; it already routes failure to comment+error status and no review ([lib.rs:2100-2129](crates/github-pr-controller/src/lib.rs#L2100-L2129), [closing.rs:70-116](crates/github-pr-controller/src/closing.rs#L70-L116)).
2. **Immutable open target — both store back ends:** `record_session_target` currently updates `repo`, `pr_number`, and any later non-null `head_sha` on a session-id conflict, rather than asserting same identity ([sqlite.rs:534-558](crates/github-pr-controller/src/store/sqlite.rs#L534-L558); [postgres.rs:795-827](crates/github-pr-controller/src/store/postgres.rs#L795-L827)). The target needs first-write/compare-equal semantics so a deduplicated/replayed open cannot revise the evidence that close compares.
3. **Durable intent and round data — `store.rs`, SQLite and Postgres migrations:** persist verified target SHA and integrity disposition, and put the verified SHA in the normal review payload/receipt correlation. `review_rounds` currently has only one ambiguous `head_sha`; `github_writes.payload_json` is schemaless and its unique key is `(session_id, kind)` ([sqlite.rs:59-110](crates/github-pr-controller/src/store/sqlite.rs#L59-L110); [sqlite.rs:983-1025](crates/github-pr-controller/src/store/sqlite.rs#L983-L1025)). The Postgres schema mirrors these shapes ([postgres.rs:99-153](crates/github-pr-controller/src/store/postgres.rs#L99-L153)). One field that alternately means claimed SHA, fallback target, and finding head cannot evidence this boundary.
4. **GitHub client and outbox drain — `github.rs` / `lib.rs`:** submit the formal review with the verified commit and require GitHub’s returned commit identity to equal it. On reclaimed review intent, adopt only an object that is controller-owned, marker-bearing, event-equal, and commit-equal; otherwise do not mark the write done. The payload already survives claim leases unchanged and is the correct durable carrier ([sqlite.rs:1027-1075](crates/github-pr-controller/src/store/sqlite.rs#L1027-L1075)); `perform_write_with_receipt` is the common execution/audit chokepoint ([lib.rs:2323-2390](crates/github-pr-controller/src/lib.rs#L2323-L2390), [lib.rs:2557-2589](crates/github-pr-controller/src/lib.rs#L2557-L2589)).
5. **Sibling approval producer — `deciding.rs` / `apply_finding_command`:** this is not a normal terminal council result and should remain separately governed, but it emits a success status and formal `APPROVE` when an authorised, current-head-CAS judgement removes the last blocker ([deciding.rs:162-193](crates/github-pr-controller/src/deciding.rs#L162-L193)). Its current payload likewise lacks review commit identity, and its replay uses the same generic review sender ([lib.rs:2510-2553](crates/github-pr-controller/src/lib.rs#L2510-L2553)). It needs an explicit commit-binding policy before release; it must not be accidentally classified as a terminal findings-block approval.

The policy for an SHA-anomalous `REQUEST_CHANGES` result is intentionally not settled by the requirements: it may retain a carefully defined safe blocking projection, or degrade to diagnostic/error only, but can never make the anomaly look successful ([requirements-report.md:19-21](.agentflow/features/review-commit-integrity/artifacts/A-001-review-service/requirements-report.md#L19-L21)). Do not select that policy implicitly while adding the approval guard.

## Stored-data and compatibility implications

### Old persisted rows that bypass a projection-only repair

- **Pending/in-flight normal `review` rows** were serialized before any new verified-commit field. The sender currently accepts all `KIND_REVIEW` payloads containing an allowed event and submits them without a SHA ([lib.rs:2510-2552](crates/github-pr-controller/src/lib.rs#L2510-L2552)). A new `plan_close` guard cannot alter them.
- **Pending success `status` rows** can still publish success without a durable verification proof; the generic status sender treats any payload state `success` as authoritative ([lib.rs:2483-2508](crates/github-pr-controller/src/lib.rs#L2483-L2508)).
- **Existing `review_rounds` / `review_findings`** can have a missing/claimed/fallback `head_sha`, which cannot retrospectively establish a valid result because the raw terminal source is not stored in these tables. Existing fields and write payloads therefore cannot safely reconstruct integrity after migration.
- **The `(session_id, kind)` unique key prevents simply enqueueing a replacement failure status/review of the same kind**; an old unsafe row must be transformed/quarantined/terminally failed under an explicitly designed migration path before a diagnostic projection can coexist. The same detail applies to in-flight rows reclaimed after lease expiry ([store.rs:521-552](crates/github-pr-controller/src/store.rs#L521-L552)).
- **Session targets from prior migrations may have `NULL` head SHA** (`head_sha` is nullable and reason/reviewer fields were added later) ([sqlite.rs:118-124](crates/github-pr-controller/src/store/sqlite.rs#L118-L124), [sqlite.rs:204-210](crates/github-pr-controller/src/store/sqlite.rs#L204-L210)). They cannot be repaired by fetching a newer head: the requirement makes that a new session, not evidence for the old one ([requirements-report.md:26-28](.agentflow/features/review-commit-integrity/artifacts/A-001-review-service/requirements-report.md#L26-L28)).

For legacy data, the defensible compatibility rule is fail closed for an unproven approval/review intent, preserve the row/audit trail and record why it was suppressed, and emit the required error/diagnostic only where the stored immutable target makes it possible. Do not silently reinterpret a queued write using today’s PR head.

### Ordinary workflows that must survive

- **`/ask`:** it creates a solo session with reason `ask` ([planner.rs:193-198](crates/github-pr-controller/src/planner.rs#L193-L198)). Terminal dispatch deliberately bypasses round/findings/status/review and queues precisely one sanitized answer comment ([lib.rs:2087-2147](crates/github-pr-controller/src/lib.rs#L2087-L2147); [closing.rs:134-157](crates/github-pr-controller/src/closing.rs#L134-L157)). Do not require a findings block or surface an SHA-guard error here.
- **Normal `pull_request` and `/review`:** their target comes from the original webhook fingerprint or the pre-open server lookup; a later PR head is not a substitute ([planner.rs:328-354](crates/github-pr-controller/src/planner.rs#L328-L354), [lib.rs:977-1004](crates/github-pr-controller/src/lib.rs#L977-L1004)). Re-review/supersession continues by opening a distinct session.
- **Dismiss / waive / reopen:** commands are parsed before planning and open no session ([lib.rs:749-772](crates/github-pr-controller/src/lib.rs#L749-L772)). They require server-checked repo-write authority and re-read the current GitHub head ([lib.rs:2802-2863](crates/github-pr-controller/src/lib.rs#L2802-L2863)); their ledger mutation CASes `(repo, pr, stable_id, head_sha)` ([sqlite.rs:731-765](crates/github-pr-controller/src/store/sqlite.rs#L731-L765)), and waive is atomic with its waiver ([sqlite.rs:812-906](crates/github-pr-controller/src/store/sqlite.rs#L812-L906)). Preserve those semantics and their audit/reply behavior. Their separate, trusted-current-head approval path still needs explicit commit binding rather than a generalized findings-SHA gate.
- **Unparseable and reviewer-insufficient closes:** remain no-formal-review failures; their existing error projection is a compatibility anchor ([closing.rs:70-125](crates/github-pr-controller/src/closing.rs#L70-L125)).

## Focused verification plan (not executed)

Add focused tests adjacent to the existing unit/integration coverage; exercise both SQLite and Postgres where the store contract changes.

1. `closing.rs`: matching valid full-format target + findings SHA produces exactly comment, success status, and review intent carrying the same commit; absent block, absent/empty/malformed SHA, and mismatch produce no success/`APPROVE`, durable failure disposition, and comment/error status when target exists. Cover the deliberately selected SHA-anomalous `request_changes` policy separately.
2. `lib.rs` terminal dispatch: one valid terminal event produces one round, findings using the verified target, and one review intent; redelivery produces neither another round nor another intent. Pair bad SHA with reviewer-insufficient/unparseable results to prove neither gate restores approval.
3. `github.rs` plus drain fixture: assert the review request contains the commit-binding field; reject a 2xx response that omits/mismatches its commit identity. A lease-expiry replay must reject/adopt neither a foreign marker nor a controller-owned marker whose event or commit differs, and must adopt exactly the matching review.
4. Migration/store tests: pending/in-flight legacy `review` and success-status rows cannot send approval/success; they become durably observable safe failures without rewriting their meaning from current head. Test both the SQLite and Postgres migrations/implementations and the session-target conflict rule (same input accepted; any changed repo/pr/SHA refused).
5. Decision-path regression: a current-head authorised dismiss/waive that clears the final blocker preserves its ledger CAS/reply behavior and, under the explicit selected policy, submits/reconciles an approval bound to the live verified head; stale-head dismiss/waive/reopen remains no mutation/no approval. Keep `reopen`’s no-approve property ([deciding.rs:626-643](crates/github-pr-controller/src/deciding.rs#L626-L643)).
6. `/ask`: an answer, even with an accidental trailer/findings-like text, writes only its sanitized comment and no round/findings/status/review/guard diagnostic ([closing.rs:750-809](crates/github-pr-controller/src/closing.rs#L750-L809)).

Required package checks after implementation (not run in this pass):

```
cargo fmt --check
cargo clippy --package github-pr-controller --all-targets -- -D warnings
cargo test --package github-pr-controller
cargo test
```

## Shared coverage: existing-repository discovery + codewalk

### Verified paths and conventions

- `crates/github-pr-controller/src/closing.rs` is the pure terminal projection; its header explicitly separates deciding from fallible outbox delivery and names `(session_id, kind)` redelivery idempotency ([closing.rs:1-10](crates/github-pr-controller/src/closing.rs#L1-L10)).
- `crates/github-pr-controller/src/lib.rs` owns webhook ingress, signed runtime-event handling, terminal persistence/queueing, drain, receipts/audit, and finding command application. `github.rs` is the sole GitHub side-effect client; it is write-gated and calls are intended to fail closed ([github.rs:1-13](crates/github-pr-controller/src/github.rs#L1-L13)).
- `crates/github-pr-controller/src/store.rs`, `store/sqlite.rs`, and `store/postgres.rs` mirror a `ProductStore` trait with durable rounds, findings, waivers, and outbox. Any persisted integrity change must reach both implementations, not SQLite alone.
- Existing conventions worth retaining: signed ingress/runtime admission, controller ownership before marker reconciliation, closed review/status enums, per-session-kind idempotency, audit records keyed idempotently, and one GitHub side-effect owner. ADR 031 assigns provider ingress/product state/provider effects to this external controller and requires the single-writer boundary ([docs/adr/031-provider-neutral-kernel.md:1-96](docs/adr/031-provider-neutral-kernel.md#L1-L96)).

### Public boundaries

- GitHub webhook ingress: `POST /api/v1/github/webhooks` (router registration at [lib.rs:400-417](crates/github-pr-controller/src/lib.rs#L400-L417)).
- Signed runtime event ingress: `POST /api/v1/openab/events`; normal close is a `session.terminal` event accepted only for controller-owned session targets ([lib.rs:2049-2076](crates/github-pr-controller/src/lib.rs#L2049-L2076)).
- GitHub effects: issue comments, commit statuses, and formal PR reviews via `GitHubClient`; formal review is the branch-protection-sensitive boundary.
- Finding commands: signed GitHub issue-comment ingress leads to controlled ledger/status/review mutations, not a session.

### Likely edit locations

- `crates/github-pr-controller/src/closing.rs` — normal-council integrity classification and safe projection.
- `crates/github-pr-controller/src/verdict.rs` — chosen object-ID validator/typed parsed evidence, if it belongs at parse boundary; avoid making parser semantics accidentally enforce `/ask`.
- `crates/github-pr-controller/src/store.rs`, `src/store/sqlite.rs`, `src/store/postgres.rs` — immutable session target, integrity disposition, durable review intent/receipt, and legacy queued-row migration.
- `crates/github-pr-controller/src/github.rs` — explicit review commit request and response/reconcile identity verification.
- `crates/github-pr-controller/src/lib.rs` and `src/deciding.rs` — sender defense, auditing/receipt correlation, legacy handling, and independently defined decision-workflow binding.
- `crates/github-pr-controller/src/closing.rs`, `src/github.rs`, `src/lib.rs`, `src/store/{sqlite,postgres}.rs`, `src/deciding.rs` tests — focused regressions above.

### Facts vs inferences

Facts are the file:line claims above, including the fallback, no-commit review request, marker-only reconcile, persistence layout, and current decision workflow. Inferences are limited to (a) the resulting GitHub authorization risk, because real API commit-default behavior was not called/verified here, and (b) the appropriate legacy posture, because historical queued-row population was not inspected in a production database. Both follow directly from requirements that demand explicit binding/proof and prohibit current-head substitution.

### Unexamined areas / bounded unknowns

- No GitHub API documentation or live endpoint was queried, so the exact review request/response field names and semantics for commit binding remain to be confirmed.
- No real SQLite/Postgres database contents, deployment config, logs, credentials, webhook deliveries, or GitHub reviews were accessed; legacy-row discussion is schema/path-based.
- No source outside the external GitHub PR controller was traced deeply. The root OCP runtime emits signed terminal events but is not the current GitHub side-effect owner; this finding is bounded to controller terminal projection and its explicit sibling writers.
- I did not decide the requirements’ blocking-result anomaly policy, SHA-1-versus-future-object-ID policy, migration rollout, or any Stage 2/3 metric/comparison work.

Self-check: Frozen scope, output, model, effort and authority declared.
