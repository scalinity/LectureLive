#!/bin/bash
# Class mode: the checks that need a real Zoom class, run beside the Python tool without touching it, your Zoom view or the audio.
#
#   tools/verify/class.sh arm                        waits for the meeting window and starts by itself (or: start [--minutes N], once it is open)
#   tools/verify/class.sh status
#   tools/verify/class.sh finish                     also done by itself when the meeting window has been gone for 3 minutes
#   tools/verify/class.sh v04                        optional, and it takes Zoom off your screen for 5 minutes (see below)
#   tools/verify/class.sh selftest                   this script's parsing and V04 logic on canned data; needs no Zoom, touches nothing
#
# One headless CLI lecture records BlackHole (the class's audio) in a scratch folder under its own course name, and watches the
# Zoom meeting window through a region (V02: the detector on a live meeting, the control bar and the camera left out). Nothing
# is spoken, nothing is chosen on screen, and it ends by quitting, so no snapshot is taken and no slide is sent anywhere.
#   V01  a real class: the first 30 minutes of the lecture's sound with no unexplained gap (audit)
#   V02  the slides it took, and when, with its recording of what the detector saw, for someone to read against the class
#   V04  Zoom in full screen with another app in front for 5 minutes: do whole slides still arrive from a window Zoom is not showing?
# It never speaks into BlackHole, signals only the CLI it started, and does not touch capture.json beyond one entry of its own.
[ "$1" = selftest ] && { OUT=$(mktemp -d "${TMPDIR:-/tmp}/class-selftest.XXXXXX"); trap 'rm -rf "$OUT"' EXIT; } # a scratch folder: the real run's is never touched
OUT="${OUT:-$HOME/Library/Application Support/LectureLive/verify/class-$(date +%Y%m%d)}"
export OUT
DIR="$(cd "$(dirname "$0")" && pwd)"
source "$DIR/lib.sh"
STATE="$OUT/class.state"
COURSE="m71-class"
SELECTIONS="$DATA/capture.json"
DEADLINE_EPOCH=$(( $(date +%s) + 4 * 3600 ))

list_windows() { if [ -n "$CANARY_WINDOWS_FIXTURE" ]; then cat "$CANARY_WINDOWS_FIXTURE"; else "$LL" canary windows 2>/dev/null; fi; } # the fixture is for selftest only
zoom_window() { [ -n "$ZOOM_WINDOW_OVERRIDE" ] && { echo "$ZOOM_WINDOW_OVERRIDE"; return; } # a rehearsal stands one in
  list_windows | awk '$2 == "Zoom" && $3 == "Zoom" && $4 == "Meeting" {print $1, $NF; exit}'; } # id WxH: columns are id, app, title words, size
saved() { [ -f "$STATE" ] && source "$STATE" && CLI_LOG="$OUT/class.cli.log"; }

selection_json() { # width height → the course's selection, from REGION=x,y,w,h or the person's own saved one at about this size
  local w=$1 h=$2 region
  if [ -n "$REGION" ]; then
    IFS=, read -r rx ry rw rh <<< "$REGION"; region=$(jq -nc --argjson x "$rx" --argjson y "$ry" --argjson w "$rw" --argjson h "$rh" '{x:$x,y:$y,w:$w,h:$h}')
  else
    region=$(jq -c --argjson w "$w" --argjson h "$h" '[to_entries[] | select(.key | startswith("m71-") | not) | select(.value.descriptor.bundle_id == "us.zoom.xos") | select(((.value.descriptor.width - $w) | fabs) <= 0.02 * $w and ((.value.descriptor.height - $h) | fabs) <= 0.02 * $h) | .value.region] | .[0] // empty' "$SELECTIONS" 2>/dev/null)
  fi
  [ -z "$region" ] && return 1
  # The bottom of the region, where Zoom's control bar fades in and out, is left out; so is nothing else unless asked.
  jq -nc --argjson w "$w" --argjson h "$h" --argjson r "$region" '{descriptor: {bundle_id: "us.zoom.xos", app: "Zoom", title: "Zoom Meeting", width: $w, height: $h}, region: $r, leave_out: [{x: 0, y: 0.88, w: 1, h: 0.12}]}'
}

cmd_start() {
  local minutes=130
  while [ $# -gt 0 ]; do case "$1" in --minutes) minutes=$2; shift 2 ;; *) shift ;; esac; done
  saved && cli_alive 2>/dev/null && { echo "class mode is already running (pid $CLI_PID); use status or finish"; return 1; }
  local win; win=$(zoom_window); [ -z "$win" ] && { echo "no Zoom meeting window yet: open the meeting, then run this again"; return 3; }
  local id size w h; read -r id size <<< "$win"; w=${size%x*}; h=${size#*x}
  "$LL" canary capture --id "$id" --out "$OUT/zoom-meeting.png" > /dev/null 2>&1
  local sel; sel=$(selection_json "$w" "$h") || { echo "the meeting window is ${w}x${h} and no saved region matches it: look at $OUT/zoom-meeting.png and run again with REGION=x,y,w,h (fractions of the window)"; return 4; }
  cp "$SELECTIONS" "$OUT/capture.json.before" 2>/dev/null || echo '{}' > "$OUT/capture.json.before"
  jq --arg c "$COURSE" --argjson s "$sel" '.[$c] = $s' "$OUT/capture.json.before" > "$OUT/capture.json.new" && mv "$OUT/capture.json.new" "$SELECTIONS" || { echo "could not add the class's region to $SELECTIONS (is it valid JSON?): not started"; return 5; }
  FOLDER="$OUT/$COURSE/Weeks/Week 01"; mkdir -p "$FOLDER"; STAGE=class; CLI_LOG="$OUT/class.cli.log"; : > "$CLI_LOG"
  CLI_ENV=("LECTURELIVE_RECORD=$OUT/detector"); CLI_SRC=(--device BlackHole)
  # No --secs: the CLI would end by a normal stop, and a normal stop takes a last snapshot that sends every slide it took. It ends by a quit.
  cli_start
  # Something must keep the CLI's input open after this command returns, or it reads end of input.
  nohup sleep 14400 > "$OUT/$STAGE.in" 2> /dev/null < /dev/null &
  HOLD_PID=$!
  START_EPOCH=$(date +%s)
  printf 'CLI_PID=%s\nHOLD_PID=%s\nFOLDER=%q\nSTART_EPOCH=%s\nMINUTES=%s\nZOOM_ID=%s\n' "$CLI_PID" "$HOLD_PID" "$FOLDER" "$START_EPOCH" "$minutes" "$id" > "$STATE"
  echo "class mode started at $(date +%H:%M): the meeting window $id (${w}x${h}), region $(echo "$sel" | jq -c .region), left out the bottom of it"
  echo "selection image: $OUT/zoom-meeting.png ; results will be in $OUT/class-report.md"
  ( nohup "$DIR/class.sh" _watch > "$OUT/class.watch.log" 2>&1 < /dev/null & )
}

cmd_status() {
  saved || { echo "class mode is not running"; return 1; }
  local alive=no; kill -0 "$CLI_PID" 2>/dev/null && alive=yes
  echo "running: $alive; since $(date -r "$START_EPOCH" +%H:%M) ($(( ($(date +%s) - START_EPOCH) / 60 )) min)"
  local bytes; bytes=$(stat -f %z "$FOLDER"/recordings/*.wav 2>/dev/null | paste -sd+ - | bc) # empty until the first recording exists
  echo "recorded: $(( ${bytes:-0} / 32000 )) s; lines transcribed: $(grep -c '^\[' "$FOLDER"/lecture_transcript_*.txt 2>/dev/null); slides taken: $(ls "$FOLDER/slides" 2>/dev/null | wc -l | tr -d ' ')"
  grep -iE "asking|watching|paused|found again|opened again|no window|capture" "$CLI_LOG" | tail -3 | cut -c1-160
}

# Ends itself when the meeting window has been gone for 3 minutes, or at the time limit.
cmd_watch() {
  saved || exit 1
  local gone=0
  while kill -0 "$CLI_PID" 2>/dev/null; do
    if [ -n "$(zoom_window)" ]; then gone=0; else gone=$(( gone + 1 )); fi
    [ "$gone" -ge 18 ] && break # 18 looks, ten seconds apart
    [ $(( $(date +%s) - START_EPOCH )) -ge $(( MINUTES * 60 )) ] && break # the time limit
    sleep 10
  done
  cmd_finish
}

cmd_finish() {
  saved || { echo "class mode is not running"; return 1; }
  CLI_PID=${CLI_PID}; STAGE=class; CLI_LOG="$OUT/class.cli.log"
  if kill -0 "$CLI_PID" 2>/dev/null; then cli_signal TERM; cli_wait_exit 20; fi # a quit: finalized, no snapshot
  ps -o command= -p "$HOLD_PID" 2>/dev/null | grep -q "sleep 14400" && kill "$HOLD_PID" # the input holder this mode started
  EVID=(); STAGE_FAIL=0; BLOCKED=0; PARTIAL=0
  local secs; secs=$(sidecar '[.recordings[].samples] | add // 0'); secs=$(( ${secs:-0} / 16000 ))
  audit
  local words; words=$(tx | grep -c '^\[')
  local took=$(( ($(date +%s) - START_EPOCH) / 60 ))
  check "the lecture's sound was recorded for ${secs} s over $(wavs) recording(s)" '[ "$secs" -ge 1800 ]'
  check "no unexplained or missing audio (audit exit 0)" '[ "$AUDIT_RC" = 0 ]'
  check "the class's words reached the transcript ($words lines): the audio was real" '[ "$words" -ge 5 ]'
  note "audit: $(echo "$AUDIT_OUT" | tail -2 | tr '\n' ' ')"
  note "gaps: $(sidecar '[.gaps[] | .kind] | group_by(.) | map({(.[0]): length}) | add')"
  finish_stage V01-class "a real Zoom class, $took minutes beside the Python tool"
  # V02: what the detector took from the live meeting, for someone to read against what the lecturer showed.
  local auto uncertain; auto=$(sidecar '[.slides[] | select(.auto)] | length'); uncertain=$(sidecar '[.slides[] | select(.uncertain)] | length')
  {
    echo "# Class report $(date +%Y-%m-%d)"; echo
    echo "- Recorded ${secs} s; audit exit ${AUDIT_RC} ($(echo "$AUDIT_OUT" | tail -1)); $words transcript lines."
    echo "- V02: the detector took ${auto:-0} slides automatically in $took minutes (${uncertain:-0} flagged uncertain): about $(awk -v n="${auto:-0}" -v m="$took" 'BEGIN{printf "%.1f", (m>0 ? n/m*10 : 0)}') per 10 minutes."
    echo "- The slides, in order, with when each was first on screen: \`$FOLDER/slides/\`; the detector's own recording (every distinct frame it saw, one line per second): \`$OUT/detector/\`."
    echo "- Capture states during the class:"; grep -iE "asking|watching|paused|found again|opened again|no window|capture" "$CLI_LOG" | sed "s/^/  /" | head -12
    echo; echo "To judge V02 by eye: open the slides folder next to the lecturer's deck. Each slide or build should appear once; a slide that is only the speaker's camera or the control bar is a false capture."
  } > "$OUT/class-report.md"
  # Put the person's own selections back as they were (this mode added one entry of its own).
  if [ -f "$OUT/capture.json.before" ]; then
    jq --arg c "$COURSE" 'del(.[$c])' "$SELECTIONS" > "$OUT/capture.json.new" && mv "$OUT/capture.json.new" "$SELECTIONS" || echo "could not take the class's region ($COURSE) out of $SELECTIONS: remove that entry by hand (the original is $OUT/capture.json.before)"
  fi
  rm -f "$STATE"
  echo "class mode finished: $OUT/class-report.md"; cat "$OUT/class-report.md"
  osascript -e 'display notification "Class checks finished: see class-report.md" with title "LectureLive harness"' > /dev/null 2>&1
}

# V04: Zoom in full screen on its own desktop, another app in front, for five minutes. The class goes on in Zoom's own space;
# this only moves what you see. Whole slides that arrive while it lasts show that Zoom keeps drawing a window it is not showing
# and that the detector still reads all of it; cropped ones show that it does not (full screen changes the window's size).
V04_SECS="${V04_SECS:-300}"; V04_POLL="${V04_POLL:-1}"; V04_SETTLE="${V04_SETTLE:-2}" # seconds: Zoom stays full screen; between looks at its state; Zoom's own animation

# How many zoom.us windows report AXFullScreen true. Every window is read, none is found by name: a full screen window is no
# longer named "Zoom Meeting". Prints nothing and returns 1 when System Events cannot be asked (why: $OUT/v04.ax.err).
zoom_fs_count() {
  local n
  n=$(osascript -e 'tell application "System Events" to tell process "zoom.us"' -e 'set n to 0' -e 'repeat with w in windows' -e 'try' -e 'if (value of attribute "AXFullScreen" of w) is true then set n to n + 1' -e 'end try' -e 'end repeat' -e 'return n' -e 'end tell' 2> "$OUT/v04.ax.err") || return 1
  echo "$n"
}
# fs_wait yes|no POLLS: looks at Zoom's state every V04_POLL seconds until a window is full screen (yes) or none is (no).
fs_wait() {
  local i n
  for ((i = 0; i < $2; i++)); do
    n=$(zoom_fs_count)
    if [ "$1" = yes ] && [ "${n:-0}" -ge 1 ]; then return 0; fi
    if [ "$1" = no ] && [ "$n" = 0 ]; then return 0; fi
    sleep "$V04_POLL"
  done
  return 1
}
# The meeting window into full screen (found by its name, which it still has); confirmed by reading it back, with one retry.
v04_enter() {
  local try; V04_SET_OK=0
  for try in 1 2; do
    osascript -e 'tell application "System Events" to tell process "zoom.us" to set value of attribute "AXFullScreen" of (first window whose name contains "Zoom Meeting") to true' > /dev/null 2> "$OUT/v04.set.err" && V04_SET_OK=1
    fs_wait yes 4 && return 0
  done
  return 1
}
# Every zoom.us window that is full screen out of it, whatever it is named (the read-back below says whether it worked).
v04_unfull() { osascript -e 'tell application "System Events" to tell process "zoom.us"' -e 'repeat with w in windows' -e 'try' -e 'if (value of attribute "AXFullScreen" of w) is true then set value of attribute "AXFullScreen" of w to false' -e 'end try' -e 'end repeat' -e 'end tell' > /dev/null 2>&1; }
# Zoom's desktop shown first, then out of full screen, confirmed by reading it back (up to ~10 s).
v04_leave() {
  osascript -e 'tell application "zoom.us" to activate' > /dev/null 2>&1; sleep "$V04_POLL"
  v04_unfull; fs_wait no 5 && return 0
  v04_unfull; fs_wait no 5
}
v04_leave_report() { # 0 when Zoom is confirmed out of full screen
  if v04_leave; then echo "V04: Zoom is out of full screen again (confirmed by reading it back)"; return 0; fi
  echo "V04: !!! ZOOM COULD NOT BE TAKEN OUT OF FULL SCREEN !!!"
  echo "V04: do this now: click in Zoom and press Ctrl-Cmd-F (or move the pointer to the top of the screen and click the green button)"
  osascript -e 'display notification "Zoom is still full screen: press Ctrl-Cmd-F in Zoom" with title "LectureLive harness"' > /dev/null 2>&1
  return 1
}
v04_interrupted() { # Ctrl-C, closing the Terminal or a kill: leave full screen first, or the person is stuck
  trap '' INT TERM HUP
  [ -n "$V04_SLEEP" ] && kill "$V04_SLEEP" 2>/dev/null # the sleep this run started
  echo; echo "V04: interrupted; taking Zoom out of full screen"
  v04_leave_report; echo "V04: no result written (run v04 again for one)"; exit 130
}

png_size() { sips -g pixelWidth -g pixelHeight "$1" 2>/dev/null | awk '/pixelWidth/ {w = $2} /pixelHeight/ {h = $2} END {if (w && h) print w, h}'; } # W H
# The slides whose shown_at falls between two local wall-clock strings (YYYY-MM-DDTHH:MM:SS, both included), as "HH:MM:SS<TAB>file".
# shown_at carries an offset ("-04:00") that jq's strptime rejects, so its first 19 characters are compared as text.
v04_slides() { jq -r --arg a "$1" --arg b "$2" '.slides[] | select(.shown_at[0:19] >= $a and .shown_at[0:19] <= $b) | [.shown_at[11:19], .file] | @tsv' "$FOLDER"/.live_notes/*.v2.json; }

# The verdict, from the files alone: the slides that arrived between two epochs, each saved whole or cropped. A slide is cropped when
# it is under 80% of the widest slide saved earlier in the folder (whole ones are saved at up to 1600 px). Prints the table; sets
# V04_STATUS and V04_NOTE. Slides without a count (jq or sips failing) are never read as "0 arrived".
v04_judge() { # from_epoch to_epoch
  local a b err="$OUT/v04.jq.err" samples="$OUT/detector/samples.jsonl" rows early t f w h ref=0 n=0 cropped=0 unreadable=0 csz="" frames ft
  a=$(date -r "$1" +%Y-%m-%dT%H:%M:%S); b=$(date -r "$2" +%Y-%m-%dT%H:%M:%S)
  V04_STATUS=INCONCLUSIVE
  rows=$(v04_slides "$a" "$b" 2> "$err") && early=$(v04_slides 0 "$(date -r $(( $1 - 1 )) +%Y-%m-%dT%H:%M:%S)" 2>> "$err") || {
    V04_NOTE="the slide list could not be read, so nothing was counted ($(head -c 200 "$err" | tr '\n' ' '))"; return; }
  if [ ! -f "$samples" ]; then ft="no detector recording to count window frames in"
  elif frames=$(jq -s --arg a "$a" --arg b "$b" '[.[] | select(.at[0:19] >= $a and .at[0:19] <= $b) | .window] | unique | length' "$samples" 2>> "$err"); then ft="the detector saw $frames distinct window frames"
  else frames=""; ft="the detector recording could not be read"; fi
  while IFS=$'\t' read -r t f; do
    [ -n "$f" ] || continue
    read -r w h <<< "$(png_size "$FOLDER/$f")"; [ "${w:-0}" -gt "$ref" ] && ref=$w
  done <<< "$early"
  echo "V04: slides that arrived from $a to $b (cropped = under 80% of $ref px, the widest earlier slide):"
  while IFS=$'\t' read -r t f; do
    [ -n "$f" ] || continue
    n=$(( n + 1 )); read -r w h <<< "$(png_size "$FOLDER/$f")"
    if [ -z "$w" ]; then unreadable=$(( unreadable + 1 )); printf '  %s  %-11s  %s\n' "$t" "unreadable" "$f"
    elif [ $(( w * 100 )) -lt $(( ref * 80 )) ]; then cropped=$(( cropped + 1 )); csz="$csz ${w}x${h}"; printf '  %s  %5s x %-4s  CROPPED  %s\n' "$t" "$w" "$h" "$f"
    else printf '  %s  %5s x %-4s  whole    %s\n' "$t" "$w" "$h" "$f"; fi
  done <<< "$rows"
  [ "$n" = 0 ] && echo "  (none)"
  local pl=s; [ "$n" = 1 ] && pl=""
  if [ "$cropped" -gt 0 ]; then V04_STATUS=FAIL; V04_NOTE="$cropped of $n slide$pl arrived cropped (${csz# }) while Zoom was full screen out of sight; the widest earlier slide is $ref px wide; $ft"
  elif [ "$n" = 0 ]; then
    if [ "${frames:-0}" -gt 1 ]; then V04_NOTE="Zoom drew $frames distinct window frames but no slide arrived: no slide changed, or the detector did not notice (mark Needs follow-up)"
    else V04_NOTE="no slide arrived and $ft: nothing shows whether Zoom kept drawing (mark Needs follow-up)"; fi
  elif [ "$unreadable" -gt 0 ]; then V04_NOTE="$unreadable of $n slide$pl could not be measured, so none could be called whole or cropped"
  elif [ "$ref" = 0 ]; then V04_NOTE="$n slide$pl arrived but no earlier slide exists to compare the size with"
  else V04_STATUS=PASS; V04_NOTE="$n slide$pl arrived whole (the widest earlier slide is $ref px wide) while Zoom was full screen out of sight; $ft"; fi
}

cmd_v04() {
  saved || { echo "start class mode first"; return 1; }
  kill -0 "$CLI_PID" 2>/dev/null || { echo "class mode's CLI is not running, so no slide could arrive: nothing was changed (see status)"; return 1; }
  local from why; from=$(date +%s)
  if ! v04_enter; then
    why=$(cat "$OUT/v04.set.err" "$OUT/v04.ax.err" 2>/dev/null | head -c 200 | tr '\n' ' ')
    echo "V04 not started: Zoom's meeting window could not be put in full screen (${why:-Zoom never reported a full screen window}). No result was written."
    [ "$V04_SET_OK" = 1 ] && v04_leave_report # it may have gone full screen without being able to say so
    return 1
  fi
  trap v04_interrupted INT TERM HUP
  sleep "$V04_SETTLE"; osascript -e 'tell application "Terminal" to activate' > /dev/null 2>&1 # after Zoom's own animation
  echo "V04: Zoom is full screen (confirmed) on its own desktop, Terminal in front, from $(date +%H:%M:%S) for $V04_SECS s; Ctrl-C leaves full screen again"
  sleep "$V04_SECS" & V04_SLEEP=$!; wait "$V04_SLEEP" # a wait, so that a signal is not held back until the sleep ends
  local left=0; v04_leave_report || left=1
  trap - INT TERM HUP
  v04_judge "$from" "$(date +%s)"
  kill -0 "$CLI_PID" 2>/dev/null || V04_NOTE="$V04_NOTE; the class's CLI had exited by the end"
  [ "$left" = 1 ] && V04_NOTE="$V04_NOTE; ZOOM WAS LEFT IN FULL SCREEN: press Ctrl-Cmd-F in Zoom"
  [ -f "$RESULTS" ] && { grep -v $'^V04\t' "$RESULTS" > "$RESULTS.tmp"; mv "$RESULTS.tmp" "$RESULTS"; } # a re-run replaces the earlier V04 line
  result V04 "$V04_STATUS" "$V04_NOTE"
  return "$left"
}

# Waits for the meeting window to open, lets it settle, and starts class mode by itself; says so if it needs a region.
# Everything it does goes to the arm log with the time, so that a wait that never ends can be told from one that has not begun.
alog() { printf '%s  %s\n' "$(date +%H:%M:%S)" "$*"; }
cmd_arm_loop() {
  local end=$(( $(date +%s) + 4 * 3600 )) win rc last; last=$(date +%s)
  alog "armed: waiting for a Zoom meeting window (gives up at $(date -r "$end" +%H:%M))"
  while :; do
    until win=$(zoom_window); [ -n "$win" ]; do
      [ "$(date +%s)" -ge "$end" ] && { alog "gave up: no Zoom meeting window in 4 hours"; exit 0; }
      [ $(( $(date +%s) - last )) -ge 300 ] && { alog "still waiting for a Zoom meeting window ($(list_windows | wc -l | tr -d ' ') windows listed)"; last=$(date +%s); }
      sleep 10
    done
    alog "Zoom meeting window seen: id ${win% *}, size ${win#* }"
    alog "waiting 30 s for it to settle"; sleep 30 # a meeting window that has only just opened is not its final size yet
    alog "starting class mode"
    cmd_start; rc=$?
    alog "cmd_start returned $rc"
    [ "$rc" = 3 ] && { alog "the meeting window is gone again: waiting for it"; continue; } # it closed during the settle wait
    [ "$rc" != 0 ] && osascript -e "display notification \"Class mode did not start (code $rc): see $OUT/class.arm.log\" with title \"LectureLive harness\"" > /dev/null 2>&1
    exit 0
  done
}
cmd_arm() { ( nohup "$DIR/class.sh" _arm > "$OUT/class.arm.log" 2>&1 < /dev/null & ); echo "armed: class mode starts by itself when a Zoom meeting window opens (log: $OUT/class.arm.log)"; }

case "$1" in
  arm) cmd_arm ;;
  _arm) cmd_arm_loop ;;
  start) shift; cmd_start "$@" ;;
  status) cmd_status ;;
  finish) cmd_finish ;;
  v04) cmd_v04 ;;
  _watch) cmd_watch ;;
  selftest) source "$DIR/class_selftest.sh"; cmd_selftest; exit $? ;;
  *) sed -n '2,16p' "$0"; exit 2 ;;
esac
