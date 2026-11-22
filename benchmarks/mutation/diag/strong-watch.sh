#!/bin/bash
# Diagnosis: runs the strong pass ("$@") keeping its stages, and every 30 s prints the newest
# stage's state to stderr: its assignments, how many claimed, each copy still without its end
# (with its last record), and the Node processes alive. Used as SUPERCOV_BENCH_STRONG_WRAP.
export SUPERCOV_STRONG_KEEP_STAGES=1
"$@" &
pid=$!
while kill -0 $pid 2>/dev/null; do
  sleep 30
  stage=$(ls -td .supercov/workspaces/*/*/.supercov/strong-stages/*/ 2>/dev/null | head -1)
  [ -n "$stage" ] || continue
  {
    echo "=== watch $(date +%T) stage $stage: $(ls $stage*/*.json 2>/dev/null | wc -l) assignments, $(wc -l < ${stage}claimed 2>/dev/null) claimed"
    for out in $stage*/*.out; do
      grep -q '"done"' "$out" || echo "open $out: $(tail -c 300 "$out" | tail -1)"
    done | head -20
    ps -eo pid,ppid,etimes,rss,args --sort=etimes | grep -E 'node|vitest' | grep -v grep | cut -c1-200 | head -40
  } >&2
done
wait $pid
