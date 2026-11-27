#!/usr/bin/env python3
"""PIT's mutants of a project as Supercov joins them: one JSON line per
mutant with its file, line, mutator, key and status.

    python3 scripts/pit-parity.py target/pit-reports/mutations.xml --root .

Writes the golden Supercov's enumeration tests compare against
(`tests/fixtures/pit-parity/*.json`) with --golden, and otherwise prints the
mutants.
"""
import json
import re
import sys
import xml.etree.ElementTree as ET

MATH = {
    "addition": "+", "subtraction": "-", "multiplication": "*", "division": "/",
    "modulus": "%", "bitwise AND": "&", "bitwise OR": "|", "XOR": "^",
    "Shift Left": "<<", "Shift Right": ">>", "Unsigned Shift Right": ">>>",
    "AND": "&", "OR": "|",
}


def key_of(mutator, description):
    if mutator == "Math":
        m = re.match(r"Replaced (?:integer |long |float |double )?(.+?) with (.+)$", description)
        if m:
            return MATH.get(m.group(1), m.group(1)) + ">" + MATH.get(m.group(2), m.group(2))
    if mutator == "VoidMethodCall":
        return description.rsplit("::", 1)[-1]
    if mutator.endswith("ReturnVals") or mutator == "PrimitiveReturns":
        return re.sub(r"\d+$", "", description.rsplit("::", 1)[-1])
    if mutator == "Increments":
        m = re.match(r"Changed increment from (-?\d+) to", description)
        if m:
            return m.group(1)
    return ""


def mutants(path):
    root = ET.parse(path).getroot()
    for m in root:
        get = lambda tag: (m.find(tag).text if m.find(tag) is not None else "") or ""
        mutator = get("mutator").rsplit(".", 1)[-1].removesuffix("Mutator")
        package = get("mutatedClass").rsplit(".", 1)[0] if "." in get("mutatedClass") else ""
        file = (package.replace(".", "/") + "/" if package else "") + get("sourceFile")
        killing = [t for t in get("killingTests").split("|") if t]
        succeeding = [t for t in get("succeedingTests").split("|") if t]
        yield {
            "file": file,
            "line": int(get("lineNumber")),
            "mutator": mutator,
            "key": key_of(mutator, get("description")),
            "status": m.get("status"),
            "class": get("mutatedClass"),
            "method": get("mutatedMethod"),
            "description": get("description"),
            "killingTests": killing,
            "succeedingTests": succeeding,
        }


if __name__ == "__main__":
    report = sys.argv[1]
    rows = sorted(mutants(report), key=lambda r: (r["file"], r["line"], r["mutator"], r["key"]))
    if "--golden" in sys.argv:
        out = sys.argv[sys.argv.index("--golden") + 1]
        keep = [{k: r[k] for k in ("file", "line", "mutator", "key")} for r in rows]
        with open(out, "w") as f:
            json.dump(keep, f, indent=1)
            f.write("\n")
    else:
        for r in rows:
            print(json.dumps(r))
