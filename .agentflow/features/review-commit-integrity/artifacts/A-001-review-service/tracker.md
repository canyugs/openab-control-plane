# Tracker

## Identity

- **Work key:** A-001-review-service.

- **Active Ask:** A-002.

- **Goal:** Deliver the approved review-service design and Stages 1-2; retain Stage 3 as separately gated future work.

- **Last update:** 2026-09-10 11:22:44 Asia/Taipei.

- **Evidence commit:** 4b46e16c75430f9654b18599e319f978d5b0c3ac.

## Overall state

- **State:** complete.

- **Reason:** Stage2 Result Go received in A-002; all currently authorized delivery tasks complete. Stage3 remains future separately gated work.

- **Total:** 3.

- **Completed:** 3.

- **Remaining:** 0.

## Accepted task checklist

- [x] **T-1:** Create a source-grounded design for SHA integrity, round measurement, and offline comparison; no runtime changes; prove requirements and invariant coverage. Source: A-001. Proof: design.md; spec-resolution.md; commit 8b35b4c581ac3d2f5f6669511eaa7b318310fc40.

- [x] **T-2:** Implement SHA integrity only after the exact design gate; preserve ask/dismiss and retry workflows; prove malformed and mismatched SHA rejection, valid controls, and required Rust checks. Source: A-001. Proof: acceptance-report.md; acceptance-resolution.md; d6e96c32abfd766588c549e8922e3077d6abcb00; Result-Go-RUN-027.

- [x] **T-3:** Deliver automatic multi-model finding assessment, evidence validation, synthesis, escape discovery/validation and weekly metrics after the revised design gate; prove a real model journey, unknown/partial cost honesty, model-versus-human labels and no live GitHub effects; retain later separately gated council comparison. Source: A-001. Proof: stage2-acceptance-report.md; actual-journey-report.md;4b46e16; Result Go in A-002.

## Accepted scope changes

- Add independent multi-model finding assessment, synthesis, escape discovery/validation and measured costs. Source: A-001 owner additions. Effect: Reopen Stage 2 design; human annotation becomes optional and cost reduction cannot remove required functionality.

## Current recovery

- **Current item:** none.

- **Last proven result:** Stage2 reviewed4b46e16/source6e5784b;86 tests, security, acceptance and host gate PASS; full actual model/OCI/weekly/replay/tamper evidence verified; learn complete.

- **Active blocker or running process:** none.

- **Next safe action:** none.

- **Expected changed files:** scripts/review_model_{evaluation,adapters,oci_executor}.py; scripts/review_round_weekly_report.py; tests/test_review_model_{evaluation,oci_executor}.py; tests/test_review_round_weekly_report.py; tests/fixtures/model_evaluation/; docs/model-evaluation.md; docs/review-round-weekly-report.md; stream records.

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
