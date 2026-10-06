#!/bin/bash
# V10: the lecture loses the network twice, held (nothing passes, as Wi-Fi drops) and refused. Everything said is in the
# transcript at the end, the gaps are recorded and recovered, and the audit is whole. Only this lecture goes through
# the harness's own forwarder; the real network, Zoom and the Python tool never do.
stage_net() {
  new_lecture net
  [ "$FORWARDER_OK" = 1 ] || { check "the test forwarder works (self-test)" false; BLOCKED=1; finish_stage V10 "no working forwarder"; return; }
  CLI_ENV=("LECTURELIVE_API_ADDR=$FWD")
  link up
  cli_start
  wait_until 30 "the CLI to be transcribing" 'log_has transcribing' || { check "the CLI started" false; finish_stage V10 "did not start"; return; }
  say_bh "The first marker is a telescope." || { finish_stage V10 "blocked"; return; }
  wait_until 25 "the telescope" 'tx_has telescope'
  link hold
  sleep 2
  say_bh "The second marker is a glacier."; say_bh "The third marker is a compass."
  sleep 52 # the link has been held for about a minute
  link up
  sleep 30 # a new connection, and recovery of what was said while it was down
  say_bh "The fourth marker is a violin."
  wait_until 25 "the violin" 'tx_has violin'
  link refuse
  sleep 2
  say_bh "The fifth marker is a meadow."
  sleep 25
  link up
  sleep 30
  say_bh "The sixth marker is a lantern."
  wait_until 25 "the lantern" 'tx_has lantern'
  cli_signal INT # the ordinary stop waits for recovery
  cli_wait_exit 150
  check "the lecture ended cleanly" '[ "${CLI_RC:-99}" = 0 ]'
  local missing=""; local w
  for w in telescope glacier compass violin meadow lantern; do tx_has "$w" || missing="$missing $w"; done
  check "every marker is in the transcript, including the ones said while the link was down" '[ -z "$missing" ]'
  [ -n "$missing" ] && note "missing:$missing"
  check "the CLI said it was reconnecting" 'log_has "reconnect\|retrying\|offline"'
  check "gaps were recorded" '[ "$(sidecar "[.gaps[] | select(.kind | startswith(\"stt_\"))] | length")" -ge 1 ]'
  check "every transcript gap was recovered" '[ "$(sidecar "[.gaps[] | select(.kind | startswith(\"stt_\")) | select(.resolved | not)] | length")" = 0 ]'
  audit
  check "the audit is whole (0 unexplained, 0 waiting)" '[ "$AUDIT_RC" = 0 ]'
  finish_stage V10 "link held 60 s then refused 30 s, through the harness's own forwarder"
}
