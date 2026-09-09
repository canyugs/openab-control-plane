# Tracker

## Identity

- **Work key:** A-001-review-service.

- **Active Ask:** A-001.

- **Goal:** Prepare and execute the approved three-stage review-service improvement through Agentflow.

- **Last update:** 2026-09-10 01:48:04 Asia/Taipei.

- **Evidence commit:** 1107f73567710159c298ff6a60df393e8e6b9775.

## Overall state

- **State:** blocked.

- **Reason:** Stage2 implementation and independent acceptance passed; awaiting exact owner Result Go. Stage3 remains separately gated.

- **Total:** 3.

- **Completed:** 2.

- **Remaining:** 1.

## Accepted task checklist

- [x] **T-1:** Create a source-grounded design for SHA integrity, round measurement, and offline comparison; no runtime changes; prove requirements and invariant coverage. Source: A-001. Proof: design.md; spec-resolution.md; commit 8b35b4c581ac3d2f5f6669511eaa7b318310fc40.

- [x] **T-2:** Implement SHA integrity only after the exact design gate; preserve ask/dismiss and retry workflows; prove malformed and mismatched SHA rejection, valid controls, and required Rust checks. Source: A-001. Proof: acceptance-report.md; acceptance-resolution.md; d6e96c32abfd766588c549e8922e3077d6abcb00; Result-Go-RUN-027.

- [ ] **T-3:** Deliver automatic multi-model finding assessment, evidence validation, synthesis, escape discovery/validation and weekly metrics after the revised design gate; prove a real model journey, unknown/partial cost honesty, model-versus-human labels and no live GitHub effects; retain later separately gated council comparison. Source: A-001.

## Accepted scope changes

- Add independent multi-model finding assessment, synthesis, escape discovery/validation and measured costs. Source: A-001 owner additions. Effect: Reopen Stage 2 design; human annotation becomes optional and cost reduction cannot remove required functionality.

## Current recovery

- **Current item:** T-3.

- **Last proven result:** Stage2 reviewed4b46e16/source6e5784b;86 tests, security, acceptance and host gate PASS; full actual model/OCI/weekly/replay/tamper evidence verified; learn complete.

- **Active blocker or running process:** No process running; owner Result Go for4b46e16c75430f9654b18599e319f978d5b0c3ac pending.

- **Next safe action:** Receive exact Result Go; keep Stage3 experiment under its separate design gate.

- **Expected changed files:** scripts/review_model_{evaluation,adapters,oci_executor}.py; scripts/review_round_weekly_report.py; tests/test_review_model_{evaluation,oci_executor}.py; tests/test_review_round_weekly_report.py; tests/fixtures/model_evaluation/; docs/model-evaluation.md; docs/review-round-weekly-report.md; stream records.

## Completion proof

- **All accepted tasks checked:** no.

- **Blocking accepted decision:** Result Go for4b46e16c75430f9654b18599e319f978d5b0c3ac.

- **Operation running:** no.

- **Next action remaining:** T-3.

- **Evidence status:** current.

- **Judgment:** blocked.

## Update meaning

- Saving this tracker is a recovery checkpoint, not a stop signal.

- Work continues with the next unfinished item unless an independent stop condition applies.
