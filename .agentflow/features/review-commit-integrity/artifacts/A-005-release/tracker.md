# Tracker

## Identity

- **Work key:** A-005-release.

- **Active Ask:** A-005.

- **Goal:** Merge the accepted branch and publish verified evaluation-v0.1.0 artifacts without deploying services.

- **Last update:** 2026-09-10 18:23:37 Asia/Taipei.

- **Evidence commit:** uncommitted.

## Overall state

- **State:** active.

- **Reason:** Work remains.

- **Total:** 3.

- **Completed:** 0.

- **Remaining:** 3.

## Accepted task checklist

- [ ] **T-1:** Merge PR418 at its checked head after required council and CI results; preserve source and user checkout; prove merged commit. Source: A-005.

- [ ] **T-2:** Publish only evaluation-v0.1.0 from the merged source; prove tag identity and successful release workflow, no Rust tag or deployment. Source: A-005.

- [ ] **T-3:** Independently read back release wheel/sdist and GHCR image, verify installed CLI and image identity, and record actual limits. Source: A-005.

## Accepted scope changes

- None.

## Current recovery

- **Current item:** T-1.

- **Last proven result:** Rust/Postgres CI PASS; council F1/F2 mutable build inputs confirmed, F3 PR wording clarified scope, F4 mtime claim has direct host proof.

- **Active blocker or running process:** Council changes requested; bounded image/build-lock correction required before merge.

- **Next safe action:** Pin resolved build inputs, clarify PR description, validate and rerun checks.

- **Expected changed files:** Dockerfile.evaluation; .github/workflows/evaluation-release.yml; packaging/evaluation/build-requirements.lock; docs/evaluation-package.md; stream records.

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
