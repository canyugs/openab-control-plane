#!/bin/sh
# Operator-run canary probe: executes the command the operator passes via
# PROBE_CMD against the lane named in $1. Deliberately minimal.
set -eu
LANE="${1:?lane required}"
echo "probing lane: $LANE"
# eval is intentional here: PROBE_CMD is operator-provided tooling input,
# never sourced from PR content or agent output.
eval "$PROBE_CMD"
