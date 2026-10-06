#!/bin/bash
# V01: thirty minutes of recording with no unexplained gaps. It runs in the background beside the other stages and
# stays silent, so it never changes what they hear; they only add speech it records. It is audited at the end.
soak_start() { # minutes
  SOAK_MIN=$1; SOAK_STAGE=soak
  SOAK_FOLDER="$OUT/soak/m71-soak/Weeks/Week 01"; mkdir -p "$SOAK_FOLDER"
  local fifo="$OUT/soak.in"; mkfifo "$fifo"; exec 4<>"$fifo"
  "$LL" lecture --plain --device BlackHole --dir "$SOAK_FOLDER" --secs $(( SOAK_MIN * 60 )) < "$fifo" > "$OUT/soak.cli.log" 2>&1 &
  SOAK_PID=$!; CHILDREN+=("$SOAK_PID")
  log "soak: recording $SOAK_MIN min in the background (pid $SOAK_PID)"
}
soak_finish() {
  STAGE=soak; EVID=(); STAGE_FAIL=0; BLOCKED=0; FOLDER="$SOAK_FOLDER"; CLI_LOG="$OUT/soak.cli.log"; CLI_PID=$SOAK_PID
  local wait_s=$(( $(time_left) - 120 )); [ "$wait_s" -lt 30 ] && wait_s=30
  if cli_alive; then log "soak: waiting for it to end (up to ${wait_s}s)"; cli_wait_exit "$wait_s"; fi
  if cli_alive; then
    check "the soak ran its full length" false
    cli_signal INT; cli_wait_exit 90
  fi
  local secs; secs=$(sidecar '[.recordings[].samples] | add // 0'); secs=$(( ${secs:-0} / 16000 ))
  note "recorded ${secs} s over $(wavs) recording(s)"
  check "at least $(( SOAK_MIN - 2 )) minutes were recorded" '[ "$secs" -ge $(( (SOAK_MIN - 2) * 60 )) ]'
  audit
  check "no unexplained or missing audio (audit exit 0)" '[ "$AUDIT_RC" = 0 ]'
  note "audit: $(echo "$AUDIT_OUT" | tail -2 | tr '\n' ' ')"
  note "gaps: $(sidecar '[.gaps[] | .kind] | group_by(.) | map({(.[0]): length}) | add')"
  finish_stage V01 "a $SOAK_MIN-minute recording of silent BlackHole with other stages speaking over it; the real-Zoom line still waits for a class"
  exec 4>&-
}
