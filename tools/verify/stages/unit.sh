#!/bin/bash
# Everything the automated suites cover, run again on the tree as it is. The failures known before M7.1 are named, so a new
# one stands out: three reconnect-timing tests in the STT gate and one terminal test whose needle breaks against today's date.
# A failure that is not known is run again alone: one that passes then is a timing flake under load, and said so, not a pass.
KNOWN_FAILS='a_refusal_stops_stt_without_a_reconnect_loop|a_15_s_disconnect|a_45_s_disconnect|a_300_s_disconnect|the_endpoint_fixture_writes_exact|the_failures_scenario_walks_the_table'
stage_unit() {
  STAGE=unit; EVID=(); STAGE_FAIL=0; BLOCKED=0; PARTIAL=0
  ( cd "$ROOT" && cargo test --workspace --no-fail-fast 2>&1 ) > "$OUT/unit.log" 2>&1
  local passed failed new t still=""
  passed=$(grep -E "^test result" "$OUT/unit.log" | sed -E 's/.* ([0-9]+) passed.*/\1/' | paste -sd+ - | bc)
  failed=$(grep -E "^test .* \.\.\. FAILED" "$OUT/unit.log" | sed -E 's/^test ([^ ]+) .*/\1/' | sort -u)
  new=$(echo "$failed" | grep -vE "$KNOWN_FAILS" | grep -v '^$')
  note "$passed tests passed; failing: $(echo "$failed" | grep -c .) ($(echo "$failed" | tr '\n' ' '))"
  for t in $new; do
    if ( cd "$ROOT" && cargo test --workspace -- "${t##*::}" 2>&1 ) | grep -qE "^test .* \.\.\. FAILED"; then still="$still $t"
    else note "$t failed in the full run and passed when run alone: a timing flake under load"; PARTIAL=1; fi
  done
  check "no failure beyond the known ones that also fails alone" '[ -z "$still" ]'
  [ -n "$still" ] && note "NEW failures: $still"
  (cd "$ROOT/apps/desktop" && npx vitest run 2>&1 | grep -E "Tests|Test Files") > "$OUT/vitest.txt"
  check "the frontend tests pass" 'grep -q "passed" "$OUT/vitest.txt" && ! grep -q failed "$OUT/vitest.txt"'
  note "$(tr '\n' ' ' < "$OUT/vitest.txt")"
  finish_stage UNIT "the whole workspace's tests, with the known failures named"
}
