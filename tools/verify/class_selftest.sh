#!/bin/bash
# `class.sh selftest`: class.sh's window parsing and V04 logic against canned data (tools/verify/selftest/) and a stand-in for osascript.
# Sourced by class.sh, never run. It needs no Zoom and changes nothing but a scratch folder ($OUT, made by class.sh); osascript is a
# function here, so the real one cannot be reached.
FX="$DIR/selftest"

# Zoom's full screen state is $AX/fs (0 or 1). entry_after: the entry takes effect on that attempt (never: it errors). leave_after: the
# restore takes effect on that read of the state (never: it does not). The window has no "Zoom Meeting" name while it is full screen.
osascript() {
  local s="$*" n
  echo "$s" >> "$AX/calls.log"
  case "$s" in
    *"return n"*)
      if [ -f "$AX/leaving" ] && [ "$(<"$AX/fs")" = 1 ] && [ "$(<"$AX/leave_after")" != never ]; then
        n=$(( $(<"$AX/reads") + 1 )); echo "$n" > "$AX/reads"
        [ "$n" -ge "$(<"$AX/leave_after")" ] && echo 0 > "$AX/fs"
      fi
      cat "$AX/fs" ;;
    *"whose name contains"*"to false"*) echo 'execution error: Cannot get window 1 of process "zoom.us" whose name contains "Zoom Meeting". Invalid index. (-1719)' >&2; return 1 ;;
    *"to false"*) touch "$AX/leaving" ;;
    *"to true"*)
      n=$(( $(<"$AX/tries") + 1 )); echo "$n" > "$AX/tries"
      if [ "$(<"$AX/fs")" = 1 ] || [ "$(<"$AX/entry_after")" = never ]; then echo "execution error: no such window (-1719)" >&2; return 1; fi
      [ "$n" -ge "$(<"$AX/entry_after")" ] && echo 1 > "$AX/fs" ;;
  esac
  return 0
}
ax() { rm -rf "$AX"; mkdir -p "$AX"; echo "$1" > "$AX/fs"; echo "$2" > "$AX/entry_after"; echo "$3" > "$AX/leave_after"; echo 0 > "$AX/tries"; echo 0 > "$AX/reads"; } # fs entry_after leave_after

png() { # path width height: a PNG of that size, made from a 1x1 one
  local cache="$OUT/png-$2x$3.png"
  if [ ! -f "$cache" ]; then
    base64 -D <<< 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==' > "$OUT/one.png"
    sips -z "$3" "$2" "$OUT/one.png" --out "$cache" > /dev/null
  fi
  cp "$cache" "$1"
}
epoch() { date -j -f '%Y-%m-%dT%H:%M:%S' "$1" +%s; }

cmd_selftest() {
  local fails=0 got rc FOLDER="$OUT/folder" AX="$OUT/ax" RESULTS="$OUT/results.tsv"
  unset ZOOM_WINDOW_OVERRIDE CANARY_WINDOWS_FIXTURE
  ok() { if [ "$2" = "$3" ]; then echo "PASS  $1"; else echo "FAIL  $1 (wanted '$2', got '$3')"; fails=$(( fails + 1 )); fi; }
  has() { case "$3" in *"$2"*) echo "PASS  $1" ;; *) echo "FAIL  $1 (no '$2' in: $3)"; fails=$(( fails + 1 )) ;; esac; }
  lacks() { case "$3" in *"$2"*) echo "FAIL  $1 ('$2' is in: $3)"; fails=$(( fails + 1 )) ;; *) echo "PASS  $1" ;; esac; }
  [ "$(type -t osascript)" = function ] || { echo "FAIL  the osascript stand-in is not in place: stopping before anything could reach the real one"; return 1; }

  # zoom_window against the real format of `canary windows` (id, app, title words, WxH)
  got=$(CANARY_WINDOWS_FIXTURE="$FX/windows-meeting.txt" zoom_window); ok "zoom_window finds the meeting window: id and size" "938 1168x733" "$got"
  got=$(CANARY_WINDOWS_FIXTURE="$FX/windows-no-meeting.txt" zoom_window); ok "zoom_window ignores Zoom Login, Zoom Workplace, the Healthcheck and a Chrome tab titled 'Join from Zoom Workplace app - Zoom'" "" "$got"
  : > "$OUT/empty"; got=$(CANARY_WINDOWS_FIXTURE="$OUT/empty" zoom_window); ok "zoom_window answers nothing when the window list is empty" "" "$got"

  # the counter's time selection, on a canned sidecar (offsets "-04:00", fractions of seconds, both ends included)
  mkdir -p "$FOLDER/slides" "$FOLDER/.live_notes" "$OUT/detector"; cp "$FX/sidecar.v2.json" "$FOLDER/.live_notes/fixture.v2.json"; cp "$FX/samples.jsonl" "$OUT/detector/samples.jsonl"
  local A=2026-10-06T16:30:10 B=2026-10-06T16:35:20
  got=$(v04_slides "$A" "$B" | cut -f2 | paste -sd' ' -); ok "v04_slides keeps exactly the slides between the two times" "slides/in_1.png slides/in_2.png slides/in_3.png" "$got"
  got=$(v04_slides "$A" "$B" | cut -f1 | head -1); ok "v04_slides gives the time of day of the first" "16:30:15" "$got"

  # the verdict: sizes of the slides in the window against the widest earlier one (1600 px here; 80% of it is 1280)
  judge() { # in_1 in_2 in_3 widths (heights are 900): runs v04_judge over the canned window
    png "$FOLDER/slides/early_1.png" 1600 900; png "$FOLDER/slides/early_2.png" 1580 890; png "$FOLDER/slides/late_1.png" 1600 900
    png "$FOLDER/slides/in_1.png" "$1" 900; png "$FOLDER/slides/in_2.png" "$2" 900; png "$FOLDER/slides/in_3.png" "$3" 900
    v04_judge "$(epoch $A)" "$(epoch $B)" > "$OUT/table.txt"
  }
  judge 1600 1600 1590; ok "three whole slides: PASS" PASS "$V04_STATUS"; has "  the note counts them" "3 slides arrived whole" "$V04_NOTE"; has "  and the detector's window frames inside the window only" "4 distinct window frames" "$V04_NOTE"
  judge 1280 1600 1600; ok "a slide exactly 80% as wide is whole: PASS" PASS "$V04_STATUS"
  judge 1279 1600 1600; ok "a slide just under 80% as wide is cropped: FAIL" FAIL "$V04_STATUS"; has "  the table marks it" "CROPPED" "$(cat "$OUT/table.txt")"
  judge 706 706 706; ok "all cropped (today's V04): FAIL" FAIL "$V04_STATUS"; has "  and the note says how many and how wide" "3 of 3 slides arrived cropped (706x900 706x900 706x900)" "$V04_NOTE"
  local sc="$FOLDER/.live_notes/fixture.v2.json"
  jq '.slides |= map(select(.file | startswith("slides/in_") | not))' "$FX/sidecar.v2.json" > "$sc"; judge 1600 1600 1600
  ok "no slide in the window: INCONCLUSIVE, not PASS or FAIL" INCONCLUSIVE "$V04_STATUS"; has "  and it says Zoom drew frames" "Zoom drew 4 distinct window frames but no slide arrived" "$V04_NOTE"
  rm "$OUT/detector/samples.jsonl"; judge 1600 1600 1600; has "  with no detector recording it says that, not a count" "no detector recording" "$V04_NOTE"; cp "$FX/samples.jsonl" "$OUT/detector/samples.jsonl"
  jq '.slides |= map(select(.file | startswith("slides/early_") | not))' "$FX/sidecar.v2.json" > "$sc"; judge 1600 1600 1600
  ok "no earlier slide to compare with: INCONCLUSIVE" INCONCLUSIVE "$V04_STATUS"; has "  and it says why" "no earlier slide" "$V04_NOTE"
  echo 'not json' > "$sc"; judge 1600 1600 1600
  ok "an unreadable sidecar: INCONCLUSIVE, never '0 slides'" INCONCLUSIVE "$V04_STATUS"; has "  and the reason is in the note" "could not be read" "$V04_NOTE"; lacks "  without a count" "0 slides" "$V04_NOTE"
  rm "$sc"; judge 1600 1600 1600; ok "no sidecar at all: INCONCLUSIVE" INCONCLUSIVE "$V04_STATUS"

  # V04 as a whole, with the stand-in for osascript: Zoom's state read back, the restore finding the window without its name, a re-run
  export V04_SECS=1 V04_POLL=0.05 V04_SETTLE=0
  v04() { # whole|cropped: one earlier slide and one that arrives while V04 runs; runs cmd_v04 in a subshell (it traps and exits)
    rm -rf "$FOLDER"; mkdir -p "$FOLDER/slides" "$FOLDER/.live_notes"; png "$FOLDER/slides/early.png" 1600 900
    [ "$1" = whole ] && png "$FOLDER/slides/in.png" 1600 900 || png "$FOLDER/slides/in.png" 706 395
    jq -n --arg t "$(date -v+1S +%Y-%m-%dT%H:%M:%S).500000-04:00" '{version: 2, slides: [{index: 1, file: "slides/early.png", shown_at: "2026-01-01T09:00:00.000000-04:00"}, {index: 2, file: "slides/in.png", shown_at: $t}]}' > "$FOLDER/.live_notes/fixture.v2.json"
    printf 'CLI_PID=%s\nHOLD_PID=0\nFOLDER=%q\nSTART_EPOCH=%s\nMINUTES=130\nZOOM_ID=938\n' "${CLI_PID_FOR_TEST:-$$}" "$FOLDER" "$(date +%s)" > "$STATE"
    printf 'V01-class\tPASS\tthe class itself\nV04\tINCONCLUSIVE\t0 slides arrived (an earlier run)\n' > "$RESULTS"
    ( cmd_v04 ) > "$OUT/v04.out" 2>&1; rc=$?; got=$(<"$OUT/v04.out")
  }
  v04s() { grep -c $'^V04\t' "$RESULTS"; }
  ax 0 1 3; v04 whole
  ok "V04 whole slide: exit 0" 0 "$rc"; has "  Zoom was put in full screen, confirmed, and out again, confirmed" "out of full screen again (confirmed" "$got"
  ok "  Zoom is not full screen at the end" 0 "$(<"$AX/fs")"; ok "  one V04 line, the earlier one replaced" 1 "$(v04s)"; has "  PASS" $'V04\tPASS\t' "$(<"$RESULTS")"; has "  the other results are kept" $'V01-class\tPASS' "$(<"$RESULTS")"
  ax 0 1 3; v04 cropped; has "V04 cropped slide: FAIL is written" $'V04\tFAIL\t' "$(<"$RESULTS")"; ok "  still one V04 line" 1 "$(v04s)"
  ax 0 2 2; v04 whole; ok "V04 entry taking effect on the second try (retry): exit 0" 0 "$rc"; ok "  two attempts" 2 "$(<"$AX/tries")"
  ax 0 never 3; v04 whole; ok "V04 entry failing: refuses, exit 1" 1 "$rc"; has "  says so" "V04 not started" "$got"; ok "  writes no result of its own (the earlier line stays)" $'V04\tINCONCLUSIVE\t0 slides arrived (an earlier run)' "$(grep $'^V04\t' "$RESULTS")"
  lacks "  and does not touch Zoom's windows" "to false" "$(cat "$AX/calls.log")"
  ax 0 9 3; v04 whole; ok "V04 entry that cannot be confirmed: refuses, exit 1" 1 "$rc"; ok "  and leaves Zoom out of full screen" 0 "$(<"$AX/fs")"
  ax 0 1 never; v04 whole; ok "V04 restore that never works: exit 1" 1 "$rc"; has "  says it in capitals" "COULD NOT BE TAKEN OUT OF FULL SCREEN" "$got"; has "  and what to do" "Ctrl-Cmd-F" "$got"
  has "  and still writes the result" $'V04\tPASS\t' "$(<"$RESULTS")"; has "  with the warning in it" "ZOOM WAS LEFT IN FULL SCREEN" "$(<"$RESULTS")"
  printf 'V01-class\tPASS\tthe class itself\n' > "$RESULTS"; ax 1 1 2; ( v04_interrupted ) > "$OUT/v04.out" 2>&1; rc=$?
  ok "V04 interrupted: exit 130" 130 "$rc"; ok "  Zoom is out of full screen" 0 "$(<"$AX/fs")"; ok "  no result is written" 0 "$(grep -c $'^V04\t' "$RESULTS")"
  ax 0 1 3; CLI_PID_FOR_TEST=999999 v04 whole; ok "V04 with the class's CLI gone: refuses, exit 1" 1 "$rc"; ok "  and does not touch Zoom" "" "$(cat "$AX/calls.log" 2> /dev/null)"

  if [ "$fails" = 0 ]; then echo "selftest: all checks passed"; else echo "selftest: $fails check(s) FAILED"; fi
  [ "$fails" = 0 ]
}
