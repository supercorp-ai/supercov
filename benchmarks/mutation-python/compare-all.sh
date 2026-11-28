#!/usr/bin/env bash
# Joins every job's results of the Python mutation workflow: for each
# project, mutmut's ids for Supercov's mutants, the kill matrix's shards
# joined, each Supercov pass compared with it, and both tools' times. Prints
# a Markdown summary and writes the comparisons to <out>.
#   compare-all.sh <results dir> <corpus dir> <out>
set -uo pipefail
results=$(cd "$1" && pwd)
corpus=$2
mkdir -p "$3"
out=$(cd "$3" && pwd)
here=$(cd "$(dirname "$0")" && pwd)
repository=$(cd "$here/../.." && pwd)

roots() {
  case "$1" in
    python-slugify) echo "slugify slugify ." ;;
    cachetools) echo "src/cachetools src/cachetools src" ;;
    parse) echo "parse parse ." ;;
    humanize) echo "src/humanize src/humanize src" ;;
    markupsafe) echo "src/markupsafe src/markupsafe src" ;;
    semver) echo "src/semver src/semver src" ;;
  esac
}

echo "| Project | Pass | Mutants | Both detect | Both leave | Matrix detects, Supercov not | Supercov detects, matrix not | Errors | Weak not-infected or not-reached the matrix detects |"
echo "| --- | --- | --- | --- | --- | --- | --- | --- | --- |"
for project in python-slugify cachetools parse humanize markupsafe semver; do
  dir="$corpus/$project"
  "$here/setup.sh" "$project" "$dir" > /dev/null
  read -r sources _ module_root <<< "$(roots "$project")"
  (cd "$dir/src" && find "$sources" -name '*.py' | sort | xargs "$dir/.venv/bin/python" "$repository/scripts/mutmut-parity.py" --root . --module-root "$module_root") > "$out/ids-$project.jsonl"
  cat "$results"/matrix-"$project"-*/kill-matrix.jsonl > "$out/matrix-$project.jsonl" 2> /dev/null
  for pass in "$results"/supercov-"$project"-*; do
    name=$(basename "$pass")
    [ -f "$pass/strong-mutation.json" ] || { echo "| $project | ${name#supercov-$project-} | no result |  |  |  |  |  |  |"; continue; }
    python3 "$here/compare.py" "$out/ids-$project.jsonl" "$out/matrix-$project.jsonl" "$pass/strong-mutation.json" \
      "$results/mutmut-$project/mutmut-results.json" "$pass/weak.jsonl" > "$out/$name.json"
    python3 - "$out/$name.json" "$project" "${name#supercov-$project-}" <<'PY'
import json, sys
summary, project, label = json.load(open(sys.argv[1])), sys.argv[2], sys.argv[3]
detected = {"killed", "timeout"}
both = leave = 0
for pair, count in summary["pairs"].items():
    theirs, ours = pair.split(" / ")
    if theirs in detected and ours in detected:
        both += count
    elif theirs not in detected and ours not in detected and ours != "error":
        leave += count
missed = sum(1 for v in summary["violationList"] if v["matrix"] in detected)
extra = summary["violations"] - missed
print(f"| {project} | {label} | {summary['matrix']} | {both} | {leave} | {missed} | {extra} | {summary['errors']} | {summary['weakViolations']} |")
PY
  done
done
echo
echo "| Project | mutmut | per mutant | Supercov pass | measured run | strong pass | per mutant |"
echo "| --- | --- | --- | --- | --- | --- | --- |"
for project in python-slugify cachetools parse humanize markupsafe semver; do
  for pass in "$results"/supercov-"$project"-*; do
    python3 - "$results/mutmut-$project/mutmut-timing.json" "$pass/timing.json" "$project" "$(basename "$pass")" <<'PY'
import json, sys
try:
    mutmut = json.load(open(sys.argv[1]))
    shown = f"{mutmut['seconds']:.1f} s | {mutmut['secondsPerTestedMutant'] * 1000:.1f} ms"
except OSError:
    shown = "- | -"
try:
    ours = json.load(open(sys.argv[2]))
    supercov = f"{ours['measureSeconds']:.1f} s | {ours['strongSeconds']:.1f} s | {ours['strongSecondsPerRanMutant'] * 1000:.1f} ms"
except OSError:
    supercov = "- | - | -"
print(f"| {sys.argv[3]} | {shown} | {sys.argv[4].split(sys.argv[3] + '-')[1]} | {supercov} |")
PY
  done
done
echo
echo "| Project | tests alone | measured | measured with values |"
echo "| --- | --- | --- | --- |"
for project in python-slugify cachetools parse humanize markupsafe semver; do
  python3 - "$results/overhead-$project/overhead.json" <<'PY'
import json, sys
try:
    o = json.load(open(sys.argv[1]))
except (OSError, ValueError):
    sys.exit(0)
print(f"| {o['project']} | {o['plain']:.2f} s | {o['measured']:.2f} s ({o['measured'] / o['plain']:.2f}×) | {o['values']:.2f} s ({o['values'] / o['plain']:.2f}×) |")
PY
done
echo
echo "Each pass under another interpreter or launcher against the same project's pass under Python 3.12 and \`python -m pytest\`:"
echo
echo "| Project | Pass | Mutants | Same verdict | Detected by one only | Other differences |"
echo "| --- | --- | --- | --- | --- | --- |"
for project in python-slugify cachetools parse humanize markupsafe semver; do
  base="$results/supercov-$project-pytest/strong-mutation.json"
  [ -f "$base" ] || continue
  for pass in "$results"/supercov-"$project"-pytest-*; do
    [ -d "$pass" ] || continue
    python3 - "$base" "$pass/strong-mutation.json" "$project" "${pass##*/supercov-$project-pytest-}" "$out/variant-$project-${pass##*/supercov-$project-pytest-}.json" <<'PY'
import json, sys
base_path, path, project, label, out = sys.argv[1:]
try:
    ours = json.load(open(path))["mutants"]
except (OSError, ValueError):
    print(f"| {project} | {label} | no result | | | |")
    sys.exit(0)
base = json.load(open(base_path))["mutants"]
detected = {"killed", "timeout"}
same = one = other = 0
differences = []
for mutant in sorted(set(base) | set(ours)):
    a = base.get(mutant, {}).get("status")
    b = ours.get(mutant, {}).get("status")
    if a == b:
        same += 1
        continue
    if (a in detected) != (b in detected) and a is not None and b is not None and "error" not in (a, b):
        one += 1
    else:
        other += 1
    differences.append({"mutant": mutant, "base": a, "variant": b})
json.dump(differences, open(out, "w"), indent=1)
print(f"| {project} | {label} | {len(set(base) | set(ours))} | {same} | {one} | {other} |")
PY
  done
done
