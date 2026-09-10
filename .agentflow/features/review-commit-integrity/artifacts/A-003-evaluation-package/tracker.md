# Tracker

## Identity

- **Work key:** A-003-evaluation-package.

- **Active Ask:** A-003.

- **Goal:** Deliver the approved independent package, CLI, image and release workflow without changing evaluator semantics.

- **Last update:** 2026-09-10 13:42:20 Asia/Taipei.

- **Evidence commit:** 54a0c1a614e29de10ed7decec8cbe0e00e3da46f.

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

- **Last proven result:** Final94 tests/no skips, actual final launcher/cleanup/replay, isolated wheel, amd64 image/OCI/weekly and version guards PASS; core unchanged.

- **Active blocker or running process:** security77008 and full acceptance21213, read-only same exact source.

- **Next safe action:** Collect independent reviews, resolve within scope, then final records and push.

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
