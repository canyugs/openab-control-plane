# Tracker

## Identity

- **Work key:** A-001-review-service.

- **Active Ask:** A-001.

- **Goal:** Prepare and execute the approved three-stage review-service improvement through Agentflow.

- **Last update:** 2026-09-09 13:20:16 Asia/Taipei.

- **Evidence commit:** d6e96c32abfd766588c549e8922e3077d6abcb00.

## Overall state

- **State:** blocked.

- **Reason:** Exact implementation Result Go remains owner-only; all technical Stage 1 checks passed.

- **Total:** 3.

- **Completed:** 1.

- **Remaining:** 2.

## Accepted task checklist

- [x] **T-1:** Create a source-grounded design for SHA integrity, round measurement, and offline comparison; no runtime changes; prove requirements and invariant coverage. Source: A-001. Proof: design.md; spec-resolution.md; commit 8b35b4c581ac3d2f5f6669511eaa7b318310fc40.

- [ ] **T-2:** Implement SHA integrity only after the exact design gate; preserve ask/dismiss and retry workflows; prove malformed and mismatched SHA rejection, valid controls, and required Rust checks. Source: A-001.

- [ ] **T-3:** Add round measurement/reporting and offline comparison after their design gates; prove unknown cost handling and no production writes; retain human quality adjudication and rollout decisions. Source: A-001.

## Accepted scope changes

- None.

## Current recovery

- **Current item:** T-2.

- **Last proven result:** Host gate PASS and fresh full acceptance PASS for d6e96c3; no active workers; local Postgres stopped.

- **Active blocker or running process:** Awaiting owner Result Go for d6e96c32abfd766588c549e8922e3077d6abcb00; no process running.

- **Next safe action:** Record exact Result Go, then finish Stage 1 round and prepare separately gated Stage 2 design.

- **Expected changed files:** crates/github-pr-controller/src/{closing.rs,github.rs,lib.rs,deciding.rs,store.rs,store/sqlite.rs,store/postgres.rs}; stream notebook and artifacts.

## Completion proof

- **All accepted tasks checked:** no.

- **Blocking accepted decision:** Result Go for d6e96c32abfd766588c549e8922e3077d6abcb00.

- **Operation running:** no.

- **Next action remaining:** T-2.

- **Evidence status:** current.

- **Judgment:** blocked.

## Update meaning

- Saving this tracker is a recovery checkpoint, not a stop signal.

- Work continues with the next unfinished item unless an independent stop condition applies.
