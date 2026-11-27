#!/usr/bin/env python3
"""The JVM mutation benchmark's results, one table: each project's (and
shard's) join with PIT and the times.

    python3 benchmarks/mutation-jvm/summarize.py <artifacts-dir>

<artifacts-dir> holds the run's artifacts as `actions/download-artifact`
merges them (.github/workflows/mutation-jvm.yml): <project>/<shard>/ for
each job's files.
"""
import collections
import json
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
rows = collections.defaultdict(lambda: collections.Counter())
times = collections.defaultdict(lambda: collections.Counter())
categories = collections.defaultdict(collections.Counter)
violations = []

for path in root.rglob("*.json"):
    name = path.name
    try:
        data = json.loads(path.read_text())
    except (ValueError, UnicodeDecodeError):
        continue
    # <project>/<shard>/<file>: a project's shards are summed.
    project = path.relative_to(root).parts[0]
    if name in ("pit.json", "pit-matrix.json"):
        times[project][data["scenario"]] += data["ms"]
    elif name == "supercov.json":
        times[project]["measure"] += data["measureMs"]
        times[project]["strong"] += data["strongMs"]
    elif name == "supercov-pit.json":
        if "pit" not in data:
            rows[project]["join failed"] += 1
            continue
        row = rows[project]
        row["pit"] += data["pit"]
        row["supercov"] += data["supercov"]
        row["matched"] += data["matched"]
        row["agree"] += data["agree"]
        row["violations"] += data["soundnessViolations"]["total"]
        row["only PIT"] += data["onlyPit"]["total"]
        row["only Supercov"] += data["onlySupercov"]["total"]
        for pair, count in data.get("notCompared", {}).items():
            row["not compared"] += count
        for category, mutators in data["onlyPit"]["byCategory"].items():
            categories[project][category] += sum(mutators.values())
        violations.extend((project, v) for v in data["soundnessViolations"]["mutants"])
    elif name == "supercov-strong.json":
        summary = data.get("summary", {})
        for key in ("killed", "timeout", "survived", "noCoverage", "error", "ran"):
            rows[project]["strong " + key] += summary.get(key, 0) or 0

columns = ["pit", "supercov", "matched", "agree", "violations", "only PIT", "only Supercov",
           "not compared", "strong ran", "strong killed", "strong survived", "strong timeout",
           "strong error", "join failed"]
print("| project | " + " | ".join(columns) + " | PIT (s) | PIT matrix (s) | measured run (s) | strong pass (s) |")
print("|" + " --- |" * (len(columns) + 5))
for project in sorted(set(rows) | set(times)):
    row = rows[project]
    t = times[project]
    print(f"| {project} | " + " | ".join(str(row[c]) for c in columns)
          + f" | {t['pit'] / 1000:.0f} | {t['pit-matrix'] / 1000:.0f} | {t['measure'] / 1000:.0f} | {t['strong'] / 1000:.0f} |")
print()
for project, counts in sorted(categories.items()):
    print(project, dict(counts))
print()
for project, violation in violations:
    print("VIOLATION", project, violation["id"], violation["status"], violation["pit"],
          violation.get("reason"), (violation.get("pitKillingTests") or [])[:2])
