#!/bin/bash
# V14: the lecture is killed without warning (SIGKILL) and started again. The recording is repaired, the lecture
# resumes, the utterance the kill cut off is recovered, and the audit is whole.
stage_crash() {
  new_lecture crash
  cli_start
  wait_until 30 "the CLI to be transcribing" 'log_has transcribing' || { check "the CLI started" false; finish_stage V14 "did not start"; return; }
  say_bh "The first marker is a lighthouse." || { finish_stage V14 "blocked"; return; }
  wait_until 25 "the lighthouse" 'tx_has lighthouse'
  say_bh "The second marker is a harpoon, said just before the kill."
  sleep 1
  cli_signal KILL
  cli_wait_exit 10
  check "the kill ended it" '! cli_alive'
  check "the recording was left open, as a crash leaves it" '[ "$(sidecar "[.recordings[] | select(.state == \"open\")] | length")" -ge 1 ]'
  cli_close
  echo "--- after the kill ---" >> "$CLI_LOG"
  cli_start --secs 40
  cli_wait_exit 120
  check "the start after it repaired the recording" 'log_has repaired'
  check "the lecture resumed" 'log_has "resumed\|recovering\|recovered"'
  check "nothing is left open" '[ "$(sidecar "[.recordings[] | select(.state == \"open\")] | length")" = 0 ]'
  check "the line before the kill is in the transcript" 'tx_has lighthouse'
  check "the utterance the kill cut off was recovered" 'tx_has harpoon'
  audit
  check "the audit is whole (0 unexplained, 0 waiting)" '[ "$AUDIT_RC" = 0 ]'
  finish_stage V14 "SIGKILL of the CLI mid-lecture, then a new session in the same folder"
}
