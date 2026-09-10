# Tracker

## Identity

- **Work key:** A-003-evaluation-package.

- **Active Ask:** A-003.

- **Goal:** Deliver the approved independent package, CLI, image and release workflow without changing evaluator semantics.

- **Last update:** 2026-09-10 12:55:26 Asia/Taipei.

- **Evidence commit:** uncommitted.

## Overall state

- **State:** active.

- **Reason:** Work remains.

- **Total:** 1.

- **Completed:** 0.

- **Remaining:** 1.

## Accepted task checklist

- [ ] **T-1:** Package the four existing evaluation/report modules with installable commands, separate version and Docker/release configuration; preserve evaluator behavior and external credentials/data; prove isolated wheel installation, real container controls, applicable tests and independent review. Source: A-003.

## Accepted scope changes

- None.

## Current recovery

- **Current item:** T-1.

- **Last proven result:** Candidate wheel installed outside checkout; actual PTY weekly exact match and complete replay42 files unchanged. Candidate image and launcher/CI issues reproduced, pending scoped repair.

- **Active blocker or running process:** package-implementation78052; reusable verification PTY90890.

- **Next safe action:** Implement the approved packaging in a disposable clone and verify delivered artifacts.

- **Expected changed files:** pyproject.toml; packaging/evaluation/; Dockerfile.evaluation; .github/workflows/evaluation-release.yml; docs/evaluation-package.md; package-specific tests; stream records.

## Completion proof

- **All accepted tasks checked:** no.

- **Blocking accepted decision:** none.

- **Operation running:** yes.

- **Next action remaining:** T-1.

- **Evidence status:** current.

- **Judgment:** active.

## Update meaning

- Saving this tracker is a recovery checkpoint, not a stop signal.

- Work continues with the next unfinished item unless an independent stop condition applies.
