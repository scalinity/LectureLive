#!/bin/bash
# The latest verdict for each check across several runs, oldest run first (a stage re-run after a harness fix replaces the
# earlier verdict): tools/verify/merge.sh RUN_DIR... > CONSOLIDATED.md
# A result about the app (its note says "in the app") is kept apart from the CLI's under the same check id.
echo "| Check | Verdict | What it showed |"
echo "|---|---|---|"
for d in "$@"; do cat "$d/results.tsv" 2>/dev/null; done |
  awk -F'\t' '{ id = $1; if ($3 ~ /in the app/ && id !~ /-app$/) id = id "-app"; verdict[id] = $2; note[id] = $3; seen[id] = NR } END { for (id in verdict) printf "%d\t%s\t%s\t%s\n", seen[id], id, verdict[id], note[id] }' |
  sort -n | awk -F'\t' '{ printf "| %s | %s | %s |\n", $2, $3, $4 }'
