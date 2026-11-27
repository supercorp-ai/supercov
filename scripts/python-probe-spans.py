"""Checks that each value probe the engine plans for a Python file names an
expression CPython parses at exactly that span, of the kind the probe wraps.

    cargo test ... a_directory_s_probes_are_printed | python scripts/python-probe-spans.py

A probe whose span names no such expression would never record, and its
mutant would read as never evaluated.
"""

import ast
import json
import sys

KINDS = {
    "compare": (ast.Compare,),
    "arith": (ast.BinOp,),
    "sign": (ast.UnaryOp,),
    "method": (ast.Attribute,),
}

spans: dict = {}
missing = 0
checked = 0
for line in sys.stdin:
    if not line.startswith("PROBE "):
        continue
    probe = json.loads(line[6:])
    file = probe["site"].split(":")[0]
    if file not in spans:
        tree = ast.parse(open(file, "rb").read())
        spans[file] = {}
        for node in ast.walk(tree):
            if isinstance(node, ast.expr):
                spans[file].setdefault((node.lineno, node.col_offset, node.end_lineno, node.end_col_offset), []).append(node)
    (line_, column), (end_line, end_column) = probe["span"]
    nodes = spans[file].get((line_, column, end_line, end_column), [])
    wanted = KINDS.get(probe["kind"], (ast.expr,))
    checked += 1
    if not any(isinstance(node, wanted) for node in nodes):
        missing += 1
        print("UNPLACED", probe["site"], probe["kind"], [type(node).__name__ for node in nodes])
print(f"{checked} probes, {missing} with no expression at their span")
sys.exit(1 if missing else 0)
