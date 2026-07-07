#!/usr/bin/env bash
# Open a review council against a deployed OpenAB Review Council template.
#
# Usage:
#   PLANE=https://<your-domain> KEY=<OABCP_API_KEY> ./open-council.sh owner/repo#123
#   PLANE=https://<your-domain> KEY=<OABCP_API_KEY> ./open-council.sh "Free-text task to review"
#   PLANE=… KEY=… ./open-council.sh --watch owner/repo#123              # follow + print the verdict
#   PLANE=… KEY=… ./open-council.sh --preset quick owner/repo#123       # assign angles to reviewers
#   PLANE=… KEY=… ./open-council.sh --self-fetch owner/repo#123         # bots fetch the PR (no inline diff)
#
# Env:
#   PLANE   control-plane URL                              (required)
#   KEY     OABCP_API_KEY                                  (required)
#   ROSTER  JSON array of bot names (default ["chair","rev1","rev2"], matches OABCP_BOTS)
#   QUORUM  override quorum_n          (default: all participating reviewers)
#   MODE    override mode              (default: solo for 1-entry roster, else council)
#           Note: MODE=council on a PR-shaped trigger delivers no role protocol.
#   PRESET  quick|standard|full        (same as --preset; PR path only — assigns review angles)
#   SELF_FETCH =1 to send a pointer trigger; bots fetch the diff themselves (same as --self-fetch)
#   FOLLOW  =1 to stream + print the verdict (same as --watch)
#
# Needs: curl, node, and (for the PR form) gh authenticated locally.
# Deliberately depends on node (not jq/python3): node ships on GitHub runners AND
# in the dev sandbox, so the same script runs in CI and by hand.
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

# Flags (--watch / --preset) may precede the arg, in any order. Env does the same.
FOLLOW="${FOLLOW:-0}"
PRESET="${PRESET:-}"
SELF_FETCH="${SELF_FETCH:-0}"
while [[ "${1:-}" == -* ]]; do
  case "$1" in
    -w|--watch)       FOLLOW=1; shift ;;
    -p|--preset)      PRESET="${2:?--preset needs quick|standard|full}"; shift 2 ;;
    --preset=*)       PRESET="${1#*=}"; shift ;;
    -s|--self-fetch)  SELF_FETCH=1; shift ;;
    *) echo "unknown flag: $1" >&2; exit 2 ;;
  esac
done

: "${PLANE:?set PLANE to the control-plane URL}"
: "${KEY:?set KEY to OABCP_API_KEY}"
ARG="${1:?pass owner/repo#N or a quoted task string}"

ROSTER="${ROSTER:-[\"chair\",\"rev1\",\"rev2\"]}"   # override via env; matches OABCP_BOTS.
# A 1-entry roster auto-selects "solo" mode (a lone chair has no reviewers, so a
# council never reaches quorum); else "council" with quorum = all reviewers.

# Defaults: no preset → today's behaviour (generic reviewers, derived quorum/roster).
EFF_ROSTER="$ROSTER"
QUORUM_EFF="${QUORUM:-}"
ASSIGN_TEXT=""

# Build the trigger.
if [[ "$ARG" =~ ^([^/]+/[^#]+)#([0-9]+)$ ]]; then
  REPO="${BASH_REMATCH[1]}"; NUM="${BASH_REMATCH[2]}"
  TITLE=$(gh pr view "$NUM" --repo "$REPO" --json title -q .title)
  # --self-fetch: send a pointer trigger (the bots run `gh pr diff` themselves, so a
  # huge diff never bloats the broadcast) instead of embedding the diff. Needs the
  # bots to have GitHub read access (read-only scoped token / GH_TOKEN in the pod).
  if [[ "$SELF_FETCH" == "1" ]]; then
    TMPL_FILE="$SCRIPT_DIR/pr-review-trigger-pointer.tmpl"; DIFF=""
  else
    TMPL_FILE="$SCRIPT_DIR/pr-review-trigger.tmpl";         DIFF=$(gh pr diff "$NUM" --repo "$REPO")
  fi

  # --preset: assign review angles to reviewers. Round-robin angles → reviewers;
  # if angles < reviewers, the extras sit out (trimmed from this session's roster
  # so quorum doesn't wait on idle bots); if angles > reviewers, a bot covers
  # several. quorum = participating reviewers. PR path only.
  if [[ -n "$PRESET" ]]; then
    case "$PRESET" in
      lite)     ANGLES='["correctness"]' ;;
      quick)    ANGLES='["correctness","security","integration"]' ;;
      standard) ANGLES='["correctness","architecture","security","testing","docs"]' ;;
      full)     ANGLES='["correctness","architecture","security","testing","docs","performance","spec"]' ;;
      *) echo "unknown preset: $PRESET (want lite|quick|standard|full)" >&2; exit 2 ;;
    esac
    PLAN=$(ROSTER="$ROSTER" ANGLES="$ANGLES" node -e '
      const roster = JSON.parse(process.env.ROSTER);
      const angles = JSON.parse(process.env.ANGLES);
      const chair = roster[0];
      const reviewers = roster.slice(1);
      if (reviewers.length === 0) {       // solo / no reviewers — preset is a no-op
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
        const assignment = "Review focus assignment:\n" + lines;
        process.stdout.write(JSON.stringify({ roster: [chair, ...participating], quorum_n: participating.length, assignment }));
      }
    ')
    EFF_ROSTER=$(printf '%s' "$PLAN" | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>process.stdout.write(JSON.stringify(JSON.parse(s).roster)))')
    ASSIGN_TEXT=$(printf '%s' "$PLAN" | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>process.stdout.write(JSON.parse(s).assignment))')
    [[ -z "$QUORUM_EFF" ]] && QUORUM_EFF=$(printf '%s' "$PLAN" | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>process.stdout.write(String(JSON.parse(s).quorum_n)))')
  fi

  # Render the chosen template with named {{...}} placeholders (no positional %s, no
  # printf %-in-diff hazard). Steering lives in the template; the pointer template
  # simply has no {{DIFF}} so DIFF="" is a no-op there.
  TRIGGER=$(REPO="$REPO" NUM="$NUM" TITLE="$TITLE" DIFF="$DIFF" ANGLE_ASSIGNMENT="$ASSIGN_TEXT" TMPL="$TMPL_FILE" node -e '
    const fs = require("fs");
    let t = fs.readFileSync(process.env.TMPL, "utf8");
    for (const k of ["REPO", "NUM", "TITLE", "DIFF", "ANGLE_ASSIGNMENT"]) t = t.split("{{" + k + "}}").join(process.env[k]);
    process.stdout.write(t);
  ')
  REF="github:pr/$REPO#$NUM"
else
  [[ -n "$PRESET" ]] && echo "note: --preset ignored for free-text tasks (angles apply to a PR diff)" >&2
  # TRIGGER_REF lets a panel shim (e.g. open-triage.sh, ADR 014) supply a real
  # idempotency key; bare free-text councils keep the legacy "adhoc" ref.
  TRIGGER="$ARG"; REF="${TRIGGER_REF:-adhoc}"
fi

# Open the session (node builds the body: quorum_n + mode derived from the roster,
# overridable via QUORUM / MODE; --preset may have trimmed the roster + set quorum).
OPEN_BODY=$(ROSTER="$EFF_ROSTER" REF="$REF" QUORUM="$QUORUM_EFF" MODE="${MODE:-}" node -e '
  const r = JSON.parse(process.env.ROSTER);
  const quorum = process.env.QUORUM ? Number(process.env.QUORUM) : Math.max(0, r.length - 1);
  const isPR = /^github:pr\//.test(process.env.REF || "");
  const mode = process.env.MODE || (r.length === 1 ? "solo" : isPR ? "review_council" : "council");
  process.stdout.write(JSON.stringify({
    title: "council", trigger_ref: process.env.REF, roster: r,
    quorum_n: quorum, chair_bot: r[0], mode,
  }));
')
RESP=$(curl -s -X POST "$PLANE/v1/sessions" -H "Authorization: Bearer $KEY" -H 'Content-Type: application/json' \
  -d "$OPEN_BODY")
SID=$(printf '%s' "$RESP" | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{try{process.stdout.write(JSON.parse(s).session_id||"")}catch{}})')
DEDUPED=$(printf '%s' "$RESP" | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{try{process.stdout.write(JSON.parse(s).deduped ? "1" : "0")}catch{process.stdout.write("0")}})')
if [[ -z "$SID" ]]; then
  echo "error: session open failed (plane said: ${RESP:0:200})" >&2
  exit 1
fi
if [[ "$DEDUPED" == "1" ]]; then
  echo "already active: $SID"
  exit 0
fi
echo "session: $SID"
[[ -n "$ASSIGN_TEXT" ]] && echo "preset: $PRESET → $EFF_ROSTER"

# Post the trigger.
curl -s -X POST "$PLANE/v1/sessions/$SID/messages" -H "Authorization: Bearer $KEY" -H 'Content-Type: application/json' \
  -d "$(node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>process.stdout.write(JSON.stringify({content:s})))' <<<"$TRIGGER")" >/dev/null

STREAM_URL="$PLANE/v1/sessions/$SID/stream"
if [[ "$FOLLOW" != "1" ]]; then
  echo "trigger posted. Stream it:"
  echo "  curl -N $STREAM_URL -H \"Authorization: Bearer $KEY\""
  exit 0
fi

# --watch: follow the SSE stream, echo messages live, print the verdict, exit on close.
echo "trigger posted. Following… (Ctrl-C to detach; the council keeps running)"
set +o pipefail   # node exit on close sends SIGPIPE to curl — that's expected, not a failure.
curl -sN "$STREAM_URL" -H "Authorization: Bearer $KEY" | PLANE="$PLANE" SID="$SID" KEY="$KEY" node -e '
  // Wire format: the plane sends one JSON object per SSE `data:` line
  // ({type, session_id, payload, ts}) — NOT named SSE events. So switch on
  // o.type and read o.payload (see state.rs emit_north / api.rs stream_session).
  const { execSync } = require("child_process");
  let buf = "";
  function shQuote(s) {
    const q = String.fromCharCode(39);
    return q + String(s).replace(new RegExp(q, "g"), q + "\\" + q + q) + q;
  }
  function refetchSession() {
    const url = `${process.env.PLANE}/v1/sessions/${process.env.SID}`;
    const auth = `Authorization: Bearer ${process.env.KEY}`;
    const raw = execSync(`curl -s ${shQuote(url)} -H ${shQuote(auth)}`, { encoding: "utf8" });
    return JSON.parse(raw);
  }
  function recoveredVerdict(snapshot) {
    const messages = Array.isArray(snapshot.messages) ? snapshot.messages : [];
    const chair = snapshot.session && snapshot.session.chair_bot;
    const chairMessage = messages.slice().reverse().find(m => m.author_id === chair && typeof m.content === "string");
    const botMessage = messages.slice().reverse().find(m => m.author_kind === "bot" && typeof m.content === "string");
    return (chairMessage || botMessage || {}).content || "(closed; verdict text not found in fetched session)";
  }
  process.stdin.setEncoding("utf8");
  process.stdin.on("data", d => {
    buf += d;
    let i;
    while ((i = buf.indexOf("\n")) >= 0) {
      const line = buf.slice(0, i); buf = buf.slice(i + 1);
      if (!line.startsWith("data:")) continue;
      let o; try { o = JSON.parse(line.slice(5).trim()); } catch { continue; }
      const p = o.payload || {};
      if (o.type === "message") console.error(`  [${p.author}] ${String(p.content).replace(/\s+/g, " ").slice(0, 200)}`);
      else if (o.type === "verdict") console.log("\n===== VERDICT =====\n" + p.text + "\n===================");
      else if (o.type === "resync") {
        const snapshot = refetchSession();
        if (snapshot.session && snapshot.session.state === "closed") {
          console.log("\n===== VERDICT =====\n" + recoveredVerdict(snapshot) + "\n===================");
          process.exit(0);
        }
        console.error(`resync: ${p.skipped} events dropped, refetched — still open`);
      }
      else if (o.type === "state" && p.state === "closed") process.exit(0);
    }
  });
  // A clean close exits 0 above. Reaching EOF without it means the SSE stream was
  // cut — dead plane / dropped port-forward — NOT a finished council. Fail loud so
  // callers (and CI) do not read a severed stream as success. (C5a op-hazard.)
  process.stdin.on("end", () => {
    console.error("error: stream ended before the council closed — plane or port-forward may be down");
    process.exit(3);
  });
'
