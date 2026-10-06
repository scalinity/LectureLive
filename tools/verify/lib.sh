#!/bin/bash
# Shared helpers for the autonomous verification harness (tools/verify/run.sh). Sourced, never run.
#
# Rules the helpers keep, because the harness runs while no one watches:
#   - it only ever signals processes it started itself, by pid, never by name or pattern;
#   - it never speaks into BlackHole while a real lecture (the Python tool) could be recording it;
#   - it stops at its deadline and when its spending passes its cap.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
LL="${LECTURELIVE_BIN:-$ROOT/target/debug/lecturelive}" # LECTURELIVE_BIN: another build, e.g. one made beside a running class
APPBIN="$ROOT/target/debug/desktop"
NETCUT="$ROOT/target/debug/examples/netcut"
DATA="$HOME/Library/Application Support/LectureLive"
STAMP="${STAMP:-$(date +%Y%m%d-%H%M%S)}"
OUT="${OUT:-$DATA/verify/$STAMP}"
FWD="127.0.0.1:8445"
FWD_CTL="$OUT/forwarder.control"
COST_CAP="${COST_CAP:-1.50}"
mkdir -p "$OUT"
RESULTS="$OUT/results.tsv"
HLOG="$OUT/harness.log"
CHILDREN=()
STARTED_EPOCH=$(date +%s)

log() { printf '%s  %s\n' "$(date '+%H:%M:%S')" "$*" | tee -a "$HLOG"; }

# result ID STATUS NOTE: PASS, FAIL, PARTIAL (some of it could not be shown), BLOCKED (could not run), SKIPPED.
result() { printf '%s\t%s\t%s\n' "$1" "$2" "$3" >> "$RESULTS"; log "RESULT $1 $2: $3"; }

deadline_epoch() { date -j -f "%Y-%m-%d %H:%M" "$(date +%Y-%m-%d) $1" +%s; }
time_left() { echo $(( DEADLINE_EPOCH - $(date +%s) )); }
need_time() { [ "$(time_left)" -ge "$1" ]; }

# Spent so far on the harness's own synthetic lectures (courses m71-*), in dollars.
spent() { jq -s '[.[] | select((.course // "") | startswith("m71-")) | .usd] | add // 0' "$DATA/spend.jsonl" 2>/dev/null || echo 0; }
within_budget() { awk -v s="$(spent)" -v c="$COST_CAP" 'BEGIN{exit !(s < c)}'; }

# A real lecture records BlackHole; synthetic speech must never reach it.
audio_ok() { ! pgrep -f '\.local/bin/lecture' >/dev/null && ! pgrep -f 'live_notes' >/dev/null; }
say_bh() {
  audio_ok || { log "  BLOCKED: a real lecture is running; not speaking into BlackHole"; BLOCKED=1; return 1; }
  say -a "BlackHole 2ch" -v Samantha -r 165 "$1"
}

wait_until() { # seconds, what, command (a string, evaluated)
  local limit=$1 what=$2 cmd=$3 end=$(( $(date +%s) + $1 ))
  until eval "$cmd"; do
    [ "$(date +%s)" -ge "$end" ] && { log "  timed out waiting for $what"; return 1; }
    sleep 0.5
  done
}

# ---- a lecture folder and the CLI running on it -----------------------------------------------------------
new_lecture() { # stage name
  STAGE="$1"; EVID=(); STAGE_FAIL=0; BLOCKED=0; PARTIAL=0; CLI_ENV=(); CLI_SRC=(--device BlackHole)
  FOLDER="$OUT/$1/m71-$1/Weeks/Week 01"; mkdir -p "$FOLDER"
  CLI_LOG="$OUT/$1.cli.log"; : > "$CLI_LOG"
}

cli_start() { # extra lecture arguments
  local fifo="$OUT/$STAGE.in"
  [ -p "$fifo" ] || mkfifo "$fifo"
  exec 3<>"$fifo" # held open for writing, so the CLI never reads end of input
  echo "--- session $(date +%H:%M:%S) ---" >> "$CLI_LOG"
  env "${CLI_ENV[@]}" "$LL" lecture --plain "${CLI_SRC[@]}" --dir "$FOLDER" "$@" < "$fifo" >> "$CLI_LOG" 2>&1 &
  CLI_PID=$!
  CHILDREN+=("$CLI_PID")
}
cli_cmd() { echo "$1" >&3; }
cli_alive() { kill -0 "$CLI_PID" 2>/dev/null; }
# Only ever the process this harness started: checked by its command line before any signal.
cli_signal() {
  ps -o command= -p "$CLI_PID" 2>/dev/null | grep -q "$LL" || { log "  refusing to signal pid $CLI_PID: not the harness's CLI"; return 1; }
  kill "-$1" "$CLI_PID"
}
cli_wait_exit() { # seconds
  wait_until "$1" "the CLI to exit" '! cli_alive' || return 1
  wait "$CLI_PID" 2>/dev/null; CLI_RC=$?
}
cli_close() { exec 3>&-; }

log_has() { grep -qi -- "$1" "$CLI_LOG"; }
tx() { cat "$FOLDER"/lecture_transcript_*.txt 2>/dev/null; }
tx_has() { tx | grep -qi -- "$1"; }
sidecar() { jq -c "$1" "$FOLDER"/.live_notes/*.v2.json 2>> "$HLOG"; } # a jq or missing-file error goes to the log, not away: an empty answer is not "0"
wavs() { ls "$FOLDER"/recordings/*.wav 2>/dev/null | wc -l | tr -d ' '; }
audit() { AUDIT_OUT=$("$LL" lecture --dir "$FOLDER" audit 2>&1); AUDIT_RC=$?; echo "$AUDIT_OUT" > "$OUT/$STAGE.audit.txt"; }
# What a lecture cost, by kind (transcribe or notes), from the ledger.
cost_of() { jq -s --arg c "m71-$STAGE" --arg w "$1" '[.[] | select(.course == $c and .what == $w) | .usd] | add // 0' "$DATA/spend.jsonl"; }
audio_seconds() { jq -s --arg c "m71-$STAGE" '[.[] | select(.course == $c and .what == "transcribe") | .audio_s // 0] | add // 0' "$DATA/spend.jsonl"; }

# check "what it shows" 'shell test': records one line of evidence; any failure fails the stage.
check() { local what=$1; shift; if eval "$@"; then EVID+=("ok    $what"); else EVID+=("FAIL  $what"); STAGE_FAIL=1; fi; }
note() { EVID+=("note  $*"); }
finish_stage() { # ids (space separated), summary
  printf '%s\n' "${EVID[@]}" > "$OUT/$STAGE.evidence.txt"
  local status=PASS; [ "$PARTIAL" = 1 ] && status=PARTIAL; [ "$STAGE_FAIL" = 1 ] && status=FAIL; [ "$BLOCKED" = 1 ] && status=BLOCKED
  local id; for id in $1; do result "$id" "$status" "$2 (see $STAGE.evidence.txt)"; done
  printf '%s\n' "${EVID[@]}" | sed 's/^/    /' | tee -a "$HLOG" >/dev/null
  cli_close
}

# ---- the test forwarder: the harness's own, on its own port, steered by a file -------------------------------
forwarder_start() {
  FORWARDER_OK=0
  local p
  for p in $(seq 8450 8499); do # a free port of its own: a second run must never share (or fail to bind) another run's
    lsof -nP -iTCP:$p -sTCP:LISTEN > /dev/null 2>&1 && continue
    FWD="127.0.0.1:$p"; echo up > "$FWD_CTL"
    "$NETCUT" "$FWD" api.x.ai:443 "$FWD_CTL" > "$OUT/forwarder.log" 2>&1 &
    FWD_PID=$!; CHILDREN+=("$FWD_PID"); sleep 1
    kill -0 "$FWD_PID" 2>/dev/null && break
  done
  forwarder_selftest
}
# A forwarder that cannot cut the link makes every network check pass without testing anything, so it is proved first:
# an unauthenticated request to the real API host answers when up, and gets nothing when held or refused.
forwarder_selftest() {
  local a b c; probe() { curl --connect-to "api.x.ai:443:$FWD" -sS -m 4 -o /dev/null -w "%{http_code}" https://api.x.ai/v1/models 2> /dev/null; }
  echo up > "$FWD_CTL"; sleep 0.4; local i; for i in 1 2 3; do a=$(probe); [ -n "$a" ] && [ "$a" != 000 ] && break; sleep 1; done # a slow first handshake is not a broken forwarder
  echo hold > "$FWD_CTL"; sleep 0.4; b=$(probe)
  echo refuse > "$FWD_CTL"; sleep 0.4; c=$(probe)
  echo up > "$FWD_CTL"; sleep 0.4
  [ -n "$a" ] && [ "$a" != 000 ] && [ "$b" = 000 ] && [ "$c" = 000 ] && FORWARDER_OK=1
  log "forwarder $FWD self-test: up=$a hold=$b refuse=$c: $([ "$FORWARDER_OK" = 1 ] && echo works || echo BROKEN)"
}
link() { echo "$1" > "$FWD_CTL"; log "  link: $1"; }

restore_appearance() { [ -n "$ORIG_APPEARANCE" ] && osascript -e "tell application \"System Events\" to tell appearance preferences to set dark mode to $([ "$ORIG_APPEARANCE" = Dark ] && echo true || echo false)" >/dev/null 2>&1; ORIG_APPEARANCE=""; }
forget_selections() { local f="$DATA/capture.json"; [ -f "$f" ] && jq -e "[keys[] | select(startswith(\"m71-\"))] | length > 0" "$f" > /dev/null 2>&1 && jq "with_entries(select(.key | startswith(\"m71-\") | not))" "$f" > "$f.tmp" && mv "$f.tmp" "$f"; } # the harness's own window selections, never the person's
cleanup() {
  forget_selections
  restore_appearance
  link up 2>/dev/null
  local p; for p in "${CHILDREN[@]}"; do
    # Only what this run started: it must still be one of the harness's own programs.
    if ps -o command= -p "$p" 2>/dev/null | grep -qE "$ROOT/target/debug|LectureLive Canary.app/Contents/MacOS|virtual_input|swift|caffeinate|vite|npm"; then kill "$p" 2>/dev/null; fi
  done
}
