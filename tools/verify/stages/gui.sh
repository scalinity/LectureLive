#!/bin/bash
# The app, driven by its own in-app check runner (LECTURELIVE_CHECK) and from outside by System Events and cliclick:
#   gui_capture  V19 V20 V17(app) and the M5 capture gate again: pause, a restarted window, a control bar, ending in a quit
#   gui_quit     V18 (the app half): an Apple-Event quit, Cmd-Q, closing the window, SIGTERM, SIGHUP, and a quit while paused
#   gui_looks    V06 and V17 (look): the app running and paused, in light and dark, in normal and large type (photographed)
#   gui_drag     V05: the window is moved by its top strip and zoomed by a double click
# The app is the harness's own child, run from the dev build on a synthetic folder; it records BlackHole silently and never
# sends a slide anywhere: every stage ends it by quitting, which takes no last snapshot.

gui_prep() {
  if ! curl -s -o /dev/null -m 3 http://localhost:1420; then
    ( cd "$ROOT/apps/desktop" && npm run dev > "$OUT/vite.log" 2>&1 & echo $! > "$OUT/vite.pid" )
    CHILDREN+=("$(cat "$OUT/vite.pid")")
    wait_until 40 "the page server" 'curl -s -o /dev/null -m 3 http://localhost:1420' || return 1
  fi
  if [ -z "$AWAKE_PID" ]; then
    caffeinate -d -i -t $(( $(time_left) + 60 )) > /dev/null 2>&1 < /dev/null & AWAKE_PID=$!; CHILDREN+=("$AWAKE_PID") # the display stays awake
  fi
  return 0
}

# app_launch MODE FOLDER [VAR=value ...]: the dev app in an in-app check, as the harness's own child.
app_launch() {
  local mode=$1 dir=$2; shift 2
  APP_T0=$(date +%s)
  ( cd "$ROOT" && exec env LECTURELIVE_CHECK="$mode" LECTURELIVE_CHECK_DIR="$dir" "$@" "$APPBIN" ) >> "$OUT/app.$STAGE.log" 2>&1 < /dev/null &
  APP_PID=$!; CHILDREN+=("$APP_PID")
}
app_alive() { kill -0 "$APP_PID" 2>/dev/null; }
app_signal() { ps -o command= -p "$APP_PID" 2>/dev/null | grep -qE "$APPBIN|LectureLive Canary.app/Contents/MacOS/desktop" || return 1; kill "-$1" "$APP_PID"; }
app_wait_exit() { wait_until "$1" "the app to exit" '! app_alive'; }
app_report() { local f="$DATA/m6-checks/$1.json"; [ -f "$f" ] && [ "$(stat -f %m "$f")" -ge "$APP_T0" ]; }
app_report_file() { echo "$DATA/m6-checks/$1.json"; }
# The app's main window (not the deck): the largest window of the app named desktop.
app_window() { "$LL" canary windows 2>/dev/null | awk '$2 == "desktop" && $0 !~ /deck/ {split($NF, d, "x"); print d[1]*d[2], $1}' | sort -rn | head -1 | awk '{print $2}'; }
sysevents() { osascript -e "tell application \"System Events\" to tell (first process whose unix id is $APP_PID) to $1" 2>&1; }

gui_synthetic() { # name → a fresh synthetic folder in FOLDER, with an image to drop
  new_lecture "$1"; cp "$ROOT/apps/desktop/static/favicon.png" "$FOLDER/drop-me.png"
}

stage_gui_capture() {
  gui_prep || { STAGE=gui-capture; EVID=(); STAGE_FAIL=1; BLOCKED=1; check "the page server is up" false; finish_stage "V19 V20" "no page server"; return; }
  gui_synthetic gui-capture
  app_launch capture "$FOLDER" LECTURELIVE_CHECK_QUIT=1
  app_wait_exit 480
  check "the app ended by itself (the check quits it with the lecture running)" '! app_alive'
  local rep; rep=$(app_report_file capture)
  check "the check wrote its report" 'app_report capture'
  if app_report capture; then
    local failed; failed=$(jq -r '[.steps[] | select(.ok | not) | .step] | join("; ")' "$rep")
    note "$(jq '.steps | length' "$rep") steps, failed: ${failed:-none}"
    check "every step of the in-app capture check passed" '[ -z "$failed" ]'
    for s in "paused: the strip says paused and capture waits" "nothing is sampled while the lecture is paused" "after the resume, the change made meanwhile is taken once" "a restarted window is followed with no click, and said" "a control bar coming and going twelve times registers at most two more slides"; do
      check "step: $s" 'jq -e --arg s "$s" "[.steps[] | select(.step == \$s and .ok)] | length == 1" "$rep" >/dev/null'
    done
    note "control bar: $(jq -c '.steps[] | select(.step | startswith("a control bar")) | .detail' "$rep")"
  fi
  check "the recording was finalized by the quit (nothing to repair)" '[ "$(sidecar "[.recordings[] | select(.state != \"finalized\")] | length")" = 0 ]'
  check "no last snapshot was spent" '[ "$(cost_of notes)" = 0 ]'
  finish_stage "V19 V20" "the in-app capture check: pause, a restarted window followed, a control bar, a quit with the lecture running"
}

# One quit of an `idle` app by one route.
gui_quit_once() { # name, mode, how
  local name=$1 mode=$2 how=$3
  gui_synthetic "$name"
  app_launch "$mode" "$FOLDER"
  wait_until 60 "the app to start its lecture" 'app_report "$mode"' || { check "the app started a lecture" false; return; }
  sleep 3
  local t0; t0=$(date +%s)
  case "$how" in
    apple)  sysevents "quit" > "$OUT/$name.quit.out" ;;
    cmdq)   osascript -e "tell application \"System Events\" to set frontmost of (first process whose unix id is $APP_PID) to true" -e 'delay 1' -e 'tell application "System Events" to keystroke "q" using command down' > "$OUT/$name.quit.out" 2>&1 ;;
    close)  sysevents 'click (first button of window 1 whose subrole is "AXCloseButton")' > "$OUT/$name.quit.out" ;;
    term)   app_signal TERM ;;
    hup)    app_signal HUP ;;
  esac
  app_wait_exit 20
  local took=$(( $(date +%s) - t0 ))
  check "$how: the app ended, in ${took} s (under 12)" '! app_alive && [ "$took" -lt 12 ]'
  check_quit_state "$how" "$mode"
  check "$how: no last snapshot was spent" '[ "$(cost_of notes)" = 0 ]'
  [ -s "$OUT/$name.quit.out" ] && note "$how: osascript said: $(head -c 160 "$OUT/$name.quit.out")"
  if app_alive; then app_signal TERM; sleep 2; app_alive && app_signal KILL; fi # never leave one behind
  finish_stage "" "$how"
}
stage_gui_quit() {
  gui_prep || { STAGE=gui-quit; EVID=(); BLOCKED=1; result V18-app BLOCKED "no page server"; return; }
  local ok=PASS; : > "$OUT/gui-quit.evidence.txt"
  local spec
  for spec in "gq-cmdq idle cmdq" "gq-close idle close" "gq-term idle term" "gq-hup idle hup" "gq-paused idle-paused term"; do
    gui_quit_once $spec
    sed "s/^/[$STAGE] /" "$OUT/$STAGE.evidence.txt" >> "$OUT/gui-quit.evidence.txt"
    [ "$STAGE_FAIL" = 1 ] && ok=FAIL
  done
  result V18-app "$ok" "the dev app quit by Cmd-Q, window close, SIGTERM, SIGHUP, and SIGTERM while paused; the bare dev binary has no app name for an Apple Event (see gui-quit.evidence.txt)"
}

stage_gui_looks() {
  gui_prep || { STAGE=gui-looks; EVID=(); BLOCKED=1; result V06 BLOCKED "no page server"; return; }
  local original; original=$(defaults read -g AppleInterfaceStyle 2>/dev/null || echo Light); ORIG_APPEARANCE=$original
  mkdir -p "$OUT/looks"
  local appearance state
  for appearance in Light Dark; do
    osascript -e "tell application \"System Events\" to tell appearance preferences to set dark mode to $([ $appearance = Dark ] && echo true || echo false)"
    sleep 2
    gui_synthetic "gl-$(echo $appearance | tr A-Z a-z)"
    app_launch looks "$FOLDER"
    for state in running paused paused-large running-large; do
      if wait_until 90 "the $state look" 'app_report "looks-$state"'; then
        sleep 4
        local id; id=$(app_window)
        [ -n "$id" ] && "$LL" canary capture --id "$id" --out "$OUT/looks/$appearance-$state.png" >/dev/null 2>&1
        check "$appearance, $state: photographed" '[ -s "$OUT/looks/$appearance-$state.png" ]'
      else
        check "$appearance, $state: reached" false
      fi
    done
    app_wait_exit 60
    app_alive && app_signal TERM
    finish_stage "" "$appearance looks"
  done
  restore_appearance
  result V06 PARTIAL "photographed in light and dark, normal and large type, running and paused: $OUT/looks (a person or this harness's reviewer must read them)"
}

stage_gui_drag() {
  gui_prep || { STAGE=gui-drag; EVID=(); BLOCKED=1; result V05 BLOCKED "no page server"; return; }
  gui_synthetic gui-drag
  app_launch idle "$FOLDER"
  wait_until 60 "the app to start" 'app_report idle' || { check "the app started" false; finish_stage V05 "did not start"; app_signal TERM; return; }
  sleep 3
  # Raised above every other window first: a click lands on whatever is on top at that point.
  sysevents 'set frontmost to true' > /dev/null; sysevents 'perform action "AXRaise" of window 1' > /dev/null; sleep 1
  local geom x y w h; geom=$(sysevents 'get {position, size} of window 1' | tr -d ' '); IFS=, read -r x y w h <<< "$geom"
  note "window at $x,$y size ${w}x${h}"
  local moved=no cx x2 y2 g2
  for frac in 40 25 55; do # points along the strip: its buttons sit at its edges
    cx=$(( x + w * frac / 100 ))
    cliclick -w 100 "dd:$cx,$(( y + 15 ))" "dm:$(( cx + 120 )),$(( y + 75 ))" "du:$(( cx + 120 )),$(( y + 75 ))"; sleep 1
    g2=$(sysevents 'get position of window 1' | tr -d ' '); IFS=, read -r x2 y2 <<< "$g2"
    if [ "$(( x2 > x ? x2 - x : x - x2 ))" -ge 80 ] || [ "$(( y2 > y ? y2 - y : y - y2 ))" -ge 40 ]; then moved=yes; note "dragged from $frac% of the strip: moved by $((x2 - x)),$((y2 - y))"; break; fi
  done
  check "the window moves when dragged by its top strip" '[ "$moved" = yes ]'
  local before after restored
  before=$(sysevents 'get {position, size} of window 1' | tr -d ' '); IFS=, read -r x y w h <<< "$before"
  cliclick -w 150 "dc:$(( x + w * 40 / 100 )),$(( y + 15 ))"; sleep 2
  after=$(sysevents 'get {position, size} of window 1' | tr -d ' ')
  note "double click: $before -> $after"
  check "a double click on the strip zooms the window (its position or size changes)" '[ "$before" != "$after" ]'
  IFS=, read -r x y w h <<< "$after"
  cliclick -w 150 "dc:$(( x + w * 40 / 100 )),$(( y + 15 ))"; sleep 2
  restored=$(sysevents 'get {position, size} of window 1' | tr -d ' ')
  if [ "$restored" = "$before" ]; then check "another double click restores it" true; else note "a second double click left it at $restored, not back at $before: the window is already at its standard size, so there is nothing to restore (not asserted)"; PARTIAL=1; fi
  app_signal TERM; app_wait_exit 15
  finish_stage V05 "dragged and double-clicked with cliclick, the window raised first, read back through System Events"
}

# V10 and V14 in the app: a lecture in the running app has its link held then refused, is killed with SIGKILL, and is started
# again in the same folder, while synthetic speech goes into BlackHole. The in-app `faults` check records what a person would see.
at_s() { local target=$(( APP_T0 + $1 )); while [ "$(date +%s)" -lt "$target" ]; do sleep 1; done; }
stage_gui_faults() {
  gui_prep || { STAGE=gui-faults; EVID=(); BLOCKED=1; result V10-app BLOCKED "no page server"; return; }
  new_lecture gui-faults
  [ "$FORWARDER_OK" = 1 ] || { check "the test forwarder works (self-test)" false; BLOCKED=1; finish_stage "V10 V14" "no working forwarder"; return; }
  link up
  app_launch faults "$FOLDER" LECTURELIVE_CHECK_MINUTES=7 LECTURELIVE_API_ADDR="$FWD"
  at_s 25;  say_bh "The first marker is a telescope." || { finish_stage "V10 V14" "blocked"; app_signal TERM; return; }
  at_s 40;  link hold; say_bh "The second marker is a glacier."; say_bh "The third marker is a compass."
  at_s 105; link up
  at_s 140; say_bh "The fourth marker is a violin."
  at_s 165; link refuse; say_bh "The fifth marker is a meadow."
  at_s 200; link up
  at_s 235; say_bh "The sixth marker is a lantern."
  sleep 5
  local first_pid=$APP_PID
  app_signal KILL # the harness's own app, killed without warning mid-lecture
  app_wait_exit 10
  check "the kill ended the app" '! app_alive'
  check "the recording was left open, as a crash leaves it" '[ "$(sidecar "[.recordings[] | select(.state == \"open\")] | length")" -ge 1 ]'
  sleep 3
  app_launch faults "$FOLDER" LECTURELIVE_CHECK_MINUTES=2 LECTURELIVE_API_ADDR="$FWD"
  at_s 25;  say_bh "The seventh marker is a harpoon."
  app_wait_exit 300
  check "the second run ended by itself" '! app_alive'
  local w missing=""
  for w in telescope glacier compass violin meadow lantern harpoon; do tx_has "$w" || missing="$missing $w"; done
  check "every marker is in the transcript, the ones said while the link was down and the one cut off by the kill" '[ -z "$missing" ]'
  [ -n "$missing" ] && note "missing:$missing"
  local f; f=$(ls -t "$DATA"/m6-checks/faults-*.json 2>/dev/null | head -1)
  if [ -n "$f" ]; then
    check "the second run says it repaired the recording" 'jq -e "[.notices[] | select(test(\"Repaired\"))] | length >= 1" "$f" >/dev/null'
    note "second run notices: $(jq -r '[.notices[]] | map(select(test("Repaired|Recovered|Resumed|reconnect|Saved"))) | .[0:4] | join(" | ")' "$f" | cut -c1-260)"
  fi
  check "nothing is left open" '[ "$(sidecar "[.recordings[] | select(.state == \"open\")] | length")" = 0 ]'
  check "the link cuts really happened (stt_offline gaps were recorded, not only the kill's)" '[ "$(sidecar "[.gaps[] | select(.kind == \"stt_offline\")] | length")" -ge 1 ]'
  audit
  check "the audit is whole (0 unexplained, 0 waiting)" '[ "$AUDIT_RC" = 0 ]'
  finish_stage "V10 V14" "in the app: link held and refused through the harness forwarder, SIGKILL mid-lecture, a second run in the same folder"
}

# The state a quit leaves, for a running or a paused lecture: nothing open, and a pause closed rather than left running.
# A lecture paused before its first audio has no recording at all, which is right: nothing was recorded.
check_quit_state() { # how, mode
  if [ "$2" = idle-paused ]; then
    check "$1: nothing is left open, and the pause is closed" '[ -n "$(sidecar ".version")" ] && [ "$(sidecar "[.recordings[] | select(.state != \"finalized\")] | length")" = 0 ] && [ "$(sidecar "(.pauses | length) >= 1 and (.pauses | all(.to != null))")" = true ]'
  else
    check "$1: every recording is finalized, nothing to repair" '[ "$(sidecar "[.recordings | length] | .[0] >= 1")" = true ] && [ "$(sidecar "[.recordings[] | select(.state != \"finalized\")] | length")" = 0 ]'
  fi
}
