# Tracker

## Identity

- **Work key:** A-003-evaluation-package.

- **Active Ask:** A-003.

- **Goal:** Deliver the approved independent package, CLI, image and release workflow without changing evaluator semantics.

- **Last update:** 2026-09-10 13:31:12 Asia/Taipei.

- **Evidence commit:** 47b3c28.

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

- **Last proven result:** Actual candidate staged launcher exits0 and cleans scratch; clean CI backend correction and tag guards PASS; image/wheel/core evidence unchanged.

- **Active blocker or running process:** package-ownership98765; reusable PTY90890.

- **Next safe action:** Collect source staging repair, verify real launcher, full suite and independent reviews.

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
