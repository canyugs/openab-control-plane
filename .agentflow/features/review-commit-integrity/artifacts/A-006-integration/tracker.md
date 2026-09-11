# Tracker

## Identity

- **Work key:** A-006-integration.

- **Active Ask:** A-006.

- **Goal:** Connect read-only review capture to standalone evaluation and reports.

- **Last update:** 2026-09-11 18:39:19 Asia/Taipei.

- **Evidence commit:** 96f329d current corrected bridge.

## Overall state

- **State:** active.

- **Reason:** Work remains.

- **Total:** 3.

- **Completed:** 0.

- **Remaining:** 3.

## Accepted task checklist

- [ ] **T-1:** Implement capture/prepare/run bridge with existing contracts only, prove red/green and focused tests. Source: A-006. Proof: bridge-report.md; cursor-fix-report.md; final-python-tests.json; source96f329d.

- [ ] **T-2:** Run complete relevant tests and real-data/PTY journey without production writes; retain actual coverage and limits. Source: A-006. Proof: final-python-tests.json; final-pty-readback.json; candidate-equivalence.json; live-pilot-summary.json; rust-tests.json; rust-clippy.json.

- [ ] **T-3:** Independently review exact source, commit and push reviewable delivery; no deployment. Source: A-006.

## Accepted scope changes

- None.

## Current recovery

- **Current item:** T-1.

- **Last proven result:** Real candidate PTY capture/prepare/evaluate/report completed; 2 findings, 9 successful model calls; private evidence retained. Proof: live-pilot-summary.json.

- **Active blocker or running process:** Weekly verification gate repair required after current review; prior real pilot remains verified.

- **Next safe action:** Repair unverified-output weekly gate and recheck exact source.

- **Expected changed files:** scripts/review_evaluation_bridge.py; tests/test_review_evaluation_bridge.py; docs/evaluation-integration.md; active stream records.

## Completion proof

- **All accepted tasks checked:** no.

- **Blocking accepted decision:** none.

- **Operation running:** yes.

- **Next action remaining:** T-1, T-2, T-3.

- **Evidence status:** current.

- **Judgment:** active.

## Update meaning

- Saving this tracker is a recovery checkpoint, not a stop signal.

- Work continues with the next unfinished item unless an independent stop condition applies.
