#!/bin/bash
# Diagnosis: runs the strong pass ("$@") with decoder-trace.mjs in Vitest's main process and the
# memory samples and fatal-error report of strong-report.sh. Used as SUPERCOV_BENCH_STRONG_WRAP.
export NODE_OPTIONS="${NODE_OPTIONS:+$NODE_OPTIONS }--import=$(cd "$(dirname "$0")" && pwd)/decoder-trace.mjs"
exec bash "$(dirname "$0")/strong-report.sh" "$@"
