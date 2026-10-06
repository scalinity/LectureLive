#!/bin/bash
# V18 (the CLI half): the lecture is quit by SIGTERM, by SIGHUP, and by SIGTERM while paused. Each ends promptly with
# the conventional exit status, every recording finalized, no last snapshot spent, and a start afterwards that
# repairs nothing.
quit_once() { # name, signal, expected status, whether to pause first
  local name=$1 sig=$2 want=$3 paused=$4
  new_lecture "$name"
  cli_start
  wait_until 30 "the CLI to be transcribing" 'log_has transcribing' || { check "the CLI started" false; return; }
  say_bh "The quokka is sleeping in the shade." || return
  wait_until 25 "the words" 'tx_has quokka'
  if [ "$paused" = yes ]; then cli_cmd pause; wait_until 10 "the pause" 'log_has paused'; fi
  local t0; t0=$(date +%s)
  cli_signal "$sig"
  cli_wait_exit 20
  local took=$(( $(date +%s) - t0 ))
  check "SIG$sig ended it in ${took} s (under 12)" '[ "$took" -lt 12 ] && ! cli_alive'
  check "exit status $want" '[ "${CLI_RC:-0}" = "$want" ]'
  check "every recording is finalized" '[ "$(sidecar "[.recordings[] | select(.state != \"finalized\")] | length")" = 0 ]'
  check "no last snapshot was spent (no notes cost)" '[ "$(cost_of notes)" = 0 ]'
  check "the lines said are kept in the transcript" 'tx_has quokka'
  # The next start has nothing to repair or recover.
  : > "$CLI_LOG"; cli_close
  cli_start --secs 5
  cli_wait_exit 40
  check "the next start repairs nothing" '! log_has "repaired" && ! log_has "recovered"'
  audit
  check "the audit has nothing unexplained" '[ "$AUDIT_RC" != 1 ]'
  finish_stage "" "SIG$sig${paused:+, paused: $paused}"
}
stage_quit() {
  local ok=PASS i
  : > "$OUT/quit.evidence.txt"
  for spec in "quit-term TERM 143 no" "quit-hup HUP 129 no" "quit-paused TERM 143 yes"; do
    quit_once $spec
    cat "$OUT/$STAGE.evidence.txt" | sed "s/^/[$STAGE] /" >> "$OUT/quit.evidence.txt"
    [ "$STAGE_FAIL" = 1 ] && ok=FAIL; [ "$BLOCKED" = 1 ] && ok=BLOCKED
  done
  result V18-cli "$ok" "SIGTERM, SIGHUP, and SIGTERM while paused on the CLI (see quit.evidence.txt)"
}
