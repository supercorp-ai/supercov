#!/usr/bin/env bash
# Runs one part of a validation run as parts.tsv describes it, writing its
# results to <out>/<name>, where <name> is the directory compare-all.sh reads
# (supercov-<project>-<runner>[-values], overhead-<project>, mutmut-<project>
# or matrix-<project>-<part>).
#   part.sh <parts file> <part> <corpus dir> <supercov> <out>
#   part.sh <parts file> list    prints the parts as a JSON array
set -euo pipefail
parts=$1
here=$(cd "$(dirname "$0")" && pwd)
if [ "$2" = list ]; then
  grep -v '^#' "$parts" | awk 'NF { printf "%s%s", (n++ ? "," : "["), $1 } END { print "]" }'
  exit 0
fi
read -r _ project scenario shard < <(grep -v '^#' "$parts" | awk -v part="$2" '$1 == part')
corpus=$3
supercov=$4
mkdir -p "$5"
out=$(cd "$5" && pwd)
"$here/setup.sh" "$project" "$corpus"
case "$scenario" in
  supercov-*)
    runner=${scenario#supercov-}
    values=""
    case "$runner" in *-values) runner=${runner%-values}; values=values ;; esac
    "$here/run-supercov.sh" "$project" "$corpus" "$runner" "$supercov" "$out/supercov-$project-$runner${values:+-values}" $values ;;
  overhead)
    mkdir -p "$out/overhead-$project"
    "$here/overhead.sh" "$project" "$corpus" "$supercov" | tee "$out/overhead-$project/overhead.json" ;;
  mutmut)
    "$here/run-mutmut.sh" "$project" "$corpus" "$out/mutmut-$project" ;;
  matrix)
    "$here/mutmut-config.sh" "$project" "$corpus/src"
    "$corpus/.venv/bin/python" "$here/generate.py" "$corpus/src"
    mkdir -p "$out/matrix-$project-$2"
    "$corpus/.venv/bin/python" "$here/kill_matrix.py" "$corpus" --jobs "$(nproc)" --max-seconds 450 --baseline-runs 2 \
      --shard "$shard" --out "$out/matrix-$project-$2/kill-matrix.jsonl" ;;
  *) echo "part $2: unknown scenario '$scenario'" >&2; exit 2 ;;
esac
