#!/bin/bash
# V13: the disk fills up. The session stops cleanly naming the path, and a start after space is freed repairs the
# recording and carries on. Done on a 40 MB disk image of the harness's own, never a real disk.
stage_disk() {
  new_lecture disk
  local dmg="$OUT/LLVerify.dmg" vol=/Volumes/LLVerify
  hdiutil create -size 40m -fs HFS+ -volname LLVerify "$dmg" >/dev/null 2>&1 && hdiutil attach "$dmg" >/dev/null 2>&1
  if ! diskutil info "$vol" 2>/dev/null | grep -qi "Disk Image"; then
    check "a scratch disk image could be made" false; finish_stage V13 "no scratch disk"; return
  fi
  FOLDER="$vol/m71-disk/Weeks/Week 01"; mkdir -p "$FOLDER"
  cli_start
  wait_until 30 "the CLI to be transcribing" 'log_has transcribing' || { check "the CLI started" false; finish_stage V13 "did not start"; hdiutil detach "$vol" >/dev/null 2>&1; return; }
  say_bh "The first marker is a glacier." || { finish_stage V13 "blocked"; hdiutil detach "$vol" >/dev/null 2>&1; return; }
  wait_until 25 "the glacier" 'tx_has glacier'
  dd if=/dev/zero of="$vol/filler" bs=1m > /dev/null 2> "$OUT/disk.dd.txt" # runs until the 40 MB image is full
  check "the image is full (dd ran out of space)" 'grep -q "No space left" "$OUT/disk.dd.txt"'
  cli_wait_exit 60
  check "the session stopped by itself" '! cli_alive'
  check "it named the path or the cause" 'log_has "No space left\|session failed\|failed"'
  rm -f "$vol/filler"
  check "space is free again" '[ "$(df -k "$vol" | tail -1 | awk "{print \$4}")" -gt 5000 ]'
  cli_close
  echo "--- after freeing space ---" >> "$CLI_LOG"
  cli_start --secs 20
  cli_wait_exit 90
  check "the next start repaired the recording" 'log_has repaired'
  check "nothing is left open" '[ "$(sidecar "[.recordings[] | select(.state == \"open\")] | length")" = 0 ]'
  audit
  check "the audit has nothing unexplained" '[ "$AUDIT_RC" != 1 ]'
  note "audit: $(echo "$AUDIT_OUT" | tail -3 | tr '\n' ' ')"
  finish_stage V13 "a 40 MB scratch image filled under a recording"
  hdiutil detach "$vol" >/dev/null 2>&1; rm -f "$dmg"
}
