# Tracker

## Identity

- **Work key:** A-005-release.

- **Active Ask:** A-005.

- **Goal:** Merge the accepted branch and publish verified evaluation-v0.1.0 artifacts without deploying services.

- **Last update:** 2026-09-11 10:02:49 Asia/Taipei.

- **Evidence commit:** 4b9ea56f0fd7466685ee57b196f6947c7de713a7.

## Overall state

- **State:** complete.

- **Reason:** Approved merge and publication verified.

- **Total:** 3.

- **Completed:** 3.

- **Remaining:** 0.

## Accepted task checklist

- [x] **T-1:** Merge PR418 at its checked head after required council and CI results; preserve source and user checkout; prove merged commit. Source: A-005. Proof: merge-readback.json; pr-ready-to-merge.json; pr-ci-round2.json; council-round2.json; merge e33f77c78c4137abd7104f2e0b6acb667f0c4a5d.

- [x] **T-2:** Publish only evaluation-v0.1.0 from the merged source; prove tag identity and successful release workflow, no Rust tag or deployment. Source: A-005. Proof: published-tag.txt; release-workflow.json; published-release.json.

- [x] **T-3:** Independently read back release wheel/sdist and GHCR image, verify installed CLI and image identity, and record actual limits. Source: A-005. Proof: published-wheel-install.json; published-image-pull.json; published-image-readback.json.

## Accepted scope changes

- None.

## Current recovery

- **Current item:** none.

- **Last proven result:** PR418 merged after CI/council/cross-check PASS; merged tree equals checked4b9ea56; evaluation-v0.1.0 tag pushed at e33f77c.

- **Active blocker or running process:** none.

- **Next safe action:** none.

- **Expected changed files:** Dockerfile.evaluation; .github/workflows/evaluation-release.yml; packaging/evaluation/build-requirements.lock; docs/evaluation-package.md; stream records.

## Completion proof

- **All accepted tasks checked:** yes.

- **Blocking accepted decision:** none.

- **Operation running:** no.

- **Next action remaining:** none.

- **Evidence status:** complete.

- **Judgment:** complete.

## Update meaning

- Saving this tracker is a recovery checkpoint, not a stop signal.

- Work continues with the next unfinished item unless an independent stop condition applies.
