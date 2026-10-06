#!/bin/bash
# V18 for the packaged app. The dev binary is a bare executable; a proctoring browser quits registered apps by Apple Event, so
# the faithful target is the packaged app, built here from the tree as it is, and quit the ways a person or a tool would.
# It is run from the shell, not through LaunchServices, so it needs no new microphone or screen permission of its own.
BUNDLE="$ROOT/target/release/bundle/macos/LectureLive Canary.app"

gui_bundle_once() { # name, how, mode (idle or idle-paused)
  local name=$1 how=$2 mode=${3:-idle}
  gui_synthetic "$name"
  APP_T0=$(date +%s)
  ( cd "$ROOT" && exec env LECTURELIVE_CHECK="$mode" LECTURELIVE_CHECK_DIR="$FOLDER" "$BUNDLE/Contents/MacOS/desktop" ) >> "$OUT/app.$STAGE.log" 2>&1 < /dev/null &
  APP_PID=$!; CHILDREN+=("$APP_PID")
  wait_until 60 "the packaged app to start its lecture" 'app_report "$mode"' || { check "the packaged app started a lecture" false; app_signal TERM; return; }
  sleep 3
  local t0; t0=$(date +%s)
  case "$how" in
    by-name)    osascript -e 'tell application "LectureLive Canary" to quit' > "$OUT/$name.quit.out" 2>&1 ;;
    cmdq)       osascript -e "tell application \"System Events\" to set frontmost of (first process whose unix id is $APP_PID) to true" -e 'delay 1' -e 'tell application "System Events" to keystroke "q" using command down' > "$OUT/$name.quit.out" 2>&1 ;;
    close)      sysevents 'click (first button of window 1 whose subrole is "AXCloseButton")' > "$OUT/$name.quit.out" ;;
    term)       app_signal TERM ;;
  esac
  app_wait_exit 20
  local took=$(( $(date +%s) - t0 ))
  check "$how: the packaged app ended, in ${took} s (under 12)" '! app_alive && [ "$took" -lt 12 ]'
  check_quit_state "$how" "$mode"
  check "$how: no last snapshot was spent" '[ "$(cost_of notes)" = 0 ]'
  [ -s "$OUT/$name.quit.out" ] && note "$how: osascript said: $(head -c 160 "$OUT/$name.quit.out")"
  if app_alive; then app_signal TERM; sleep 2; app_alive && app_signal KILL; fi
  finish_stage "" "packaged app, $how"
}

stage_gui_bundle() {
  STAGE=gui-bundle; EVID=(); STAGE_FAIL=0; BLOCKED=0; PARTIAL=0
  gui_prep || { BLOCKED=1; result V18-bundle BLOCKED "no page server"; return; }
  local before; before=$(date +%s)
  ( cd "$ROOT/apps/desktop" && npm run tauri build -- --bundles app ) > "$OUT/bundle-build.log" 2>&1
  check "the packaged app builds from the tree as it is" '[ -x "$BUNDLE/Contents/MacOS/desktop" ] && [ "$(stat -f %m "$BUNDLE/Contents/MacOS/desktop")" -ge $(( before - 7200 )) ]'
  if [ "$STAGE_FAIL" = 1 ]; then finish_stage V18-bundle "the packaged app did not build (see bundle-build.log)"; return; fi
  local ok=PASS spec
  : > "$OUT/gui-bundle.evidence.txt"
  for spec in "gb-byname by-name idle" "gb-cmdq cmdq idle" "gb-close close idle" "gb-term term idle" "gb-paused by-name idle-paused"; do
    gui_bundle_once $spec
    sed "s/^/[$STAGE] /" "$OUT/$STAGE.evidence.txt" >> "$OUT/gui-bundle.evidence.txt"
    [ "$STAGE_FAIL" = 1 ] && ok=FAIL
  done
  result V18-bundle "$ok" "the packaged app quit by Apple Event addressed by app name (as a proctoring tool does), Cmd-Q, window close, SIGTERM, and by Apple Event while paused (see gui-bundle.evidence.txt)"
}
