# Tracker

## Identity

- **Work key:** A-001-review-service.

- **Active Ask:** A-001.

- **Goal:** Prepare and execute the approved three-stage review-service improvement through Agentflow.

- **Last update:** 2026-09-09 11:49:39 Asia/Taipei.

- **Evidence commit:** uncommitted.

## Overall state

- **State:** active.

- **Reason:** Work remains.

- **Total:** 3.

- **Completed:** 0.

- **Remaining:** 3.

## Accepted task checklist

- [ ] **T-1:** Create a source-grounded design for SHA integrity, round measurement, and offline comparison; no runtime changes; prove requirements and invariant coverage. Source: A-001.

- [ ] **T-2:** Implement SHA integrity only after the exact design gate; preserve ask/dismiss and retry workflows; prove malformed and mismatched SHA rejection, valid controls, and required Rust checks. Source: A-001.

- [ ] **T-3:** Add round measurement/reporting and offline comparison after their design gates; prove unknown cost handling and no production writes; retain human quality adjudication and rollout decisions. Source: A-001.

## Accepted scope changes

- None.

## Current recovery

- **Current item:** T-1.

- **Last proven result:** None.

- **Active blocker or running process:** None.

- **Next safe action:** Run external requirements and boundary discovery.

- **Expected changed files:** .agentflow/features/review-commit-integrity/artifacts/A-001-review-service/*; stream notebook; stream ag.json; .gitignore.

## Completion proof

- **All accepted tasks checked:** no.

- **Blocking accepted decision:** none.

- **Operation running:** no.

- **Next action remaining:** T-1.

- **Evidence status:** current.

- **Judgment:** active.

## Update meaning

- Saving this tracker is a recovery checkpoint, not a stop signal.

- Work continues with the next unfinished item unless an independent stop condition applies.
