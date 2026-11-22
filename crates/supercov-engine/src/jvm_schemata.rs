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
            let outer = mutants[sites[*top]].placement.span;
            if outer.0 <= span.0 && span.1 <= outer.1 {
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
        let mut rest = template.as_str();
        while let Some(at) = rest.find('\u{1}') {
            out.push_str(&rest[..at]);
            let number = rest[at + 1..]
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
            rest = &rest[at + 2..];
        }
        out.push_str(rest);
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
                }
                | Form::Step {
                    from,
                    to,
                    statement,
                    prefix: _,
                } if matches!(form, Form::Compound { .. }) => {
                    let _ = (from, to);
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
                Form::Compound { .. } => unreachable!("matched above"),
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
        (ReturnValue::Zero(_), _) | (ReturnValue::EmptyString, _) | (_, None) => {
            java_return(value, f, k, hole, None)
        }
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

    #[test]
    fn a_compiler_position_is_an_offset() {
        let text = "ab\ncdé\nf";
        assert_eq!(offset_of(text, 1, 1), Some(0));
        assert_eq!(offset_of(text, 2, 3), Some(5));
        assert_eq!(offset_of(text, 2, 9), Some(7));
        assert_eq!(offset_of(text, 3, 1), Some(8));
        assert_eq!(offset_of(text, 5, 1), None);
    }
}
