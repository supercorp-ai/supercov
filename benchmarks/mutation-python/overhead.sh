#!/usr/bin/env bash
# The cost of a measured run and of its mutants' value probes on one corpus
# project: the suite alone, under `supercov --`, and under `supercov --` with
# SUPERCOV_MUTANTS=1, each the best of three, in seconds.
#   overhead.sh <project> <dir> <supercov>
set -uo pipefail
project=$1
dir=$(cd "$2" && pwd)
supercov=$3
cd "$dir/src"
export PATH="$dir/.venv/bin:$PATH"
case "$project" in
  python-slugify) export SUPERCOV_SOURCE_ROOTS=slugify ;;
  cachetools) export SUPERCOV_SOURCE_ROOTS=src/cachetools ;;
  parse) export SUPERCOV_SOURCE_ROOTS=parse ;;
  humanize) export SUPERCOV_SOURCE_ROOTS=src/humanize ;;
  markupsafe) export SUPERCOV_SOURCE_ROOTS=src/markupsafe ;;
  semver) export SUPERCOV_SOURCE_ROOTS=src/semver ;;
esac
command=(python -m pytest -q -p no:cacheprovider)
# humanize's pytest-benchmark tests size their rounds to the speed they
# measure, so their time does not grow with the code's: each runs once here.
[ "$project" = humanize ] && command+=(--benchmark-disable)
best() {
  local best=""
  for _ in 1 2 3; do
    local started ended took
    started=$(date +%s.%N)
    "$@" > /dev/null 2>&1
    ended=$(date +%s.%N)
    took=$(python3 -c "print(f'{$ended - $started:.3f}')")
    if [ -z "$best" ] || python3 -c "import sys; sys.exit(0 if $took < $best else 1)"; then best=$took; fi
  done
  echo "$best"
}
"$supercov" -- "${command[@]}" > /dev/null 2>&1  # warm the probe caches
plain=$(best "${command[@]}")
measured=$(best "$supercov" -- "${command[@]}")
export SUPERCOV_MUTANTS=1
valued=$(best "$supercov" -- "${command[@]}")
unset SUPERCOV_MUTANTS
# Where each kind of run spends its time, as Supercov times its phases.
{
  echo "measured: $("$supercov" -- "${command[@]}" 2>&1 | grep -F '[supercov] timings')"
  echo "values: $(SUPERCOV_MUTANTS=1 "$supercov" -- "${command[@]}" 2>&1 | grep -F '[supercov] timings')"
} >&2
echo "{\"project\":\"$project\",\"plain\":$plain,\"measured\":$measured,\"values\":$valued}"
