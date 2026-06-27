#!/usr/bin/env bash
# Open a review council against a deployed OpenAB Review Council template.
#
# Usage:
#   PLANE=https://<your-domain> KEY=<OABCP_API_KEY> ./open-council.sh owner/repo#123
#   PLANE=https://<your-domain> KEY=<OABCP_API_KEY> ./open-council.sh "Free-text task to review"
#
# Needs: curl, python3, and (for the PR form) gh authenticated locally.
set -euo pipefail

: "${PLANE:?set PLANE to the control-plane URL}"
: "${KEY:?set KEY to OABCP_API_KEY}"
ARG="${1:?pass owner/repo#N or a quoted task string}"

ROSTER='["chair","rev1","rev2"]'   # matches OABCP_BOTS in the template; edit to taste.
# A 1-entry roster auto-selects "solo" mode (a lone chair has no reviewers, so a
# council never reaches quorum); else "council" with quorum = all reviewers.

# Build the trigger.
if [[ "$ARG" =~ ^([^/]+/[^#]+)#([0-9]+)$ ]]; then
  REPO="${BASH_REMATCH[1]}"; NUM="${BASH_REMATCH[2]}"
  TITLE=$(gh pr view "$NUM" --repo "$REPO" --json title -q .title)
  DIFF=$(gh pr diff "$NUM" --repo "$REPO")
  TRIGGER=$(printf 'PR Review Council — %s #%s "%s"\n\nReview the diff below.\n\nReviewers: post your findings in THIS thread only — do NOT run gh. End your final message with the token [done].\n\nChair (you alone have gh): maintain EXACTLY ONE comment on %s #%s. Always write it with:\n  gh pr comment %s --repo %s --edit-last --create-if-none --body "..."\nThat one command creates the comment the first time and edits the SAME comment every time after, so the PR never accumulates duplicates. Post it once as in-progress, then overwrite that same comment with the synthesized verdict as reviewers finish — never run a plain `gh pr comment` (without --edit-last) a second time. End your final message with [done].\n\nThe [done] token (on its own, at the end of your last message) is how the council records that you are finished — it is what closes the session. Send it exactly once, when truly done.\n\n===== DIFF =====\n%s\n===== END DIFF =====\n' "$REPO" "$NUM" "$TITLE" "$REPO" "$NUM" "$NUM" "$REPO" "$DIFF")
  REF="github:pr/$REPO#$NUM"
else
  TRIGGER="$ARG"; REF="adhoc"
fi

# Open the session.
SID=$(curl -s -X POST "$PLANE/v1/sessions" -H "Authorization: Bearer $KEY" -H 'Content-Type: application/json' \
  -d "$(python3 -c 'import json,sys; r=json.loads(sys.argv[1]); print(json.dumps({"title":"council","trigger_ref":sys.argv[2],"roster":r,"quorum_n":max(0,len(r)-1),"chair_bot":r[0],"mode":"solo" if len(r)==1 else "council"}))' "$ROSTER" "$REF")" \
  | python3 -c 'import sys,json;print(json.load(sys.stdin)["session_id"])')
echo "session: $SID"

# Post the trigger.
curl -s -X POST "$PLANE/v1/sessions/$SID/messages" -H "Authorization: Bearer $KEY" -H 'Content-Type: application/json' \
  -d "$(python3 -c 'import json,sys;print(json.dumps({"content":sys.stdin.read()}))' <<<"$TRIGGER")" >/dev/null
echo "trigger posted. Stream it:"
echo "  curl -N $PLANE/v1/sessions/$SID/stream -H \"Authorization: Bearer $KEY\""
