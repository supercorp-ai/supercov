//! Mutant schemata for Java and Kotlin (`docs/design/strong-mutation.md`):
//! every listed mutant of a file written into its strong copy behind a
//! switch, so that one build holds them all and the runtime turns one on at a
//! time.
//!
//! A switch reads `Supercov.on(file, index)`: `file` is the file's slot in
//! the pass, `index` the mutant's place in the file's list. Switches nest as
//! the code does: a switch's kept parts are written with the switches inside
//! them, so the copy of `a + b < c` holds the `+` switch inside the `<` one.
//!
//! Java's switches are calls of typed helpers (`Supercov.lt(f, k, a, b)`)
//! wherever an operator is replaced, so each operand is evaluated once and
//! overload resolution keeps javac's own numeric promotion; a statement form
//! goes through `Supercov.v(...)`, a call, which Java accepts wherever a
//! statement expression is required. Kotlin has no implicit widening to
//! match overloads with, so an operator's switch binds its operands with
//! `let` and applies the operator itself, and a negated condition goes
//! through a function whose contract keeps the smart casts the condition
//! made. A switch that javac or kotlinc refuses (a `+` of strings, a smart
//! cast a negation cannot keep) is reported by the build at a position
//! inside its region; the pass drops it and the mutant is `unviable`.

use crate::jvm_instrumenter::JvmLanguage;
use crate::jvm_mutants::{Form, JvmMutant, Prim, Range, ReturnValue};

/// The runtime class the switches call.
pub const RUNTIME: &str = "com.supercorp.supercov.Supercov";
/// Kotlin's helpers with contracts, top-level functions of the runtime's
/// package.
pub const KOTLIN_NEGATE: &str = "com.supercorp.supercov.supercovNegate";

/// A file with its switches, and where each switch landed in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Switched {
    pub source: String,
    /// Each placed mutant's index and the byte range its switch took in
    /// `source`.
    pub regions: Vec<(usize, Range)>,
}

/// Writes the switches of `mutants` whose index is in `placed` into
/// `source`. `slot` is the file's number in the pass.
pub fn switch_source(
    source: &str,
    language: JvmLanguage,
    slot: usize,
    mutants: &[JvmMutant],
    placed: &[usize],
) -> Switched {
    let mut sites = placed
        .iter()
        .copied()
        .filter(|index| *index < mutants.len())
        .collect::<Vec<_>>();
    sites.sort_by_key(|index| {
        let placement = &mutants[*index].placement;
        (
            placement.span.0,
            std::cmp::Reverse(placement.span.1),
            placement.layer,
            *index,
        )
    });
    // Each site's children: the sites directly inside it.
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); sites.len()];
    let mut roots = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    for (position, index) in sites.iter().enumerate() {
        let span = mutants[*index].placement.span;
        while let Some(top) = stack.last() {
            // Sites are sorted by start, so an open site starts at or
            // before this one: it holds this one when it ends after it.
            let outer = mutants[sites[*top]].placement.span;
            if span.1 <= outer.1 {
                break;
            }
            stack.pop();
        }
        match stack.last() {
            Some(parent) => children[*parent].push(position),
            None => roots.push(position),
        }
        stack.push(position);
    }
    let writer = Writer {
        source,
        language,
        slot,
        mutants,
        sites: &sites,
        children: &children,
    };
    let (source, regions) = writer.range((0, source.len()), &roots);
    Switched { source, regions }
}

/// Text with the regions its switches took, relative to its start.
type Written = (String, Vec<(usize, Range)>);

struct Writer<'a> {
    source: &'a str,
    language: JvmLanguage,
    slot: usize,
    mutants: &'a [JvmMutant],
    sites: &'a [usize],
    children: &'a [Vec<usize>],
}

impl Writer<'_> {
    /// `range` of the source with the sites `within` (positions in `sites`,
    /// in order) switched.
    fn range(&self, range: Range, within: &[usize]) -> Written {
        let mut out = String::new();
        let mut regions = Vec::new();
        let mut at = range.0;
        for position in within {
            let span = self.mutants[self.sites[*position]].placement.span;
            if span.0 < at || span.1 > range.1 {
                continue;
            }
            out.push_str(&self.source[at..span.0]);
            let (text, inner) = self.site(*position);
            let base = out.len();
            regions.extend(
                inner
                    .into_iter()
                    .map(|(index, (s, e))| (index, (s + base, e + base))),
            );
            out.push_str(&text);
            at = span.1;
        }
        out.push_str(&self.source[at..range.1]);
        (out, regions)
    }

    fn site(&self, position: usize) -> Written {
        let index = self.sites[position];
        let placement = &self.mutants[index].placement;
        let holes = placement
            .holes
            .iter()
            .map(|hole| {
                let inside = self.children[position]
                    .iter()
                    .copied()
                    .filter(|child| {
                        let span = self.mutants[self.sites[*child]].placement.span;
                        hole.0 <= span.0 && span.1 <= hole.1
                    })
                    .collect::<Vec<_>>();
                self.range(*hole, &inside)
            })
            .collect::<Vec<_>>();
        let template = self.template(index, &placement.form);
        let mut out = String::new();
        let mut regions = Vec::new();
        let mut placed = vec![false; holes.len()];
        // Every form's template holds a hole: the text before the first,
        // then each hole's number and the text after it.
        let mut pieces = template.split('\u{1}');
        out.push_str(pieces.next().expect("a split has a first piece"));
        pieces.for_each(|piece| {
            let number = piece
                .chars()
                .next()
                .and_then(|c| c.to_digit(10))
                .unwrap_or(0) as usize;
            if let Some((text, inner)) = holes.get(number) {
                // A hole written twice keeps its switches' regions at its
                // first copy.
                if !placed[number] {
                    let base = out.len();
                    regions.extend(
                        inner
                            .iter()
                            .map(|(index, (s, e))| (*index, (s + base, e + base))),
                    );
                    placed[number] = true;
                }
                out.push_str(text);
            }
            out.push_str(&piece[1..]);
        });
        regions.push((index, (0, out.len())));
        (out, regions)
    }

    /// The switch's text, with `\u{1}N` where hole N goes.
    fn template(&self, index: usize, form: &Form) -> String {
        let mutant = &self.mutants[index];
        let f = self.slot;
        let k = index;
        let on = format!("{RUNTIME}.on({f}, {k})");
        let h = |n: usize| format!("\u{1}{n}");
        let id = format!("{f}_{k}");
        match self.language {
            JvmLanguage::Java => match form {
                Form::Negate => format!("{RUNTIME}.n({f}, {k}, {})", h(0)),
                Form::Compare(helper) | Form::Arith(helper) => {
                    format!("{RUNTIME}.{helper}({f}, {k}, {}, {})", h(0), h(1))
                }
                Form::Compound {
                    from,
                    to,
                    statement,
                } => {
                    let choice = format!(
                        "({on} ? ({} {to} {}) : ({} {from} {}))",
                        h(0),
                        h(1),
                        h(0),
                        h(1)
                    );
                    statement_or_value(&choice, *statement)
                }
                Form::Step {
                    from,
                    to,
                    prefix,
                    statement,
                } => {
                    let choice = if *prefix {
                        format!("({on} ? {to}{} : {from}{})", h(0), h(0))
                    } else {
                        format!("({on} ? {}{to} : {}{from})", h(0), h(0))
                    };
                    statement_or_value(&choice, *statement)
                }
                Form::StepBy { operator } => {
                    let to = if *operator == "+=" { "-=" } else { "+=" };
                    format!(
                        "{RUNTIME}.v({on} ? ({} {to} {}) : ({} {operator} {}))",
                        h(0),
                        h(1),
                        h(0),
                        h(1)
                    )
                }
                Form::Invert => format!("{RUNTIME}.inv({f}, {k}, {})", h(0)),
                Form::RemoveCall { lambda: false } => format!("{{ if (!{on}) {} }}", h(0)),
                Form::RemoveCall { lambda: true } => format!("{{ if (!{on}) {}; }}", h(0)),
                Form::Return(value, witness) => {
                    java_return(*value, f, k, &h(0), witness.as_deref())
                }
            },
            JvmLanguage::Kotlin => match form {
                Form::Negate => format!("{KOTLIN_NEGATE}({f}, {k}, {})", h(0)),
                Form::Compare(helper) | Form::Arith(helper) => {
                    let (from, to) = kotlin_operators(helper, mutant);
                    format!(
                        "({}).let {{ supercovL{id} -> ({}).let {{ supercovR{id} -> if ({on}) supercovL{id} {to} supercovR{id} else supercovL{id} {from} supercovR{id} }} }}",
                        h(0),
                        h(1)
                    )
                }
                Form::Compound { from, to, .. } => format!(
                    "if ({on}) {} {to} {} else {} {from} {}",
                    h(0),
                    h(1),
                    h(0),
                    h(1)
                ),
                Form::StepBy { operator } => {
                    let to = if *operator == "+=" { "-=" } else { "+=" };
                    format!(
                        "if ({on}) {} {to} {} else {} {operator} {}",
                        h(0),
                        h(1),
                        h(0),
                        h(1)
                    )
                }
                Form::Step {
                    from,
                    to,
                    prefix,
                    statement,
                } => {
                    let choice = if *prefix {
                        format!("if ({on}) {to}{} else {from}{}", h(0), h(0))
                    } else {
                        format!("if ({on}) {}{to} else {}{from}", h(0), h(0))
                    };
                    if *statement {
                        choice
                    } else {
                        format!("({choice})")
                    }
                }
                Form::Invert => format!(
                    "({}).let {{ supercovV{id} -> if ({on}) supercovV{id} else -supercovV{id} }}",
                    h(0)
                ),
                Form::RemoveCall { .. } => format!("if (!{on}) {} else Unit", h(0)),
                Form::Return(value, witness) => {
                    kotlin_return(*value, f, k, &h(0), witness.as_deref())
                }
            },
        }
    }
}

/// A Java choice as a statement (a call, which a statement expression may
/// be anywhere) or as a value.
fn statement_or_value(choice: &str, statement: bool) -> String {
    if statement {
        format!("{RUNTIME}.v{choice}")
    } else {
        choice.to_owned()
    }
}

fn java_return(
    value: ReturnValue,
    f: usize,
    k: usize,
    hole: &str,
    witness: Option<&str>,
) -> String {
    let generic = |helper: &str| match witness {
        Some(witness) => format!("{RUNTIME}.<{witness}>{helper}({f}, {k}, {hole})"),
        None => format!("{RUNTIME}.{helper}({f}, {k}, {hole})"),
    };
    let call = |helper: &str| format!("{RUNTIME}.{helper}({f}, {k}, {hole})");
    match value {
        ReturnValue::True => call("rT"),
        ReturnValue::False => call("rF"),
        ReturnValue::BoxedTrue => generic("rBT"),
        ReturnValue::BoxedFalse => generic("rBF"),
        ReturnValue::Null => generic("rN"),
        ReturnValue::Zero(prim @ (Prim::Byte | Prim::Short | Prim::Char)) => {
            format!("(({}) {})", prim.java(), call("rZ"))
        }
        ReturnValue::Zero(_) => call("rZ"),
        ReturnValue::BoxedZero(prim) => generic(boxed_zero_helper(prim)),
        ReturnValue::EmptyString => call("rS"),
        ReturnValue::EmptyList => generic("rList"),
        ReturnValue::EmptySet => generic("rSet"),
        ReturnValue::EmptyMap => generic("rMap"),
        ReturnValue::EmptyOptional => generic("rOpt"),
        ReturnValue::EmptyStream => generic("rStream"),
    }
}

fn kotlin_return(
    value: ReturnValue,
    f: usize,
    k: usize,
    hole: &str,
    witness: Option<&str>,
) -> String {
    // The same Java helpers: Kotlin calls them with its own types, which
    // are the JVM's, and has no literal to cast back from `int`. Its type
    // argument follows the name.
    match (value, witness) {
        // A Kotlin `Char`, `Byte` or `Short` is exactly the JVM's, so the
        // helper's overload of that type applies without Java's cast.
        (ReturnValue::Zero(Prim::Byte | Prim::Short | Prim::Char), _) => {
            format!("{RUNTIME}.rZ({f}, {k}, {hole})")
        }
        (
            ReturnValue::Zero(_)
            | ReturnValue::EmptyString
            | ReturnValue::True
            | ReturnValue::False,
            _,
        )
        | (_, None) => java_return(value, f, k, hole, None),
        (other, Some(witness)) => {
            let plain = java_return(other, f, k, hole, None);
            let (name, rest) = plain.split_once('(').expect("a call");
            format!("{name}<{witness}>({rest}")
        }
    }
}

fn boxed_zero_helper(prim: Prim) -> &'static str {
    match prim {
        Prim::Long => "rBL",
        Prim::Float => "rBF0",
        Prim::Double => "rBD",
        Prim::Short => "rBS",
        Prim::Char => "rBC",
        _ => "rBI",
    }
}

/// Kotlin's operator and its replacement for a helper's name, spelled as the
/// source spells them (`and`, `shl` are infix calls).
fn kotlin_operators(helper: &str, mutant: &JvmMutant) -> (String, String) {
    let infix = mutant
        .operator
        .as_ref()
        .is_some_and(|(operator, _)| operator.chars().all(char::is_alphabetic));
    let (from, to) = match helper {
        "lt" => ("<", "<="),
        "le" => ("<=", "<"),
        "gt" => (">", ">="),
        "ge" => (">=", ">"),
        "add" => ("+", "-"),
        "sub" => ("-", "+"),
        "mul" => ("*", "/"),
        "div" => ("/", "*"),
        "rem" => ("%", "*"),
        "and" => ("and", "or"),
        "or" => ("or", "and"),
        "xor" => ("xor", "and"),
        "shl" => ("shl", "shr"),
        "shr" => ("shr", "shl"),
        "ushr" => ("ushr", "shl"),
        _ => ("", ""),
    };
    let _ = infix;
    (from.to_owned(), to.to_owned())
}

/// The mutant whose switch a compiler error at `offset` of the switched
/// source is in: the innermost region holding it.
pub fn mutant_at(switched: &Switched, offset: usize) -> Option<usize> {
    switched
        .regions
        .iter()
        .filter(|(_, (start, end))| *start <= offset && offset < (*end).max(*start + 1))
        .min_by_key(|(_, (start, end))| end - start)
        .map(|(index, _)| *index)
}

/// The byte offset of a 1-based line and column (in characters) of `text`.
pub fn offset_of(text: &str, line: usize, column: usize) -> Option<usize> {
    let mut start = 0;
    for _ in 1..line {
        start += text[start..].find('\n')? + 1;
    }
    let line_text = &text[start
        ..text[start..]
            .find('\n')
            .map_or(text.len(), |end| start + end)];
    let within = line_text
        .char_indices()
        .nth(column.saturating_sub(1))
        .map_or(line_text.len(), |(at, _)| at);
    Some(start + within)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jvm_mutants::{ProjectIndex, enumerate};

    fn switched(source: &str, language: JvmLanguage) -> (Switched, Vec<JvmMutant>) {
        let tree = crate::jvm_instrumenter::parse(source, language).unwrap();
        let path = match language {
            JvmLanguage::Java => "A.java",
            JvmLanguage::Kotlin => "A.kt",
        };
        let index = ProjectIndex::of([(path, source)]);
        let mutants = enumerate(source, &tree, language, &index);
        let all = (0..mutants.len()).collect::<Vec<_>>();
        (switch_source(source, language, 3, &mutants, &all), mutants)
    }

    #[test]
    fn java_switches_nest_as_the_code_does() {
        let source = "class A {\n  boolean f(int a, int b) { return a + 1 < b; }\n}\n";
        let (out, mutants) = switched(source, JvmLanguage::Java);
        let names = mutants.iter().map(|m| m.mutator.name()).collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "BooleanTrueReturnVals",
                "NegateConditionals",
                "ConditionalsBoundary",
                "Math"
            ]
        );
        let s = RUNTIME;
        assert_eq!(
            out.source,
            format!(
                "class A {{\n  boolean f(int a, int b) {{ return {s}.rT(3, 0, {s}.n(3, 1, {s}.lt(3, 2, {s}.add(3, 3, a, 1), b))); }}\n}}\n"
            )
        );
        // Each switch's region holds the switches inside it, and an offset
        // finds the innermost.
        let add = out.source.find(".add(").unwrap();
        assert_eq!(mutant_at(&out, add), Some(3));
        let lt = out.source.find(".lt(").unwrap();
        assert_eq!(mutant_at(&out, lt), Some(2));
        assert_eq!(mutant_at(&out, 0), None);
    }

    #[test]
    fn java_statement_forms_are_statement_expressions() {
        let source = "class A {\n  int c;\n  void f(int i) {\n    for (; i < 3; i++) { c++; log(); }\n  }\n  void log() {}\n}\n";
        let (out, _) = switched(source, JvmLanguage::Java);
        assert!(
            out.source
                .contains(&format!("{RUNTIME}.v({RUNTIME}.on(3, ")),
            "{}",
            out.source
        );
        assert!(out.source.contains("? c-- : c++)"), "{}", out.source);
        assert!(
            out.source.contains(&format!("{{ if (!{RUNTIME}.on(3, ")),
            "{}",
            out.source
        );
    }

    #[test]
    fn kotlin_switches_bind_operands_and_keep_smart_casts() {
        let source = "fun f(s: String?, a: Int): Int {\n    if (s != null) return s.length + a\n    return -a\n}\n";
        let (out, _) = switched(source, JvmLanguage::Kotlin);
        assert!(
            out.source.contains(&format!("{KOTLIN_NEGATE}(3, ")),
            "{}",
            out.source
        );
        assert!(out.source.contains(".let { supercovL3_"), "{}", out.source);
        assert!(out.source.contains("else -supercovV3_"), "{}", out.source);
        assert!(
            out.source.contains(&format!("{RUNTIME}.rZ(3, ")),
            "{}",
            out.source
        );
    }

    #[test]
    fn a_kotlin_char_return_has_no_java_cast() {
        let source = "fun f(s: String, i: Int): Char {\n    return s[i]\n}\n";
        let (out, _) = switched(source, JvmLanguage::Kotlin);
        assert!(
            out.source
                .contains(&format!("return {RUNTIME}.rZ(3, 0, s[i])")),
            "{}",
            out.source
        );
    }

    /// Each operator form's switch, as a statement and as a value: Java's
    /// compound assignment and `++`, prefix and postfix, and Kotlin's.
    #[test]
    fn steps_and_compound_assignments_switch_as_statements_and_values() {
        let java = "class A {\n  int c;\n  int f(int i, long n) {\n    c += i;\n    int d = (c -= 2);\n    ++c;\n    int e = c++ + --c;\n    for (; i < 3; i += 2) {}\n    return d + e;\n  }\n}\n";
        let (out, _) = switched(java, JvmLanguage::Java);
        let on = format!("{RUNTIME}.on(3, ");
        for expected in [
            "? (c -= i) : (c += i));",
            "? (c += 2) : (c -= 2)))",
            "? --c : ++c);",
            "? c-- : c++)",
            "? ++c : --c)",
            &format!("{RUNTIME}.v({on}"),
        ] {
            assert!(out.source.contains(expected), "{expected}\n{}", out.source);
        }
        let kotlin = "fun f(a: Int): Int {\n    var c = a\n    c += 2\n    ++c\n    val d = c++\n    return d\n}\n";
        let (out, _) = switched(kotlin, JvmLanguage::Kotlin);
        for expected in [") c -= 2 else c += 2", ") --c else ++c", "(if ("] {
            assert!(out.source.contains(expected), "{expected}\n{}", out.source);
        }
    }

    #[test]
    fn a_compiler_position_is_an_offset() {
        let text = "ab\ncdé\nf";
        assert_eq!(offset_of(text, 1, 1), Some(0));
        assert_eq!(offset_of(text, 2, 3), Some(5));
        assert_eq!(offset_of(text, 2, 9), Some(7));
        assert_eq!(offset_of(text, 3, 1), Some(8));
        assert_eq!(offset_of(text, 5, 1), None);
    }

    // Coverage of the schemata's forms, their nesting and their helpers.

    /// A mutant of `source` whose switch takes `span`, keeps `holes` and has
    /// `form`: the enumeration's own mutant with its placement replaced.
    fn crafted(span: Range, holes: Vec<Range>, form: Form) -> JvmMutant {
        let source = "class A {\n  int f(int a) { return a; }\n}\n";
        let tree = crate::jvm_instrumenter::parse(source, JvmLanguage::Java).unwrap();
        let index = ProjectIndex::of([("A.java", source)]);
        let mut mutant = enumerate(source, &tree, JvmLanguage::Java, &index)
            .into_iter()
            .next()
            .unwrap();
        mutant.placement = crate::jvm_mutants::Placement {
            span,
            holes,
            form,
            layer: 0,
        };
        mutant.operator = None;
        mutant
    }

    /// `source` with the one mutant's switch, in slot 3.
    fn one(source: &str, language: JvmLanguage, mutant: JvmMutant) -> String {
        switch_source(source, language, 3, &[mutant], &[0]).source
    }

    /// A file with no mutant placed is written unchanged.
    #[test]
    fn nothing_placed_leaves_the_source_as_it_is() {
        let source = "class A {}\n";
        let out = switch_source(source, JvmLanguage::Java, 3, &[], &[]);
        assert_eq!(out.source, source);
        assert!(out.regions.is_empty());
    }

    /// Java's `i += n` step written both ways, its `-x` inverted, and a
    /// lambda body's removed call: each switch as the runtime calls it.
    #[test]
    fn java_step_by_invert_and_lambda_call_switches() {
        let s = RUNTIME;
        let on = format!("{s}.on(3, 0)");
        let plus = crafted(
            (0, 6),
            vec![(0, 1), (5, 6)],
            Form::StepBy { operator: "+=" },
        );
        assert_eq!(
            one("i += 2", JvmLanguage::Java, plus),
            format!("{s}.v({on} ? (i -= 2) : (i += 2))")
        );
        let minus = crafted(
            (0, 6),
            vec![(0, 1), (5, 6)],
            Form::StepBy { operator: "-=" },
        );
        assert_eq!(
            one("i -= 2", JvmLanguage::Java, minus),
            format!("{s}.v({on} ? (i += 2) : (i -= 2))")
        );
        let invert = crafted((0, 2), vec![(1, 2)], Form::Invert);
        assert_eq!(
            one("-a", JvmLanguage::Java, invert),
            format!("{s}.inv(3, 0, a)")
        );
        let lambda = crafted((0, 5), vec![(0, 5)], Form::RemoveCall { lambda: true });
        assert_eq!(
            one("log()", JvmLanguage::Java, lambda),
            format!("{{ if (!{on}) log(); }}")
        );
    }

    /// Kotlin's compound assignment, its `-=` step and a removed call: an
    /// `if` on the switch, with `Unit` where the call is gone.
    #[test]
    fn kotlin_compound_step_by_and_removed_call_switches() {
        let on = format!("{RUNTIME}.on(3, 0)");
        let compound = crafted(
            (0, 6),
            vec![(0, 1), (5, 6)],
            Form::Compound {
                from: "*=",
                to: "/=",
                statement: true,
            },
        );
        assert_eq!(
            one("c *= 2", JvmLanguage::Kotlin, compound),
            format!("if ({on}) c /= 2 else c *= 2")
        );
        let minus = crafted(
            (0, 6),
            vec![(0, 1), (5, 6)],
            Form::StepBy { operator: "-=" },
        );
        assert_eq!(
            one("i -= 2", JvmLanguage::Kotlin, minus),
            format!("if ({on}) i += 2 else i -= 2")
        );
        let removed = crafted((0, 5), vec![(0, 5)], Form::RemoveCall { lambda: false });
        assert_eq!(
            one("log()", JvmLanguage::Kotlin, removed),
            format!("if (!{on}) log() else Unit")
        );
    }

    /// A Kotlin `%` binds its operands and multiplies them when on, as
    /// PIT's math mutator does.
    #[test]
    fn a_kotlin_remainder_switches_to_a_product() {
        let rem = crafted((0, 5), vec![(0, 1), (4, 5)], Form::Arith("rem"));
        assert_eq!(
            one("a % b", JvmLanguage::Kotlin, rem),
            format!(
                "(a).let {{ supercovL3_0 -> (b).let {{ supercovR3_0 -> if ({RUNTIME}.on(3, 0)) supercovL3_0 * supercovR3_0 else supercovL3_0 % supercovR3_0 }} }}"
            )
        );
    }

    /// Every helper's Kotlin operator and the one PIT puts in its place;
    /// a name with no operator writes none.
    #[test]
    fn kotlin_operators_follow_pits_replacements() {
        let mutant = crafted((0, 1), vec![], Form::Negate);
        for (helper, from, to) in [
            ("lt", "<", "<="),
            ("le", "<=", "<"),
            ("gt", ">", ">="),
            ("ge", ">=", ">"),
            ("add", "+", "-"),
            ("sub", "-", "+"),
            ("mul", "*", "/"),
            ("div", "/", "*"),
            ("rem", "%", "*"),
            ("and", "and", "or"),
            ("or", "or", "and"),
            ("xor", "xor", "and"),
            ("shl", "shl", "shr"),
            ("shr", "shr", "shl"),
            ("ushr", "ushr", "shl"),
            ("eq", "", ""),
        ] {
            assert_eq!(
                kotlin_operators(helper, &mutant),
                (from.to_owned(), to.to_owned()),
                "{helper}"
            );
        }
    }

    /// Each Java return replacement's helper: a generic one takes the
    /// declared return type as its type argument, a primitive zero of a
    /// narrow type is cast back, and a boxed zero has its type's helper.
    #[test]
    fn java_returns_call_each_value_s_helper() {
        let s = RUNTIME;
        for (value, plain, witnessed) in [
            (ReturnValue::True, "rT(3, 0, v)", "rT(3, 0, v)"),
            (ReturnValue::False, "rF(3, 0, v)", "rF(3, 0, v)"),
            (ReturnValue::BoxedTrue, "rBT(3, 0, v)", "<T>rBT(3, 0, v)"),
            (ReturnValue::BoxedFalse, "rBF(3, 0, v)", "<T>rBF(3, 0, v)"),
            (ReturnValue::Null, "rN(3, 0, v)", "<T>rN(3, 0, v)"),
            (ReturnValue::Zero(Prim::Int), "rZ(3, 0, v)", "rZ(3, 0, v)"),
            (ReturnValue::EmptyString, "rS(3, 0, v)", "rS(3, 0, v)"),
            (
                ReturnValue::EmptyList,
                "rList(3, 0, v)",
                "<T>rList(3, 0, v)",
            ),
            (ReturnValue::EmptySet, "rSet(3, 0, v)", "<T>rSet(3, 0, v)"),
            (ReturnValue::EmptyMap, "rMap(3, 0, v)", "<T>rMap(3, 0, v)"),
            (
                ReturnValue::EmptyOptional,
                "rOpt(3, 0, v)",
                "<T>rOpt(3, 0, v)",
            ),
            (
                ReturnValue::EmptyStream,
                "rStream(3, 0, v)",
                "<T>rStream(3, 0, v)",
            ),
            (
                ReturnValue::BoxedZero(Prim::Int),
                "rBI(3, 0, v)",
                "<T>rBI(3, 0, v)",
            ),
            (
                ReturnValue::BoxedZero(Prim::Long),
                "rBL(3, 0, v)",
                "<T>rBL(3, 0, v)",
            ),
            (
                ReturnValue::BoxedZero(Prim::Float),
                "rBF0(3, 0, v)",
                "<T>rBF0(3, 0, v)",
            ),
            (
                ReturnValue::BoxedZero(Prim::Double),
                "rBD(3, 0, v)",
                "<T>rBD(3, 0, v)",
            ),
            (
                ReturnValue::BoxedZero(Prim::Short),
                "rBS(3, 0, v)",
                "<T>rBS(3, 0, v)",
            ),
            (
                ReturnValue::BoxedZero(Prim::Char),
                "rBC(3, 0, v)",
                "<T>rBC(3, 0, v)",
            ),
        ] {
            assert_eq!(java_return(value, 3, 0, "v", None), format!("{s}.{plain}"));
            assert_eq!(
                java_return(value, 3, 0, "v", Some("T")),
                format!("{s}.{witnessed}"),
                "{value:?}"
            );
        }
        assert_eq!(
            java_return(ReturnValue::Zero(Prim::Short), 3, 0, "v", None),
            format!("((short) {s}.rZ(3, 0, v))")
        );
    }

    /// Kotlin calls the same helpers with the type argument after the
    /// name; values with no type argument ignore the declared type.
    #[test]
    fn kotlin_returns_put_the_type_argument_after_the_name() {
        let s = RUNTIME;
        assert_eq!(
            kotlin_return(ReturnValue::EmptyList, 3, 0, "v", Some("List<Int>")),
            format!("{s}.rList<List<Int>>(3, 0, v)")
        );
        assert_eq!(
            kotlin_return(ReturnValue::Zero(Prim::Int), 3, 0, "v", Some("Int")),
            format!("{s}.rZ(3, 0, v)")
        );
        assert_eq!(
            kotlin_return(ReturnValue::Null, 3, 0, "v", None),
            format!("{s}.rN(3, 0, v)")
        );
    }

    /// Of two sites that overlap without one holding the other, the later
    /// is left out: its text is already inside the first switch.
    #[test]
    fn an_overlapping_site_is_left_out() {
        let first = crafted((0, 3), vec![(0, 3)], Form::Negate);
        let second = crafted((2, 5), vec![(2, 5)], Form::Negate);
        let out = switch_source("abcde", JvmLanguage::Java, 3, &[first, second], &[0, 1]);
        assert_eq!(out.source, format!("{RUNTIME}.n(3, 0, abc)de"));
        assert_eq!(out.regions, vec![(0, (0, out.source.len() - 2))]);
    }

    /// A site running past the end of the text is left out, and the text is
    /// written unchanged.
    #[test]
    fn a_site_past_the_end_is_left_out() {
        let past = crafted((1, 10), vec![(1, 10)], Form::Negate);
        let out = switch_source("abc", JvmLanguage::Java, 3, &[past], &[0]);
        assert_eq!(out.source, "abc");
        assert!(out.regions.is_empty());
    }

    /// A template's hole the placement does not have is written empty.
    #[test]
    fn a_missing_hole_is_written_empty() {
        let lt = crafted((0, 5), vec![(0, 1)], Form::Compare("lt"));
        assert_eq!(
            one("a < b", JvmLanguage::Java, lt),
            format!("{RUNTIME}.lt(3, 0, a, )")
        );
    }
}
