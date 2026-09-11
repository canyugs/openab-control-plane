* _2026-09-10 00:05:13 (gpt-5.6-luna/max)_

# latency-control

Result: fixed incomplete terminal-projection latency without changing reliability categories or denominator logic.

Authority and scope:
- Exact source commit: `1b14ceef976cefb75c1c62e2a615b996f9e73203`.
- Write authority: `scripts/review_round_weekly_report.py`, `tests/test_review_round_weekly_report.py`, root `latency-control-report.md`, and root `red-correction.patch`.
- Owner Design Go: `1107f73567710159c298ff6a60df393e8e6b9775`.
- Read and applied E-7 from the A-001 round-measurement design and spec resolution, plus the supplied initial PTY JSON readback.
- No commit, push, delegation, Agentflow call, network, live service, model, Docker, credential, notebook, settings, or hook access.

Observed defect:
- The supplied initial readback showed `timeout-noop` as `pending_or_unknown` with `tombstone_visible: false`, but `latency.status: observed` and `duration_ms: 290`.
- Its successful `comment_abandon` receipt had a null provider `comment_id`; E-7 treats that as a no-op, not a visible terminal projection.

Change:
- `_tombstone_binding` now supplies proof to latency only when the bound provider tombstone is visible. A non-visible/null-comment receipt becomes unavailable latency.
- Visible tombstones retain their original/reconciliation qualification and clock checks. Superseded classification remains `superseded`; timeout classification remains unchanged.

Red/green evidence:
- Red, before the source edit: `python3 -m unittest tests.test_review_round_weekly_report.WeeklyReportBehaviorTests.test_incomplete_projection_latency_is_unavailable_for_timeout_and_supersession` — 1 test failed with `'observed' != 'unknown'`.
- Green, after the source edit: the same focused test ran 1 test and passed.
- Full weekly suite: `python3 -m unittest tests.test_review_round_weekly_report` — 14 tests passed.
- `python3 -m compileall -q scripts/review_round_weekly_report.py` — passed.
- `git diff --check` — passed.
- `red-correction.patch` was verified to byte-match the tests-only diff.
- Regression coverage preserves normal original-receipt timing, visible timeout timing (290 ms, original receipt), reconciliation upper-bound timing (292 ms), negative-clock invalidity, and superseded/timeout category behavior.
- No post-fix real PTY readback was run; the parent will re-run it after import as requested.

Self-check: Exact source, authority, scope, model, effort, result and evidence contract frozen.

