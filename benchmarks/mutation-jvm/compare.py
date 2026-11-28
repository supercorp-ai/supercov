#!/usr/bin/env python3
"""Two JVM benchmark runs' strong verdicts, mutant by mutant: what a
performance change must leave as it was.

    python3 benchmarks/mutation-jvm/compare.py <old-artifacts> <new-artifacts>

Each directory is a run's `agreement` artifact (<project>/<shard>/
strong-mutation.json). Prints, per project, how many mutants each pair of
statuses holds and every mutant whose verdict changed; exits 1 when one did.
A `timeout` against `killed` is listed apart: both detect the mutant, and
which one a mutant gets depends on the machine's speed.
"""
import collections
import json
import pathlib
import sys


def verdicts(root):
    found = collections.defaultdict(dict)
    for path in pathlib.Path(root).rglob("strong-mutation.json"):
        project = path.relative_to(root).parts[0]
        data = json.loads(path.read_text())
        for mutant, verdict in data.get("mutants", {}).items():
            found[project][mutant] = verdict.get("status")
    return found


old, new = verdicts(sys.argv[1]), verdicts(sys.argv[2])
changed = 0
for project in sorted(set(old) | set(new)):
    pairs = collections.Counter()
    differences = []
    for mutant in sorted(set(old[project]) | set(new[project])):
        a, b = old[project].get(mutant), new[project].get(mutant)
        pairs[(a, b)] += 1
        if a != b:
            differences.append((mutant, a, b))
    detected = {"killed", "timeout"}
    speed = [d for d in differences if d[1] in detected and d[2] in detected]
    real = [d for d in differences if d not in speed]
    print(f"{project}: {sum(pairs.values())} mutants, {len(real)} changed, {len(speed)} killed/timeout swaps")
    for (a, b), count in sorted(pairs.items(), key=lambda item: -item[1]):
        print(f"  {a} -> {b}: {count}")
    for mutant, a, b in real:
        print(f"  CHANGED {mutant}: {a} -> {b}")
    for mutant, a, b in speed:
        print(f"  speed {mutant}: {a} -> {b}")
    changed += len(real)
sys.exit(1 if changed else 0)
