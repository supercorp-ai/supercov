#!/bin/bash
# Diagnosis: runs the strong pass ("$@") with Node's diagnostic report written on a fatal error
# (its JavaScript stack and heap state, in $DIAG_OUT/report), and every second the five largest
# Node processes by resident memory in $DIAG_OUT/memory.log. Used as SUPERCOV_BENCH_STRONG_WRAP.
out="${DIAG_OUT:-/tmp}"
mkdir -p "$out/report"
export NODE_OPTIONS="${NODE_OPTIONS:+$NODE_OPTIONS }--report-on-fatalerror --report-directory=$out/report"
"$@" &
pid=$!
while kill -0 $pid 2>/dev/null; do
  { echo "=== $(date +%T.%N | cut -c1-12)"; ps -eo pid,ppid,etimes,rss,args --sort=-rss | grep -E 'node|vitest' | grep -v grep | cut -c1-220 | head -5; } >> "$out/memory.log"
  sleep 1
done
wait $pid
