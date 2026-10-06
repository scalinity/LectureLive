#!/bin/bash
# The binaries every other stage runs, built from the tree as it is, and the helper that stands in for a receiver.
stage_build() {
  STAGE=build; EVID=(); STAGE_FAIL=0; BLOCKED=0
  ( cd "$ROOT" && cargo build -p lecturelive-cli -p desktop 2>&1 && cargo build -p lecturelive-core --example netcut 2>&1 ) > "$OUT/build.log" 2>&1
  check "the CLI, the app and the forwarder build" '[ -x "$LL" ] && [ -x "$APPBIN" ] && [ -x "$NETCUT" ] && ! grep -q "^error" "$OUT/build.log"'
  VINPUT="$OUT/virtual_input"
  swiftc -O "$ROOT/tools/verify/virtual_input.swift" -o "$VINPUT" > "$OUT/swiftc.log" 2>&1
  check "the virtual receiver helper builds" '[ -x "$VINPUT" ]'
  finish_stage "" "build"
  [ "$STAGE_FAIL" = 1 ] && { result build FAIL "see build.log"; return 1; }
  return 0
}
