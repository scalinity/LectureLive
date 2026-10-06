#!/bin/bash
# V16 and V12 on the packaged app's own permission identity (com.lecturelive.canary), not Terminal's: turning the microphone or
# Screen Recording off for Terminal would also take it from the Python tool, and a change needs Terminal reopened. The packaged app is
# launched through LaunchServices (`open --env`) so macOS treats it as itself, its permission is reset with `tccutil`, and the
# system's own prompt is answered through System Events. The microphone is granted again at the end; Screen Recording, which only
# System Settings can grant, is left reset (nothing but this packaged app's own checks uses it).
PERMS_ID=com.lecturelive.canary
PERMS_APP="$ROOT/target/release/bundle/macos/LectureLive Canary.app"

# Every button of every permission dialog on screen, one line per window, for the log and for choosing what to press.
dialog_buttons() {
  osascript -e 'tell application "System Events"' -e 'set out to ""' -e 'repeat with p in (processes whose name is "UserNotificationCenter")' -e 'try' -e 'repeat with w in windows of p' -e 'set out to out & (name of every button of w as string) & " | "' -e 'end repeat' -e 'end try' -e 'end repeat' -e 'return out' -e 'end tell' 2> /dev/null
}
click_dialog() { # the button's label: pressed with a real mouse click, which macOS records (an accessibility press is not)
  local g; g=$(osascript -e "tell application \"System Events\" to tell process \"UserNotificationCenter\" to get {position, size} of (first button of window 1 whose name is \"$1\")" 2> /dev/null | tr -d ' ')
  [ -z "$g" ] && { echo "no such button"; return; }
  local x y w h; IFS=, read -r x y w h <<< "$g"
  cliclick -w 200 "m:$(( x + w / 2 )),$(( y + h / 2 ))" "c:$(( x + w / 2 )),$(( y + h / 2 ))" && echo clicked
}
# The packaged app, launched as itself. Its pid is found by its own path, and only a process whose command line is exactly it is ever signalled.
perms_launch() { # mode, folder
  APP_T0=$(date +%s)
  open -n -a "$PERMS_APP" --env "LECTURELIVE_CHECK=$1" --env "LECTURELIVE_CHECK_DIR=$2"
  local i; for i in $(seq 1 20); do APP_PID=$(pgrep -f "LectureLive Canary.app/Contents/MacOS/desktop" | head -1); [ -n "$APP_PID" ] && break; sleep 0.5; done
  CHILDREN+=("$APP_PID")
}
perms_quit() { osascript -e 'tell application "LectureLive Canary" to quit' > /dev/null 2>&1; app_wait_exit 15 || app_signal TERM; }
# Waits for the check's report. A permission prompt is answered only when its text is recognised: the microphone prompt with `answer`, a Documents
# prompt with OK (the packaged app's ledger lives there). Anything else, a password or Keychain dialog above all, is never touched:
# the stage stops and says what it saw.
dialog_text() {
  osascript -e 'tell application "System Events"' -e 'set out to ""' -e 'repeat with p in (processes whose name is "UserNotificationCenter" or name is "SecurityAgent" or name is "coreauthd")' -e 'try' -e 'repeat with w in windows of p' -e 'set out to out & (name of p) & ": " & (name of every static text of w as string) & linefeed' -e 'end repeat' -e 'end try' -e 'end repeat' -e 'return out' -e 'end tell' 2> /dev/null
}
perms_wait() { # report name, answer, seconds
  local end=$(( $(date +%s) + $3 )) seen=""
  while [ "$(date +%s)" -lt "$end" ]; do
    app_report "$1" && return 0
    local t; t=$(dialog_text)
    if [ -n "$t" ] && [ "$t" != "$seen" ]; then
      seen=$t; log "  a dialog: $t"
      if echo "$t" | grep -qi "microphone"; then click_dialog "$2" | sed 's/^/  pressed (microphone): /'
      elif echo "$t" | grep -qi "documents"; then click_dialog "OK" | sed 's/^/  pressed (documents): /'
      else log "  UNRECOGNISED dialog: not touched, stage stopping"; return 2; fi
    fi
    sleep 1
  done
  return 1
}

stage_gui_perms_mic() {
  gui_prep > /dev/null; STAGE=gui-perms-mic; EVID=(); STAGE_FAIL=0; BLOCKED=0; PARTIAL=0
  new_lecture gui-perms-mic
  [ -d "$PERMS_APP" ] || { BLOCKED=1; check "the packaged app is built" false; finish_stage V16 "no packaged app"; return; }
  # 1. Undetermined, then the prompt is refused.
  tccutil reset Microphone "$PERMS_ID" > /dev/null 2>&1
  perms_launch idle "$FOLDER"
  perms_wait idle "Don’t Allow" 40 || [ "$?" = 1 ] || { BLOCKED=1; check "only recognised dialogs appeared" false; finish_stage V16 "an unrecognised dialog"; perms_quit; return; }
  sleep 8; perms_quit # macOS takes a few seconds to carry the answer to the next launch
  # 2. Denied: the app says so, with the fix-it, and does not start.
  : > "$OUT/app.$STAGE.log"; perms_launch idle "$FOLDER"
  perms_wait idle "Allow" 25
  local rep; rep=$(app_report_file idle)
  check "the second launch reported a failed start" 'jq -e "[.steps[] | select(.step == \"failed\")] | length == 1" "$rep" > /dev/null'
  check "the microphone reads denied" 'jq -e "[.steps[] | select(.step == \"failed\") | .detail.microphone] | .[0] == \"denied\"" "$rep" > /dev/null'
  note "the message shown: $(jq -r '[.steps[] | select(.step == "failed") | .detail.shown] | .[0]' "$rep" | cut -c1-200)"
  check "the message names System Settings" 'jq -e "[.steps[] | select(.step == \"failed\") | .detail.shown] | .[0] | test(\"System Settings\")" "$rep" > /dev/null'
  local id; id=$(app_window); [ -n "$id" ] || id=$("$LL" canary windows 2> /dev/null | awk '$2 == "LectureLive" && $3 == "Canary" && $4 == "LectureLive" {print $1; exit}')
  [ -n "$id" ] && "$LL" canary capture --id "$id" --out "$OUT/perms-mic-denied.png" > /dev/null 2>&1
  note "photograph: perms-mic-denied.png"
  perms_quit
  # 3. Turned back on: reset, answer the prompt with OK, and the lecture starts.
  tccutil reset Microphone "$PERMS_ID" > /dev/null 2>&1
  perms_launch idle "$FOLDER"
  perms_wait idle "Allow" 40
  check "after it is allowed again the lecture starts" 'jq -e "[.steps[] | select(.step == \"started\" and .ok)] | length == 1" "$rep" > /dev/null'
  check "the microphone reads granted" 'jq -e "[.steps[] | select(.step == \"started\") | .detail.microphone] | .[0] == \"granted\"" "$rep" > /dev/null'
  perms_quit
  finish_stage V16 "the packaged app's microphone refused, shown with its fix-it, and allowed again (its own permission, not Terminal's)"
}

stage_gui_perms_screen() {
  gui_prep > /dev/null; STAGE=gui-perms-screen; EVID=(); STAGE_FAIL=0; BLOCKED=0; PARTIAL=0
  new_lecture gui-perms-screen
  [ -d "$PERMS_APP" ] || { BLOCKED=1; check "the packaged app is built" false; finish_stage V12 "no packaged app"; return; }
  # A saved window for this course, so the capture worker has one to look for.
  cp "$DATA/capture.json" "$OUT/capture.json.before" 2> /dev/null || echo '{}' > "$OUT/capture.json.before"
  jq --arg c "m71-gui-perms-screen" '.[$c] = {descriptor: {bundle_id: "us.zoom.xos", app: "Zoom", title: "Zoom Meeting", width: 1168, height: 733}, region: {x: 0.06, y: 0.2, w: 0.87, h: 0.78}}' "$OUT/capture.json.before" > "$OUT/capture.json.new" && mv "$OUT/capture.json.new" "$DATA/capture.json"
  tccutil reset ScreenCapture "$PERMS_ID" > /dev/null 2>&1
  perms_launch idle "$FOLDER"
  perms_wait idle "Deny" 40
  local rep; rep=$(app_report_file idle)
  check "the lecture started and went on recording" 'jq -e "[.steps[] | select(.step == \"capture\") | .detail.recording] | .[0] == true" "$rep" > /dev/null'
  note "the strip said: $(jq -c '[.steps[] | select(.step == "capture") | .detail] | .[0]' "$rep" | cut -c1-200)"
  check "capture says Screen Recording is off" 'jq -e "[.steps[] | select(.step == \"capture\") | .detail.state] | .[0] == \"denied\"" "$rep" > /dev/null'
  local id; id=$(app_window); [ -n "$id" ] || id=$("$LL" canary windows 2> /dev/null | awk '$2 == "LectureLive" && $3 == "Canary" && $4 == "LectureLive" {print $1; exit}')
  [ -n "$id" ] && "$LL" canary capture --id "$id" --out "$OUT/perms-screen-denied.png" > /dev/null 2>&1
  sleep 5
  check "the recording kept growing while capture was denied" '[ "$(stat -f %z "$FOLDER"/recordings/*.wav 2> /dev/null | head -1)" -gt 100000 ]'
  perms_quit
  cp "$OUT/capture.json.before" "$DATA/capture.json" # this stage's course entry goes; the person's own are as they were
  finish_stage V12 "the packaged app with Screen Recording reset: the strip says so and the lecture keeps recording (left reset; System Settings grants it again)"
}
