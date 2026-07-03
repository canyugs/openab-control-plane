#!/usr/bin/env bash
# Reference triage shim (ADR 014). Renders the triage trigger from
# triage-trigger.tmpl and convenes a council through the generic
# POST /v1/sessions — the plane carries zero triage-specific code.
#
# Usage:
#   PLANE=… KEY=… ./open-triage.sh --ref forum:zeabur/12345 --title "crashloop after upgrade" < ticket.txt
#   … ./open-triage.sh --ref … --title … --watch                # follow + print the report
#   … ./open-triage.sh --ref … --title … --angles symptoms,config
#   … ./open-triage.sh --ref … --title … --render-only          # print the trigger, call nothing
#
# Env: PLANE, KEY (required unless --render-only); ROSTER (default
# ["chair","rev1","rev2"], [0] is the chair). Ticket body on stdin.
# Idempotency: trigger_ref = "triage:<ref>" — a re-delivered ticket while the
# council is active is rejected by the plane's active-trigger uniqueness.
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

REF=""; TITLE=""; ANGLES_CSV="symptoms,config,account,history"
FOLLOW=0; RENDER_ONLY=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --ref)         REF="${2:?--ref needs a ticket ref}"; shift 2 ;;
    --title)       TITLE="${2:?--title needs text}"; shift 2 ;;
    --angles)      ANGLES_CSV="${2:?--angles needs a,b,c}"; shift 2 ;;
    -w|--watch)    FOLLOW=1; shift ;;
    --render-only) RENDER_ONLY=1; shift ;;
    *) echo "unknown flag: $1" >&2; exit 2 ;;
  esac
done
[[ -n "$REF" ]] || { echo "error: --ref is required" >&2; exit 2; }
[[ -n "$TITLE" ]] || { echo "error: --title is required" >&2; exit 2; }
BODY=$(cat)
[[ -n "$BODY" ]] || { echo "error: ticket body expected on stdin" >&2; exit 2; }

ROSTER="${ROSTER:-[\"chair\",\"rev1\",\"rev2\"]}"

# Round-robin angles onto reviewers (same policy as open-council.sh presets):
# fewer angles than reviewers → extras sit out; more → a bot covers several.
PLAN=$(ROSTER="$ROSTER" ANGLES_CSV="$ANGLES_CSV" node -e '
  const roster = JSON.parse(process.env.ROSTER);
  const angles = process.env.ANGLES_CSV.split(",").map(s => s.trim()).filter(Boolean);
  const chair = roster[0];
  const reviewers = roster.slice(1);
  if (reviewers.length === 0 || angles.length === 0) {
    process.stdout.write(JSON.stringify({ roster, quorum_n: 0, assignment: "" }));
  } else {
    const assign = {};
    let participating;
    if (angles.length <= reviewers.length) {
      participating = reviewers.slice(0, angles.length);
      angles.forEach((a, i) => { assign[participating[i]] = [a]; });
    } else {
      participating = reviewers.slice();
      angles.forEach((a, i) => { (assign[participating[i % participating.length]] ||= []).push(a); });
    }
    const lines = participating.map(r => `- ${r} → ${assign[r].join(", ")}`).join("\n");
    process.stdout.write(JSON.stringify({
      roster: [chair, ...participating],
      quorum_n: participating.length,
      assignment: "Investigation angle assignment:\n" + lines + `\n- ${chair} → chair: synthesis only, no investigation`,
    }));
  }
')
EFF_ROSTER=$(printf '%s' "$PLAN" | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>process.stdout.write(JSON.stringify(JSON.parse(s).roster)))')
QUORUM=$(printf '%s' "$PLAN" | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>process.stdout.write(String(JSON.parse(s).quorum_n)))')
ASSIGN_TEXT=$(printf '%s' "$PLAN" | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>process.stdout.write(JSON.parse(s).assignment))')

CHAIR=$(printf '%s' "$EFF_ROSTER" | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>process.stdout.write(JSON.parse(s)[0]))')

TRIGGER=$(TICKET_REF="$REF" TITLE="$TITLE" BODY="$BODY" ANGLE_ASSIGNMENT="$ASSIGN_TEXT" CHAIR="$CHAIR" \
  TMPL="$SCRIPT_DIR/triage-trigger.tmpl" node -e '
  const fs = require("fs");
  let t = fs.readFileSync(process.env.TMPL, "utf8");
  // Single pass: replacement values are never re-scanned, so a {{CHAIR}} inside
  // the untrusted TITLE/BODY stays literal (council finding on #71).
  t = t.replace(/\{\{(TICKET_REF|TITLE|BODY|ANGLE_ASSIGNMENT|CHAIR)\}\}/g, (_, k) => process.env[k]);
  process.stdout.write(t);
')

if [[ "$RENDER_ONLY" == "1" ]]; then
  printf '%s\n' "$TRIGGER"
  exit 0
fi

# triage_council = QuorumCouncil mechanics + text-[done] chair (a prompt-driven
# chair's auto-🆗 ack must not close the session — found in the first dogfood).
exec env TRIGGER_REF="triage:$REF" MODE=triage_council ROSTER="$EFF_ROSTER" QUORUM="$QUORUM" \
  FOLLOW="$FOLLOW" "$SCRIPT_DIR/open-council.sh" "$TRIGGER"
