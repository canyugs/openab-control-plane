# Review commit integrity

The GitHub controller owns verdict publication. Agent text alone cannot
authorize a review: the terminal event must meet the stored reviewer minimum,
and its findings block must explicitly identify the same full Git SHA as the
controller's stored session target. SHA comparison normalizes hex case, but
does not accept prefixes, whitespace, placeholders, or missing values.

If provenance is invalid, the controller posts a diagnostic comment and an
error status on a valid trusted target SHA. It submits no formal review,
imports no findings, and fires no waivers. An invalid trusted target permits
only a diagnostic comment. Ordinary ask sessions still post plain answers.

Formal review writes carry `commit_id`; GitHub's response must echo both the
requested review state and commit. Reconciliation after a crash requires an
App-owned round marker at the same commit and state. A conflicting marked
review produces an error rather than being adopted or blindly duplicated.
New pushes do not retarget an old round's review or status.

The sender repeats target checks before network access. It requires the
original closing review's validated provenance, including for later finding
decisions. This intentionally prevents an unverified historical round from
being turned into an approval by dismissing its findings. Re-review that
commit to establish valid provenance first.

## Upgrade behavior

Pre-upgrade pending verdict writes do not carry the new provenance fields.
They are retained in `github_writes` with `state = 'blocked'`, a reason in
`last_error`, and a `github.write.blocked` audit event. They are not sent,
marked successful, or retried after claim-lease expiry. Diagnostic comments
and error statuses remain deliverable. Existing completed GitHub artifacts
are not automatically corrected by an upgrade.

Before deployment, inventory pending verdict writes and prepare a scoped
re-review/recovery list. Do not synthesize provenance fields to release old
rows. If rollback is necessary, do not restore unsafe verdict writes merely
to restore service availability; prepare a separate write-containment step.

## Validation

Controller tests cover parser/closing rejection, signed terminal projection,
legacy outbox blocking, destination tampering, explicit review payloads,
response commit mismatch, and retry conflicts. PostgreSQL tests require an
isolated `TEST_POSTGRES_URL`; without it, PostgreSQL tests return early.
Never point those tests at an operational database: they recreate test schemas.

SHA equality establishes commit binding, not proof that the model read the
code. Role steering, required repository reads, and reviewer evidence remain
necessary and are separate from this publication boundary.
