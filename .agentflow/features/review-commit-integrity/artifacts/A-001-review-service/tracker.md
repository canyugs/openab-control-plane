# Tracker

## Identity

- **Work key:** A-001-review-service.

- **Active Ask:** A-001.

- **Goal:** Prepare and execute the approved three-stage review-service improvement through Agentflow.

- **Last update:** 2026-09-09 13:52:40 Asia/Taipei.

- **Evidence commit:** d6e96c32abfd766588c549e8922e3077d6abcb00.

## Overall state

- **State:** active.

- **Reason:** Stage 1 accepted; Stage 2/3 remain.

- **Total:** 3.

- **Completed:** 2.

- **Remaining:** 1.

## Accepted task checklist

- [x] **T-1:** Create a source-grounded design for SHA integrity, round measurement, and offline comparison; no runtime changes; prove requirements and invariant coverage. Source: A-001. Proof: design.md; spec-resolution.md; commit 8b35b4c581ac3d2f5f6669511eaa7b318310fc40.

- [x] **T-2:** Implement SHA integrity only after the exact design gate; preserve ask/dismiss and retry workflows; prove malformed and mismatched SHA rejection, valid controls, and required Rust checks. Source: A-001. Proof: acceptance-report.md; acceptance-resolution.md; d6e96c32abfd766588c549e8922e3077d6abcb00; Result-Go-RUN-027.

- [ ] **T-3:** Add round measurement/reporting and offline comparison after their design gates; prove unknown cost handling and no production writes; retain human quality adjudication and rollout decisions. Source: A-001.

## Accepted scope changes

- None.

## Current recovery

- **Current item:** T-3.

- **Last proven result:** Host gate PASS and fresh full acceptance PASS for d6e96c3; no active workers; local Postgres stopped.

- **Active blocker or running process:** Stage 2 spec correction external worker session 76454; same spec stage attempt 2.

- **Next safe action:** Inspect Stage 2 metric sources and freeze its minimal design before source work.

- **Expected changed files:** crates/github-pr-controller/src/{closing.rs,github.rs,lib.rs,deciding.rs,store.rs,store/sqlite.rs,store/postgres.rs}; stream notebook and artifacts.

## Completion proof

- **All accepted tasks checked:** no.

- **Blocking accepted decision:** none.

- **Operation running:** yes.

- **Next action remaining:** T-3.

- **Evidence status:** current.

- **Judgment:** active.

## Update meaning

- Saving this tracker is a recovery checkpoint, not a stop signal.

- Work continues with the next unfinished item unless an independent stop condition applies.
