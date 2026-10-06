#!/bin/bash
# V17 (the core of it): pause a lecture for a break and resume it. Words said before and after are transcribed, words
# said while paused are not, the lecture is two recordings with the pause between them, and the audit is whole.
stage_pause() {
  new_lecture pause
  local t0; t0=$(date +%s)
  cli_start
  wait_until 30 "the CLI to be transcribing" 'log_has transcribing' || { check "the CLI started" false; finish_stage V17 "did not start"; return; }
  say_bh "The pineapple is perfectly ripe today." || { finish_stage V17 "blocked"; return; }
  wait_until 25 "the first words" 'tx_has pineapple'
  check "words said before the pause are transcribed" 'tx_has pineapple'
  cli_cmd pause
  check "the pause is acknowledged" 'wait_until 10 "the pause notice" "log_has paused"'
  sleep 2
  say_bh "The volcano is erupting tonight."
  sleep 9 # live transcription answers within a few seconds; this is long enough for anything to show
  check "words said while paused are not transcribed" '! tx_has volcano'
  local recs_during; recs_during=$(wavs)
  cli_cmd resume
  check "the resume is acknowledged" 'wait_until 10 "the resume notice" "log_has resumed"'
  check "the resume starts a second recording" 'wait_until 20 "a second recording" "[ \$(wavs) -ge 2 ]"'
  say_bh "The lighthouse is shining brightly."
  wait_until 25 "the words after the pause" 'tx_has lighthouse'
  check "words said after the resume are transcribed" 'tx_has lighthouse'
  cli_signal INT # the ordinary stop: it waits for the transcript and takes the last snapshot
  cli_wait_exit 90
  local wall=$(( $(date +%s) - t0 ))
  check "the lecture ended cleanly" '[ "${CLI_RC:-99}" = 0 ]'
  check "two recordings, both finalized" '[ "$(sidecar "[.recordings[] | select(.state == \"finalized\")] | length")" = 2 ]'
  check "one pause, closed, in the sidecar" '[ "$(sidecar "(.pauses | length) == 1 and (.pauses[0].to != null)")" = true ]'
  local rec; rec=$(sidecar '[.recordings[].samples] | add // 0'); rec=$(( ${rec:-0} / 16000 ))
  note "wall clock ${wall} s, recorded ${rec} s: the pause cost nothing recorded"
  check "the recorded time is shorter than the wall clock by about the pause" '[ $(( wall - rec )) -ge 8 ]'
  audit
  check "the audit is whole (0 unexplained, 0 waiting), the hole explained by the pause" '[ "$AUDIT_RC" = 0 ] && echo "$AUDIT_OUT" | grep -q "a pause (explained)"'
  note "transcription billed for $(audio_seconds) s of audio"
  finish_stage V17 "pause and resume through the CLI, a real transcription service, synthetic speech"
}
