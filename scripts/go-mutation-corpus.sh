#!/usr/bin/env bash
# The Go mutation corpus: public modules pinned at a tag, run under Gremlins
# 0.6.0 (every test per mutant) and under Supercov's strong pass, for
# scripts/gremlins-compare.mjs to join.
#
#   go-mutation-corpus.sh clone <project> <dir>
#   go-mutation-corpus.sh gremlins <dir> <out>
#   go-mutation-corpus.sh supercov <dir> <out> <supercov binary>
#
# Each run writes <out>/time.txt (wall seconds) and its log.
set -euo pipefail

command=$1
shift

case $command in
  clone)
    project=$1 dir=$2
    case $project in
      go-humanize) repo=dustin/go-humanize tag=v1.0.1 ;;
      btree) repo=google/btree tag=v1.1.3 ;;
      golang-lru) repo=hashicorp/golang-lru tag=v2.0.7 ;;
      uuid) repo=google/uuid tag=v1.6.0 ;;
      cast) repo=spf13/cast tag=v1.7.1 ;;
      *) echo "unknown project $project" >&2; exit 2 ;;
    esac
    git clone -q --depth 1 --branch "$tag" "https://github.com/$repo" "$dir"
    # Gremlins reads the module's name from go.mod's first line: a leading
    # comment (btree's licence) leaves every mutant NOT COVERED. Both tools
    # get the same go.mod.
    grep -v '^//' "$dir/go.mod" | sed '/./,$!d' > "$dir/go.mod.new"
    mv "$dir/go.mod.new" "$dir/go.mod"
    (cd "$dir" && go mod download)
    ;;
  gremlins)
    dir=$1 out=$2
    mkdir -p "$out"
    GOBIN="$out/bin" go install github.com/go-gremlins/gremlins/cmd/gremlins@v0.6.0
    start=$(date +%s.%N)
    # -count=1: Gremlins sets each mutant's time limit from its coverage
    # run, and a cached one takes milliseconds, which timed out every mutant.
    (cd "$dir" && GOFLAGS=-count=1 "$out/bin/gremlins" unleash --silent \
      --integration --coverpkg ./... --timeout-coefficient 10 \
      --invert-assignments --invert-logical --invert-loopctrl --remove-self-assignments \
      --invert-bitwise --invert-bwassign \
      -o "$out/report.json" . > "$out/log.txt" 2>&1) || true
    echo "gremlins $(echo "$(date +%s.%N) - $start" | bc)" > "$out/time.txt"
    ;;
  supercov)
    dir=$1 out=$2 binary=$3
    mkdir -p "$out"
    chmod +x "$binary"
    start=$(date +%s.%N)
    (cd "$dir" && "$binary" mutants -- go test ./... > "$out/log.txt" 2>&1) || true
    echo "supercov $(echo "$(date +%s.%N) - $start" | bc)" > "$out/time.txt"
    cp -R "$dir/.supercov/runs" "$out/runs"
    find "$dir/.supercov/strong" -name 'results.jsonl*' -exec cp {} "$out/" \; || true
    ;;
  probes)
    # What the value probes cost and what they spare: the measured run with
    # and without them, three times each, and the plan's unknown mutants and
    # candidate test runs from each.
    dir=$1 out=$2 binary=$3
    mkdir -p "$out"
    chmod +x "$binary"
    clock() { python3 -c 'import time; print(time.time())'; }
    {
      for round in 1 2 3; do
        for mode in plain probes; do
          rm -rf "$dir/.supercov"
          start=$(clock)
          if [ "$mode" = probes ]; then
            (cd "$dir" && SUPERCOV_MUTANTS=1 "$binary" -- go test ./... > /dev/null 2>&1) || true
          else
            (cd "$dir" && "$binary" -- go test ./... > /dev/null 2>&1) || true
          fi
          end=$(clock)
          plan=$(cd "$dir" && "$binary" runs latest mutants test --plan --json 2>/dev/null || echo '{}')
          weak=$(cd "$dir" && "$binary" runs latest mutants --json --limit 0 2>/dev/null || echo '{}')
          echo "{\"round\": $round, \"mode\": \"$mode\", \"seconds\": $(python3 -c "print($end - $start)"), \"plan\": $(echo "$plan" | python3 -c 'import json,sys; d=json.load(sys.stdin); print(json.dumps(d.get("summary", d.get("data", {}).get("summary"))))'), \"weak\": $(echo "$weak" | python3 -c 'import json,sys; d=json.load(sys.stdin); print(json.dumps((d.get("data") or {}).get("summary")))')}"
        done
        start=$(clock)
        (cd "$dir" && go test -count=1 ./... > /dev/null 2>&1) || true
        echo "{\"round\": $round, \"mode\": \"go test\", \"seconds\": $(python3 -c "print($(clock) - $start)")}"
      done
    } > "$out/probes.jsonl"
    cat "$out/probes.jsonl"
    ;;
  *) echo "unknown command $command" >&2; exit 2 ;;
esac
