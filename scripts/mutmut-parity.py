"""mutmut's own mutants of Python sources, as Supercov names them.

Prints, for each file, every mutant mutmut 3.8.1 makes of it, in mutmut's
order, with Supercov's id parts (span, mutator, descriptor), mutmut's name
for it (`x_func__mutmut_3`, `xǁClassǁmethod__mutmut_3`) and the edit it makes
to the file: the shortest span of the original whose replacement gives
mutmut's mutated text. The engine's enumeration (`python_mutants.rs`) is
tested against what this writes for the fixtures in
`crates/supercov-engine/tests/fixtures/mutmut-parity`:

    python scripts/mutmut-parity.py --write crates/supercov-engine/tests/fixtures/mutmut-parity/*.py

and joins a corpus's mutmut verdicts to Supercov's mutants by these ids:

    python scripts/mutmut-parity.py --root <project> --module-root <dir> <files...> > ids.jsonl

Needs mutmut 3.8.1 (and libcst) importable. The descriptor grammar is the
engine's (`python_mutants.rs`); keep the two the same.
"""

from __future__ import annotations

import argparse
import json
import os
import sys

import libcst as cst
from libcst.metadata import MetadataWrapper, PositionProvider

MUTANT_SET = "mutmut@3.8.1"


def _require_mutmut():
    from importlib.metadata import version

    found = version("mutmut")
    if found != MUTANT_SET.split("@")[1]:
        sys.exit(f"mutmut {found} is installed; the goldens are of {MUTANT_SET}")


_require_mutmut()

from mutmut.mutation import file_mutation, mutators, pragma_handling  # noqa: E402
from mutmut.mutation.pragma_handling import get_ignored_lines  # noqa: E402
from mutmut.mutation.trampoline_templates import mangle_function_name  # noqa: E402

# Lines a configured `do_not_mutate_patterns` ignores are the only part of
# mutmut's configuration enumeration reads; the goldens are of its default,
# none, and reading it would need a project's configuration.
pragma_handling.get_lines_ignored_by_pattern = lambda source: set()


BINARY = {
    cst.Add: "+", cst.Subtract: "-", cst.Multiply: "*", cst.Divide: "/", cst.FloorDivide: "//",
    cst.Modulo: "%", cst.Power: "**", cst.LeftShift: "<<", cst.RightShift: ">>", cst.BitAnd: "&",
    cst.BitOr: "|", cst.BitXor: "^", cst.MatrixMultiply: "@",
}
UNARY = {cst.Plus: "+", cst.Minus: "-", cst.BitInvert: "~", cst.Not: "not"}
BOOLEAN = {cst.And: "and", cst.Or: "or"}
COMPARISON = {
    cst.LessThan: "<", cst.LessThanEqual: "<=", cst.GreaterThan: ">", cst.GreaterThanEqual: ">=",
    cst.Equal: "==", cst.NotEqual: "!=", cst.Is: "is", cst.IsNot: "is not", cst.In: "in", cst.NotIn: "not in",
}
AUGMENTED = {
    cst.AddAssign: "+=", cst.SubtractAssign: "-=", cst.MultiplyAssign: "*=", cst.DivideAssign: "/=",
    cst.FloorDivideAssign: "//=", cst.ModuloAssign: "%=", cst.PowerAssign: "**=", cst.LeftShiftAssign: "<<=",
    cst.RightShiftAssign: ">>=", cst.BitAndAssign: "&=", cst.BitOrAssign: "|=", cst.BitXorAssign: "^=",
}


def _string_descriptor(original: cst.SimpleString, mutated: cst.SimpleString) -> str:
    value = original.value
    quote_at = min(at for at in (value.find('"'), value.find("'")) if at != -1)
    prefix, body = value[:quote_at], value[quote_at + 1 : -1]
    new_body = mutated.value[quote_at + 1 : -1]
    if new_body == "XX" + body + "XX":
        return "XX"
    lowered = [m(body) for m in _case_mutations(prefix)]
    return "lower" if new_body == lowered[0] else "upper"


def _case_mutations(prefix: str):
    if "r" in prefix.lower():
        import re

        pattern = re.compile(r"((?<!\\)[^\\]+)")
        return [
            lambda x: pattern.sub(lambda m: m.group(1).lower(), x),
            lambda x: pattern.sub(lambda m: m.group(1).upper(), x),
        ]
    pattern = mutators.NON_ESCAPE_SEQUENCE
    return [
        lambda x: pattern.sub(lambda m: m.group(2).lower() if m.group(2) else m.group(0), x),
        lambda x: pattern.sub(lambda m: m.group(2).upper() if m.group(2) else m.group(0), x),
    ]


def describe(operator, original, mutated) -> tuple[str, str]:
    """Supercov's mutator name and descriptor for one of mutmut's mutants."""
    name = operator.__name__
    if name == "operator_number":
        return "NumberLiteral", mutated.value
    if name == "operator_string":
        return "StringLiteral", _string_descriptor(original, mutated)
    if name == "operator_name":
        if original.value in ("True", "False"):
            return "BooleanLiteral", mutated.value
        return "Name", mutated.value
    if name == "operator_assignment":
        return "AssignmentValue", "None" if isinstance(mutated.value, cst.Name) else '""'
    if name == "operator_augmented_assignment":
        return "AugmentedAssignment", "="
    if name == "operator_remove_unary_ops":
        return ("BooleanLiteral" if isinstance(original.operator, cst.Not) else "UnaryOperator"), "removed"
    if name == "operator_dict_arguments":
        for before, after in zip(original.args, mutated.args):
            if before.keyword.value != after.keyword.value:
                return "DictKeyword", after.keyword.value
    if name == "operator_arg_removal":
        if len(mutated.args) == len(original.args):
            at = next(i for i, (a, b) in enumerate(zip(original.args, mutated.args)) if a is not b)
            return "CallArgument", f"None@{at + 1}"
        kept = {id(arg) for arg in mutated.args}
        at = next(i for i, arg in enumerate(original.args) if id(arg) not in kept)
        return "CallArgument", f"removed@{at + 1}"
    if name in ("operator_symmetric_string_methods_swap", "operator_unsymmetrical_string_methods_swap"):
        return "MethodExpression", mutated.func.attr.value
    if name == "operator_lambda":
        return "Lambda", "None" if isinstance(mutated.body, cst.Name) else "0"
    if name == "operator_keywords":
        if isinstance(original, (cst.Break, cst.Continue)):
            return "LoopControl", "return" if isinstance(mutated, cst.Return) else "break"
        return "EqualityOperator", COMPARISON[type(mutated)]
    if name == "operator_swap_op":
        if isinstance(original, cst.BinaryOperation):
            return "ArithmeticOperator", BINARY[type(mutated.operator)]
        if isinstance(original, cst.UnaryOperation):
            return "UnaryOperator", UNARY[type(mutated.operator)]
        if isinstance(original, cst.BooleanOperation):
            return "LogicalOperator", BOOLEAN[type(mutated.operator)]
        if isinstance(original, cst.ComparisonTarget):
            return "EqualityOperator", COMPARISON[type(mutated.operator)]
        return "AssignmentOperator", AUGMENTED[type(mutated.operator)]
    if name == "operator_match":
        kept = {id(case) for case in mutated.cases}
        at = next(i for i, case in enumerate(original.cases) if id(case) not in kept)
        return "MatchCase", f"removed@{at + 1}"
    if name == "operator_if_exp":
        return "IfExpression", "false" if isinstance(mutated.test.operator, cst.And) else "true"
    raise ValueError(f"unknown mutmut operator {name}")


class _Recording(file_mutation.MutationVisitor):
    """mutmut's visitor, remembering which operator made each mutation."""

    def _create_mutations(self, node):
        for kind, operator in self._operators:
            if isinstance(node, kind):
                for mutated in operator(node):
                    if self._removes_ignored_code(node, mutated):
                        continue
                    self.mutations.append(
                        (
                            operator,
                            file_mutation.Mutation(
                                original_node=node,
                                mutated_node=mutated,
                                contained_by_top_level_function=self.get_metadata(
                                    file_mutation.OuterFunctionProvider, node, None
                                ),
                            ),
                        )
                    )


def _units(module: cst.Module) -> dict:
    """The functions mutmut copies, by node: its mangled name for each."""
    units = {}
    for statement in module.body:
        if isinstance(statement, cst.FunctionDef):
            units[statement] = mangle_function_name(name=statement.name.value, class_name=None)
        elif isinstance(statement, cst.ClassDef) and isinstance(statement.body, cst.IndentedBlock):
            for method in statement.body.body:
                if isinstance(method, cst.FunctionDef):
                    units[method] = mangle_function_name(name=method.name.value, class_name=statement.name.value)
    return units


def _offsets(text: str):
    starts = [0]
    for index, character in enumerate(text):
        if character == "\n":
            starts.append(index + 1)
    return starts


def _at(starts, offset):
    """1-based line and 1-based column, in code points, of a code point offset."""
    import bisect

    line = bisect.bisect_right(starts, offset) - 1
    return [line + 1, offset - starts[line] + 1]


def _position(starts, line, column):
    return starts[line - 1] + column


def mutants_of(path: str, relative: str) -> list:
    with open(path, encoding="utf-8") as stream:
        source = stream.read()
    module = cst.parse_module(source)
    wrapper = MetadataWrapper(module)
    ignored = get_ignored_lines(path, source, wrapper, None)
    visitor = _Recording(mutators.mutation_operators, ignored, None)
    tree = wrapper.visit(visitor)
    positions = wrapper.resolve(PositionProvider)
    units = _units(wrapper.module)
    starts = _offsets(source)
    counters: dict = {}
    listed = []
    for operator, mutation in visitor.mutations:
        unit = mutation.contained_by_top_level_function
        if unit not in units:
            continue
        counters[unit] = counters.get(unit, 0) + 1
        mutator, descriptor = describe(operator, mutation.original_node, mutation.mutated_node)
        position = positions[mutation.original_node]
        start = _position(starts, position.start.line, position.start.column)
        end = _position(starts, position.end.line, position.end.column)
        # A comparison's operator owns the whitespace before it.
        while start < end and source[start] in " \t\\\n\r\f":
            start += 1
        mutated = file_mutation.deep_replace(wrapper.module, mutation.original_node, mutation.mutated_node).code
        prefix = 0
        limit = min(len(source), len(mutated))
        while prefix < limit and source[prefix] == mutated[prefix]:
            prefix += 1
        suffix = 0
        while (
            suffix < limit - prefix
            and source[len(source) - 1 - suffix] == mutated[len(mutated) - 1 - suffix]
        ):
            suffix += 1
        listed.append(
            {
                "file": relative,
                "mutator": mutator,
                "start": _at(starts, start),
                "end": _at(starts, end),
                "replacement": descriptor,
                "mutmut": f"{units[unit]}__mutmut_{counters[unit]}",
                "edit": {
                    "start": _at(starts, prefix),
                    "end": _at(starts, len(source) - suffix),
                    "text": mutated[prefix : len(mutated) - suffix],
                },
            }
        )
    del tree
    return listed


def _module_name(relative: str) -> str:
    """mutmut's module prefix of a mutant's name (`pkg.mod` for `pkg/mod.py`)."""
    stem = relative[:-3] if relative.endswith(".py") else relative
    parts = stem.replace(os.sep, "/").split("/")
    if parts[-1] == "__init__":
        parts.pop()
    return ".".join(parts)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--write", action="store_true", help="write <file>.mutmut.json beside each file")
    parser.add_argument("--root", help="ids relative to this directory (a project's root)")
    parser.add_argument("--module-root", help="mutmut names relative to this directory (where packages start)")
    parser.add_argument("files", nargs="+")
    options = parser.parse_args()
    for path in options.files:
        relative = os.path.relpath(path, options.root).replace(os.sep, "/") if options.root else os.path.basename(path)
        mutants = mutants_of(path, relative)
        if options.write:
            with open(f"{path}.mutmut.json", "w", encoding="utf-8") as stream:
                stream.write('{\n"mutantSet": "%s",\n"file": "%s",\n"mutants": [\n' % (MUTANT_SET, relative))
                stream.write(",\n".join("  " + json.dumps(mutant, ensure_ascii=False) for mutant in mutants))
                stream.write("\n]\n}\n")
            continue
        module = _module_name(os.path.relpath(path, options.module_root)) if options.module_root else None
        for mutant in mutants:
            if module is not None:
                mutant["mutmut"] = f"{module}.{mutant['mutmut']}"
            mutant["id"] = "{}:{}:{}-{}:{}:{}:{}".format(
                mutant["file"], *mutant["start"], *mutant["end"], mutant["mutator"], mutant["replacement"]
            )
            print(json.dumps(mutant, ensure_ascii=False))


if __name__ == "__main__":
    main()
