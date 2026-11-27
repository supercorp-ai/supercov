#!/usr/bin/env bash
# The target runner Self Coverage measures through (self-coverage.yml):
# Supercov runs each test as its binary given `--exact NAME`, and this times
# each, appending its name, seconds and processor seconds to $TEST_SECONDS.
# Given only on the command line, it reaches no project a test runs Cargo in
# itself. Committed, not written by each job: the build job and every slice
# name it by the same path, so the slices reuse the build job's instrumented
# build (Supercov keys a build on the command before its `--`).
name=""; previous=""
for argument in "$@"; do
  [ "$argument" = --list ] && exec "$@"
  [ "$previous" = --exact ] && name=$argument
  previous=$argument
done
[ -n "$name" ] && [ -n "${TEST_SECONDS:-}" ] || exec "$@"
# Its seconds and the processor seconds it and its children used: tests run
# side by side, so what one costs a slice is the latter.
TIMEFORMAT='%R %U %S'
report=$(mktemp)
{ time "$@" 2>&3; } 3>&2 2>"$report"
status=$?
read -r real user system < "$report"
rm -f "$report"
awk -v name="$name" -v real="$real" -v cpu="$(awk -v u="$user" -v s="$system" 'BEGIN { print u + s }')" \
  'BEGIN { printf "%s\t%.1f\t%.1f\n", name, real, cpu }' >> "$TEST_SECONDS"
exit $status
