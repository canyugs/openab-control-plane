<!-- Delete sections that do not apply. -->

## What

## Steering changes only (ADR 032 P5 — measured steering)

A PR touching `docs/steering/` must declare its metric before merge:

- **Metric this change should move**: <!-- e.g. actionable rate of the
  `correctness` class on zeabur/nuphos -->
- **Current value** (cite `ops data/actionable-history.jsonl` date):
- **Expected direction**:
- **Check back**: after ~2 weeks of rounds, `actionable-rate.py` shows the
  delta; a steering change that moved nothing gets reverted or rewritten.
