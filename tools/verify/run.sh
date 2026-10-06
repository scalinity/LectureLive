#!/bin/bash
# The autonomous verification harness. It runs what docs/VERIFICATION.html asks of a person, wherever a machine can: real
# transcription of synthetic speech played into BlackHole, a forwarder that takes the network from the lecture alone,
# a scratch disk image, a virtual input in place of a receiver, kills and quits, and the audit as the judge. It writes one
# verdict per check, with its evidence, and says what it could not do and why.
#
#   tools/verify/run.sh [--deadline HH:MM] [--only a,b] [--skip a,b] [--soak-min N] [--no-soak]
#
# Stages, in order: build pause net quit crash disk devgone gui_capture gui_quit gui_looks gui_drag (the soak runs beside
# them and is judged next), then unit. The gui_* stages drive the app and need the display awake; they never speak.
# Nothing here signals a process it did not start. It stops at its deadline, and when its spending passes COST_CAP.
DIR="$(cd "$(dirname "$0")" && pwd)"
source "$DIR/lib.sh"
for f in "$DIR"/stages/*.sh; do source "$f"; done

DEADLINE_HHMM="${DEADLINE_HHMM:-15:45}"; ONLY=""; SKIP=""; SOAK_MIN=31; SOAK=yes
while [ $# -gt 0 ]; do
  case "$1" in
    --deadline) DEADLINE_HHMM=$2; shift 2 ;;
    --only) ONLY=$2; shift 2 ;;
    --skip) SKIP=$2; shift 2 ;;
    --soak-min) SOAK_MIN=$2; shift 2 ;;
    --no-soak) SOAK=no; shift ;;
    *) echo "unknown argument $1"; exit 2 ;;
  esac
done
DEADLINE_EPOCH=$(deadline_epoch "$DEADLINE_HHMM")
trap cleanup EXIT

wanted() { # stage name
  case "$1" in gui_perms*) echo ",$ONLY," | grep -q ",$1," || return 1 ;; esac # experimental: a macOS dialog is answered, so only when asked for by name
  [ -n "$ONLY" ] && ! echo ",$ONLY," | grep -q ",$1," && return 1
  echo ",$SKIP," | grep -q ",$1," && return 1
  return 0
}

# How long each stage needs at most, so none starts that cannot finish before the deadline.
need() { case "$1" in build) echo 400;; unit) echo 420;; pause) echo 150;; net) echo 330;; quit) echo 240;; crash) echo 180;; disk) echo 200;; devgone) echo 360;; gui_capture) echo 540;; gui_quit) echo 330;; gui_looks) echo 360;; gui_drag) echo 120;; gui_faults) echo 660;; gui_bundle) echo 900;; gui_perms_mic) echo 300;; gui_perms_screen) echo 240;; *) echo 120;; esac; }

run_stage() {
  local s=$1
  wanted "$s" || { log "skip $s"; return; }
  if ! need_time "$(need "$s")"; then result "$s" SKIPPED "the deadline $DEADLINE_HHMM is too close"; return; fi
  if ! within_budget; then result "$s" SKIPPED "spending passed the cap of \$$COST_CAP"; return; fi
  log "=== stage $s ($(( $(time_left) / 60 )) min to the deadline, \$$(spent) spent)"
  "stage_$s"
}

log "harness run $STAMP; deadline $DEADLINE_HHMM; output $OUT"
log "preflight: python lecture running: $(pgrep -f '\.local/bin/lecture' | head -1 || true); key in .env: $(grep -qE '^GROK_API_KEY=.+' "$ROOT/.env" && echo yes || echo NO)"
run_stage build || { log "the build failed: nothing else can run"; exit 1; }
forwarder_start
[ "$SOAK" = yes ] && wanted soak && need_time $(( SOAK_MIN * 60 + 600 )) && audio_ok && soak_start "$SOAK_MIN"
for s in pause net quit crash disk devgone gui_capture gui_quit gui_looks gui_drag gui_faults gui_bundle gui_perms_mic gui_perms_screen; do run_stage "$s"; done
[ "$SOAK" = yes ] && wanted soak && [ -n "$SOAK_PID" ] && soak_finish
run_stage unit # heavy on the CPU, so after the soak has finished

# ---- the report ---------------------------------------------------------------------------------------------
{
  echo "# Verification run $STAMP"
  echo
  echo "Finished $(date '+%H:%M'); deadline was $DEADLINE_HHMM; spent \$$(spent) of \$$COST_CAP on synthetic lectures (courses m71-*)."
  echo
  echo "| Check | Verdict | What it showed |"
  echo "|---|---|---|"
  awk -F'\t' '{printf "| %s | %s | %s |\n", $1, $2, $3}' "$RESULTS"
  echo
  cat <<'TXT'
## Not automated, and why

- **V02, V04**: the slide detector on a live Zoom meeting, and Zoom in full screen with another app in front. They need a real meeting that a lecturer shares slides in; the synthetic deck equivalents run in gui_capture and in M5's full-screen check.
- **V03**: run by hand on the day it was asked (the packaged canary captured Zoom's meeting window, the shared slide legible): PASS, evidence in the app data folder's canary directory.
- **V07**: the study page opening in the default browser. Typesetting is a paid request and the app reads the Keychain for it; the open itself is a plain `open` of the file.
- **V08**: the Keychain "Always Allow" prompt needs the login password, which the harness never types.
- **V12, V16**: turning Screen Recording or the microphone off for Terminal would also take them from the Python tool and from this harness, and turning them back on needs a dialog click: not done unattended.
- **V09**: the Python tool and the Rust CLI writing the same study page was run in M6.
- **V06**: photographed (see looks/); the pictures still have to be read by someone.
TXT
} > "$OUT/SUMMARY.md"
ln -sfn "$OUT" "$DATA/verify/latest"
log "done: $OUT/SUMMARY.md"
osascript -e 'display notification "Verification run finished: see SUMMARY.md" with title "LectureLive harness"' >/dev/null 2>&1
