#!/usr/bin/env bash
# Runs mutmut 3.8.1 over one corpus project with one child per CPU, timing
# it, and writes its verdicts to <out>/mutmut-results.json.
#   run-mutmut.sh <project> <dir> <out>
set -uo pipefail
project=$1
dir=$(cd "$2" && pwd)
out=$3
mkdir -p "$out"
out=$(cd "$out" && pwd)
here=$(cd "$(dirname "$0")" && pwd)
"$here/mutmut-config.sh" "$project" "$dir/src"
cd "$dir/src"
rm -rf mutants
started=$(date +%s.%N)
"$dir/.venv/bin/mutmut" run --max-children "$(nproc)" > "$out/mutmut-run.log" 2>&1
exit_code=$?
ended=$(date +%s.%N)
"$dir/.venv/bin/python" - "$out" "$started" "$ended" "$exit_code" <<'PY'
import json, sys
from pathlib import Path
from mutmut.stats import status_by_exit_code
out, started, ended, exit_code = sys.argv[1:]
results = {}
for meta in sorted(Path("mutants").rglob("*.meta")):
    for name, code in json.loads(meta.read_text())["exit_code_by_key"].items():
        results[name] = status_by_exit_code[code]
json.dump(results, open(f"{out}/mutmut-results.json", "w"), indent=0, sort_keys=True)
untested = {"no tests", "not checked", "skipped", "caught by type check"}
tested = sum(1 for status in results.values() if status not in untested)
seconds = float(ended) - float(started)
json.dump({"seconds": seconds, "mutants": len(results), "tested": tested,
           "secondsPerTestedMutant": seconds / max(tested, 1), "exit": int(exit_code)},
          open(f"{out}/mutmut-timing.json", "w"), indent=1)
print(open(f"{out}/mutmut-timing.json").read())
PY
