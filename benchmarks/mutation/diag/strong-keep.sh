#!/bin/bash
# Diagnosis: runs the strong pass ("$@") keeping what every copy reported, and archives it into
# $DIAG_OUT/stages.tgz (each copy's assignment and its records, with times). Used as
# SUPERCOV_BENCH_STRONG_WRAP.
export SUPERCOV_STRONG_KEEP_STAGES=1
"$@"
status=$?
dir=$(ls -d .supercov/workspaces/*/*/.supercov/strong-stages 2>/dev/null | head -1)
[ -n "$dir" ] && tar czf "${DIAG_OUT:-/tmp}/stages.tgz" -C "$dir" .
exit $status
