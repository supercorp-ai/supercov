#!/usr/bin/env bash
# Measures one corpus project with Supercov and runs its strong pass, timing
# each step; copies the run's mutants and verdicts to <out>. With `values`
# the run records its mutants' values (SUPERCOV_MUTANTS=1), and the plan
# decides more of them without running anything.
#   run-supercov.sh <project> <dir> <runner: pytest|unittest> <supercov> <out> [values]
set -uo pipefail
project=$1
dir=$(cd "$2" && pwd)
runner=$3
supercov=$4
out=$5
mkdir -p "$out"
out=$(cd "$out" && pwd)
cd "$dir/src"
rm -rf .supercov
export PATH="$dir/.venv/bin:$PATH"
# The same sources mutmut mutates: what `mutmut-config.sh` names.
case "$project" in
  python-slugify) export SUPERCOV_SOURCE_ROOTS=slugify ;;
  cachetools) export SUPERCOV_SOURCE_ROOTS=src/cachetools ;;
  parse) export SUPERCOV_SOURCE_ROOTS=parse ;;
  humanize) export SUPERCOV_SOURCE_ROOTS=src/humanize ;;
esac
case "$runner" in
  pytest) command=(python -m pytest -p no:cacheprovider) ;;
  unittest) command=(python -m unittest discover -s tests -t .) ;;
esac
now() { date +%s.%N; }
started=$(now)
if [ "${6:-}" = values ]; then export SUPERCOV_MUTANTS=1; fi
"$supercov" -- "${command[@]}" > "$out/measure.log" 2>&1
unset SUPERCOV_MUTANTS
measure_exit=$?
measured=$(now)
"$supercov" runs latest mutants test --json > "$out/strong.json" 2> "$out/strong.log"
strong_exit=$?
ended=$(now)
run=$(ls -d .supercov/runs/run_* | head -1)
cp "$run"/strong-mutation.json "$run"/mutants-listed.json "$out"/ 2>/dev/null
"$supercov" runs latest mutants --format jsonl --output "$out/weak.jsonl" > /dev/null 2>&1
for results in .supercov/strong/*/results.jsonl; do
  cat "$results" >> "$out/results.jsonl"
done
python3 - "$out" "$started" "$measured" "$ended" "$measure_exit" "$strong_exit" "$(nproc)" <<'PY'
import json, sys
out, started, measured, ended, measure_exit, strong_exit, cpus = sys.argv[1:]
verdicts = json.load(open(f"{out}/strong-mutation.json"))["mutants"]
ran = sum(1 for v in verdicts.values() if v.get("tests", 0) > 0)
json.dump({
    "measureSeconds": float(measured) - float(started),
    "strongSeconds": float(ended) - float(measured),
    "mutants": len(verdicts),
    "ran": ran,
    "strongSecondsPerRanMutant": (float(ended) - float(measured)) / max(ran, 1),
    "measureExit": int(measure_exit),
    "strongExit": int(strong_exit),
    "cpus": int(cpus),
}, open(f"{out}/timing.json", "w"), indent=1)
PY
cat "$out/timing.json"
