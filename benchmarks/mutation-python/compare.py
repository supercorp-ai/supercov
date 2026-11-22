"""Joins Supercov's strong verdicts to mutmut's full kill matrix.

    python compare.py <ids.jsonl> <kill-matrix.jsonl> <strong-mutation.json> [mutmut-results.json] [weak.jsonl]

`ids.jsonl` is `scripts/mutmut-parity.py --root <project> --module-root
<where packages start> <its sources>`: each mutmut mutant's name and
Supercov's id for it. The kill matrix runs every test against every mutant
(`kill_matrix.py`). A soundness violation is a mutant the matrix detects
(killed, timeout) that Supercov calls undetected (survived, no-coverage,
equivalent), or the reverse. With `weak.jsonl` (`supercov runs latest
mutants --format jsonl`), a mutant the weak report calls not reached or
not infected that the matrix detects is counted too. Prints one JSON
summary.
"""

import json
import os
import sys
from collections import Counter

DETECTED = {"killed", "timeout"}
UNDETECTED = {"survived", "no-coverage", "equivalent", "no tests"}

ids_path, matrix_path, strong_path = sys.argv[1:4]
# A tool's file that is missing (its job did not finish) is left out.
mutmut_path = sys.argv[4] if len(sys.argv) > 4 and os.path.exists(sys.argv[4]) else None
weak_path = sys.argv[5] if len(sys.argv) > 5 and os.path.exists(sys.argv[5]) else None
weak = {}
if weak_path:
    for line in open(weak_path, encoding="utf-8"):
        mutant = json.loads(line)
        # The weak verdict, whatever a strong pass then said.
        facts = mutant.get("weak") or {}
        if facts.get("rule"):
            status = "equivalent"
        elif facts.get("reach") == "not-reached":
            status = "not-reached"
        else:
            status = facts.get("infection") or "unknown"
        weak[mutant["id"]] = status
ids = {}
for line in open(ids_path, encoding="utf-8"):
    record = json.loads(line)
    ids[record["mutmut"]] = record["id"]
matrix = {}
for line in open(matrix_path, encoding="utf-8"):
    record = json.loads(line)
    matrix[record["mutant"]] = record
strong = json.load(open(strong_path, encoding="utf-8"))["mutants"]
mutmut = json.load(open(mutmut_path, encoding="utf-8")) if mutmut_path else {}

pairs = Counter()
violations = []
unmatched = []
errors = []
for name, record in sorted(matrix.items()):
    mutant = ids.get(name)
    verdict = strong.get(mutant) if mutant else None
    if verdict is None:
        unmatched.append(name)
        continue
    ours, theirs = verdict["status"], record["status"]
    pairs[(theirs, ours)] += 1
    if ours == "error":
        errors.append({"mutmut": name, "id": mutant, "reason": verdict.get("reason")})
        continue
    if (theirs in DETECTED and ours in UNDETECTED) or (theirs in UNDETECTED - {"no tests"} and ours in DETECTED):
        violations.append(
            {
                "mutmut": name,
                "id": mutant,
                "matrix": theirs,
                "supercov": ours,
                "killers": record.get("killers", [])[:5],
                "killedBy": verdict.get("killedBy"),
                "reason": verdict.get("reason"),
                "mutmutOwn": mutmut.get(name),
            }
        )
weak_pairs = Counter()
weak_violations = []
for name, record in sorted(matrix.items()):
    mutant = ids.get(name)
    status = weak.get(mutant)
    if status is None:
        continue
    weak_pairs[(record["status"], status)] += 1
    if record["status"] in DETECTED and status in ("not-reached", "not-infected", "equivalent"):
        weak_violations.append({"mutmut": name, "id": mutant, "matrix": record["status"], "weak": status})
summary = {
    "matrix": len(matrix),
    "joined": sum(pairs.values()),
    "unmatched": len(unmatched),
    "pairs": {f"{theirs} / {ours}": count for (theirs, ours), count in sorted(pairs.items())},
    "errors": len(errors),
    "violations": len(violations),
    "violationList": violations,
    "errorList": errors[:20],
    "unmatchedList": unmatched[:20],
    "weakPairs": {f"{theirs} / {ours}": count for (theirs, ours), count in sorted(weak_pairs.items())},
    "weakViolations": len(weak_violations),
    "weakViolationList": weak_violations,
}
print(json.dumps(summary, indent=1))
