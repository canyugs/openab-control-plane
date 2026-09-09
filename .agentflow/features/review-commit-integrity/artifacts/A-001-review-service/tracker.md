# Tracker

## Identity

- **Work key:** A-001-review-service.

- **Active Ask:** A-001.

- **Goal:** Prepare and execute the approved three-stage review-service improvement through Agentflow.

- **Last update:** 2026-09-09 13:57:57 Asia/Taipei.

- **Evidence commit:** 5f18484f24359e2e13278dc216e4462b8b1080df.

## Overall state

- **State:** blocked.

- **Reason:** Stage 1 accepted; exact Stage 2 Design Go is required before new source work.

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

- **Last proven result:** Stage 1 Result Go d6e96c3; Stage 2 design and source-backed resolution frozen in 5f18484.

- **Active blocker or running process:** No running process; awaiting Stage 2 Design Go for 5f18484f24359e2e13278dc216e4462b8b1080df.

- **Next safe action:** After exact Stage 2 Design Go, implement the three-file offline weekly report and its fixture/PTY checks.

- **Expected changed files:** crates/github-pr-controller/src/{closing.rs,github.rs,lib.rs,deciding.rs,store.rs,store/sqlite.rs,store/postgres.rs}; stream notebook and artifacts.

## Completion proof

- **All accepted tasks checked:** no.

- **Blocking accepted decision:** Stage 2 Design Go for 5f18484f24359e2e13278dc216e4462b8b1080df.

- **Operation running:** no.

- **Next action remaining:** T-3.

- **Evidence status:** current.

- **Judgment:** blocked.

## Update meaning

- Saving this tracker is a recovery checkpoint, not a stop signal.

- Work continues with the next unfinished item unless an independent stop condition applies.
