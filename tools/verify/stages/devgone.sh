#!/bin/bash
# V15 and V11: the input goes away and comes back. Alone (V15) the lecture records nothing meanwhile and goes on from the
# same input when it is back, never switching by itself. Mixed with Zoom's audio (V11) the surviving source continues
# and a gap marks where the other was missing. A virtual input stands in for the receiver: a real unplug is the same event
# to the source (the device vanishes from the system's list and returns).
stage_devgone() {
  local vi_ctl="$OUT/virtual_input.control"
  echo up > "$vi_ctl"
  "$VINPUT" "$vi_ctl" > "$OUT/virtual_input.log" 2>&1 &
  CHILDREN+=("$!")
  wait_until 20 "the virtual input to be listed" '"$LL" inputs 2>/dev/null | grep -q "LL Verify Input"' || { STAGE=devgone; EVID=(); STAGE_FAIL=1; BLOCKED=1; check "a virtual input could be listed" false; finish_stage "V11 V15" "no virtual input"; return; }

  # V15: the only input, unplugged and plugged back.
  new_lecture devgone-single
  CLI_SRC=(--device "LL Verify Input")
  cli_start
  wait_until 30 "the CLI to be transcribing" 'log_has transcribing' || { check "the CLI started" false; finish_stage V15 "did not start"; return; }
  say_bh "The first marker is a violin." || { finish_stage V15 "blocked"; return; }
  wait_until 25 "the violin" 'tx_has violin'
  echo gone > "$vi_ctl"
  check "the CLI says the input is gone" 'wait_until 15 "the notice" "log_has \"unplugged\|gone\|disconnected\|waits for\""'
  sleep 2; say_bh "The second marker is a meadow, said while it is away."; sleep 6
  check "nothing said while it was away is transcribed" '! tx_has meadow'
  echo up > "$vi_ctl"
  sleep 4
  say_bh "The third marker is a lantern."
  wait_until 25 "the lantern" 'tx_has lantern'
  check "it goes on from the same input when it is back" 'tx_has lantern'
  cli_signal INT; cli_wait_exit 90
  check "two or more recordings (a new one after the return)" '[ "$(wavs)" -ge 2 ]'
  check "the absence is marked as a gap" '[ "$(sidecar "[.gaps[] | select(.kind == \"device_gone\")] | length")" -ge 1 ]'
  audit
  check "the audit is whole (the hole is explained by the gap)" '[ "$AUDIT_RC" = 0 ]'
  finish_stage V15 "the only input removed for 25 s and restored (a virtual input stands in for the receiver)"

  # V11: mixed with the loopback (Zoom's side), the other input removed and restored.
  echo up > "$vi_ctl"; sleep 2
  new_lecture devgone-mixed
  CLI_SRC=(--mixed "LL Verify Input")
  cli_start
  wait_until 30 "the CLI to be transcribing" 'log_has transcribing' || { check "the CLI started" false; finish_stage V11 "did not start"; return; }
  say_bh "The first marker is a compass." || { finish_stage V11 "blocked"; return; }
  wait_until 25 "the compass" 'tx_has compass'
  echo gone > "$vi_ctl"
  sleep 3
  say_bh "The second marker is a glacier, said while the receiver is away."
  wait_until 25 "the glacier" 'tx_has glacier'
  check "the surviving source carries on: what was said while the receiver was away is transcribed" 'tx_has glacier'
  echo up > "$vi_ctl"
  sleep 4
  say_bh "The third marker is a telescope."
  wait_until 25 "the telescope" 'tx_has telescope'
  check "after it returns, its audio is back in the mix" 'tx_has telescope'
  cli_signal INT; cli_wait_exit 90
  check "the absence is marked as a gap" '[ "$(sidecar "[.gaps[] | select(.kind == \"device_gone\")] | length")" -ge 1 ]'
  audit
  check "the audit is whole" '[ "$AUDIT_RC" = 0 ]'
  finish_stage V11 "the receiver side of a mixed source removed and restored (a virtual input stands in for it)"
  echo up > "$vi_ctl"
}
