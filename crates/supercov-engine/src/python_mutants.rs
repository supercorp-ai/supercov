//! mutmut's mutants of a Python file (`docs/design/python-mutants.md`).
//!
//! mutmut 3.8.1 walks a file's libcst tree in source order and asks each of
//! its operators, in a fixed order, for mutants of every node it enters. It
//! keeps the mutants inside a top-level function, or a method of a top-level
//! class whose body is indented, and numbers them per function
//! (`x_func__mutmut_3`, `xǁClassǁmethod__mutmut_3`). This module walks ruff's
//! tree the same way and applies the same operators and skip rules, so
//! `enumerate` returns the same mutants, in the same order. Each mutant says
//! where it is (its span, as Supercov's ids name it), and the edit that makes
//! it: a span of the source and the text mutmut's code generator writes there.
//! `scripts/mutmut-parity.py` prints mutmut's own list, and the tests compare
//! the two on the fixtures in `tests/fixtures/mutmut-parity`.
//!
//! # Mutators and descriptors
//!
//! An id is `file:startLine:startCol-endLine:endCol:Mutator:descriptor`, lines
//! and columns 1-based, columns in code points. StrykerJS's mutator names are
//! used where the mutation is the same, so the weak rules keyed by them apply:
//!
//! | mutmut operator | Mutator | Descriptor |
//! | --- | --- | --- |
//! | `operator_number` | `NumberLiteral` | the new literal (`2`, `2.5`, `1e+16`, `4j`) |
//! | `operator_string` | `StringLiteral` | `XX`, `lower` or `upper` |
//! | `operator_name` | `BooleanLiteral` / `Name` | `True`, `False` / `copy` |
//! | `operator_assignment` | `AssignmentValue` | `None`, or `""` for `x = None` |
//! | `operator_augmented_assignment` | `AugmentedAssignment` | `=` |
//! | `operator_remove_unary_ops` | `BooleanLiteral` (`not`) / `UnaryOperator` (`~`) | `removed` |
//! | `operator_dict_arguments` | `DictKeyword` | the new keyword (`aXX`) |
//! | `operator_arg_removal` | `CallArgument` | `None@<n>` or `removed@<n>`, the argument's 1-based place |
//! | `operator_*_string_methods_swap` | `MethodExpression` | the new method (`upper`) |
//! | `operator_lambda` | `Lambda` | `None`, or `0` for a body that is `None` |
//! | `operator_keywords` | `EqualityOperator` (`is`, `in`) / `LoopControl` | the new operator / `return` or `break` |
//! | `operator_swap_op` | `ArithmeticOperator`, `UnaryOperator`, `LogicalOperator`, `EqualityOperator`, `AssignmentOperator` | the new operator |
//! | `operator_match` | `MatchCase` | `removed@<n>` |
//! | `operator_if_exp` | `IfExpression` | `false` (`and False`) or `true` (`or True`) |
//!
//! The same grammar is implemented in the parity script; keep the two the
//! same. A comparison mutant's span starts at its operator and ends with the
//! operand after it, as libcst's `ComparisonTarget` does.

use std::collections::BTreeSet;

use ruff_python_ast::{
    self as ast, BoolOp, CmpOp, Expr, Operator, Stmt, UnaryOp,
    token::TokenKind,
    visitor::source_order::{SourceOrderVisitor, walk_expr, walk_pattern, walk_stmt},
};
use ruff_python_parser::parse_module;
use ruff_text_size::{Ranged, TextRange};

use crate::python_instrumenter::PythonInstrumenterError;

/// The mutant set: mutmut's version, named in every report.
pub const MUTANT_SET: &str = "mutmut@3.8.1";

/// Functions mutmut never mutates, at any depth.
const NEVER_MUTATE_FUNCTION_NAMES: &[&str] = &["__getattribute__", "__setattr__", "__new__"];
/// Methods of an enum class mutmut never mutates.
const NEVER_MUTATE_ENUM_FUNCTION_NAMES: &[&str] = &["__init__", "_generate_next_value_"];
const ENUM_BASE_NAMES: &[&str] = &["Enum", "IntEnum", "StrEnum", "Flag", "IntFlag"];
/// Calls mutmut leaves alone, arguments and all.
const NEVER_MUTATE_FUNCTION_CALLS: &[&str] = &["len", "isinstance"];

const SYMMETRIC_STRING_METHODS: &[(&str, &str)] = &[
    ("lower", "upper"),
    ("upper", "lower"),
    ("lstrip", "rstrip"),
    ("rstrip", "lstrip"),
    ("find", "rfind"),
    ("rfind", "find"),
    ("ljust", "rjust"),
    ("rjust", "ljust"),
    ("index", "rindex"),
    ("rindex", "index"),
    ("removeprefix", "removesuffix"),
    ("removesuffix", "removeprefix"),
    ("partition", "rpartition"),
    ("rpartition", "partition"),
];

/// A function mutmut copies: a top-level function, or a method of a
/// top-level class. Its mutants switch together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    /// mutmut's mangled name: `x_func` or `xǁClassǁmethod`.
    pub key: String,
    /// The byte where its `def` (or `async`) keyword starts.
    pub def: usize,
    /// Its end.
    pub end: usize,
    /// Each of its own parameters with a default: the default's range, the
    /// parameter's name, and whether it is keyword-only.
    pub defaults: Vec<(TextRange, String, bool)>,
}

/// One of mutmut's mutants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PythonMutant {
    pub mutator: &'static str,
    pub descriptor: String,
    /// Where it is, in bytes: what its id names.
    pub span: (usize, usize),
    /// The bytes it replaces, and with what: mutmut's text there.
    pub splice: (usize, usize),
    pub replacement: String,
    /// Its function, an index into `Enumeration::units`.
    pub unit: usize,
    /// Its place among its function's mutants, from 1: mutmut's number.
    pub ordinal: usize,
    /// Set where it changes a default of its function's own parameter: the
    /// parameter's name and whether it is keyword-only.
    pub default_of: Option<(String, bool)>,
    /// Statically equivalent: the literal's value does not change.
    pub same_value: bool,
    /// The operator it changes: where it starts, and its text.
    pub operator: Option<(usize, &'static str)>,
}

/// A file's mutants and the functions that hold them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Enumeration {
    pub units: Vec<Unit>,
    pub mutants: Vec<PythonMutant>,
}

/// mutmut's mutants of `source`, in mutmut's order.
pub fn enumerate(source: &str) -> Result<Enumeration, PythonInstrumenterError> {
    if u32::try_from(source.len()).is_err() {
        return Err(PythonInstrumenterError::SourceTooLarge);
    }
    let parsed =
        parse_module(source).map_err(|error| PythonInstrumenterError::Parse(error.to_string()))?;
    let comments = parsed
        .tokens()
        .iter()
        .filter(|token| token.kind() == TokenKind::Comment)
        .map(Ranged::range)
        .collect::<Vec<_>>();
    let text = Text::new(source, &comments);
    let pragmas = Pragmas::read(&text, parsed.suite());
    let mut walker = Walker {
        text: &text,
        pragmas: &pragmas,
        units: Vec::new(),
        mutants: Vec::new(),
        unit: None,
        bounds: Vec::new(),
        enum_hooks: BTreeSet::new(),
    };
    for statement in parsed.suite() {
        match statement {
            Stmt::FunctionDef(_) => walker.unit_of(statement, None),
            Stmt::ClassDef(class) => {
                walker.note_enum_hooks(class);
                if walker.skips_class(class) || !walker.text.indented_body(&class.body) {
                    continue;
                }
                for method in &class.body {
                    if matches!(method, Stmt::FunctionDef(_)) {
                        walker.unit_of(method, Some(&class.name));
                    }
                }
            }
            _ => {}
        }
    }
    // Number each function's mutants, and keep only functions that have some.
    let mut counts = vec![0usize; walker.units.len()];
    for mutant in &mut walker.mutants {
        counts[mutant.unit] += 1;
        mutant.ordinal = counts[mutant.unit];
    }
    let mut renumbered = vec![usize::MAX; walker.units.len()];
    let mut units = Vec::new();
    for (index, unit) in walker.units.into_iter().enumerate() {
        if counts[index] > 0 {
            renumbered[index] = units.len();
            units.push(unit);
        }
    }
    let mut mutants = walker.mutants;
    for mutant in &mut mutants {
        mutant.unit = renumbered[mutant.unit];
        let unit = &units[mutant.unit];
        mutant.default_of = unit
            .defaults
            .iter()
            .find(|(range, _, _)| {
                range.start().to_usize() <= mutant.span.0 && mutant.span.1 <= range.end().to_usize()
            })
            .map(|(_, name, keyword)| (name.clone(), *keyword));
    }
    Ok(Enumeration { units, mutants })
}

/// The source with its line starts and comments, for positions and trivia.
pub(crate) struct Text<'a> {
    pub(crate) source: &'a str,
    line_starts: Vec<usize>,
    /// Each comment's start by its end, and its end by its start.
    comments: Vec<(usize, usize)>,
}

impl<'a> Text<'a> {
    pub(crate) fn new(source: &'a str, comments: &[TextRange]) -> Self {
        let mut line_starts = vec![0];
        line_starts.extend(
            source
                .bytes()
                .enumerate()
                .filter_map(|(index, byte)| (byte == b'\n').then_some(index + 1)),
        );
        Self {
            source,
            line_starts,
            comments: comments
                .iter()
                .map(|range| (range.start().to_usize(), range.end().to_usize()))
                .collect(),
        }
    }

    /// The 1-based line of a byte offset.
    pub(crate) fn line(&self, offset: usize) -> usize {
        self.line_starts.partition_point(|start| *start <= offset)
    }

    /// The byte where a 1-based line starts.
    fn line_start(&self, line: usize) -> usize {
        self.line_starts
            .get(line - 1)
            .copied()
            .unwrap_or(self.source.len())
    }

    /// A byte offset as a 1-based line and 1-based column in code points.
    pub(crate) fn position(&self, offset: usize) -> (u32, u32) {
        let line = self.line(offset);
        let start = self.line_start(line);
        let column = self.source[start..offset].chars().count() + 1;
        (line as u32, column as u32)
    }

    fn comment_starting(&self, at: usize) -> Option<usize> {
        self.comments
            .iter()
            .find(|(start, _)| *start == at)
            .map(|(_, end)| *end)
    }

    fn comment_ending(&self, at: usize) -> Option<usize> {
        self.comments
            .iter()
            .find(|(_, end)| *end == at)
            .map(|(start, _)| *start)
    }

    /// Past whitespace, comments and line continuations from `at`, not
    /// beyond `limit`.
    fn skip_forward(&self, mut at: usize, limit: usize) -> usize {
        let bytes = self.source.as_bytes();
        while at < limit {
            match bytes[at] {
                b' ' | b'\t' | b'\n' | b'\r' | b'\x0c' => at += 1,
                b'\\' => at += 1,
                b'#' => at = self.comment_starting(at).unwrap_or(at + 1),
                _ => break,
            }
        }
        at
    }

    /// Back past whitespace, comments and line continuations from `at`, not
    /// before `limit`.
    fn skip_backward(&self, mut at: usize, limit: usize) -> usize {
        let bytes = self.source.as_bytes();
        while at > limit {
            if let Some(start) = self.comment_ending(at) {
                at = start;
                continue;
            }
            match bytes[at - 1] {
                b' ' | b'\t' | b'\n' | b'\r' | b'\x0c' | b'\\' => at -= 1,
                _ => break,
            }
        }
        at
    }

    /// An expression's range with the parentheses that are its own, within
    /// `bounds`: as many as open before it and close after it.
    fn own_parens(&self, range: TextRange, bounds: (usize, usize)) -> (usize, usize) {
        let bytes = self.source.as_bytes();
        let (mut start, mut end) = (range.start().to_usize(), range.end().to_usize());
        loop {
            let before = self.skip_backward(start, bounds.0);
            let after = self.skip_forward(end, bounds.1);
            if before > bounds.0
                && after < bounds.1
                && bytes[before - 1] == b'('
                && bytes[after] == b')'
            {
                start = before - 1;
                end = after + 1;
            } else {
                return (start, end);
            }
        }
    }

    /// Past the closing parentheses after an operand, up to the operator
    /// that follows it: where the operand's own text ends.
    fn closes_after(&self, end: usize, limit: usize) -> usize {
        let bytes = self.source.as_bytes();
        let mut kept = end;
        let mut at = end;
        loop {
            at = self.skip_forward(at, limit);
            if at < limit && bytes[at] == b')' {
                at += 1;
                kept = at;
            } else {
                return kept;
            }
        }
    }

    /// Back past the opening parentheses before an operand, down to the
    /// operator before it: where the operand's own text starts.
    fn opens_before(&self, start: usize, limit: usize) -> usize {
        let bytes = self.source.as_bytes();
        let mut kept = start;
        let mut at = start;
        loop {
            at = self.skip_backward(at, limit);
            if at > limit && bytes[at - 1] == b'(' {
                at -= 1;
                kept = at;
            } else {
                return kept;
            }
        }
    }

    /// Whether a compound statement's body is an indented block, not a suite
    /// on its header's line.
    fn indented_body(&self, body: &[Stmt]) -> bool {
        body.first().is_none_or(|first| {
            let start = first.range().start().to_usize();
            let line_start = self.line_start(self.line(start));
            self.source[line_start..start]
                .bytes()
                .all(|byte| matches!(byte, b' ' | b'\t' | b'\x0c'))
        })
    }

    fn slice(&self, range: TextRange) -> &'a str {
        &self.source[range.start().to_usize()..range.end().to_usize()]
    }
}

/// Lines a `# pragma: no mutate` comment keeps mutmut from mutating.
#[derive(Debug, Default)]
struct Pragmas {
    /// Nodes that start here are not mutated.
    no_mutate: BTreeSet<usize>,
    /// Functions and classes that start here are skipped whole, and other
    /// nodes are not mutated.
    ignore_node: BTreeSet<usize>,
}

/// `block`, `start`, `end`, or `bare` for a pragma comment; `None` for any
/// other comment.
fn pragma_token(comment: &str) -> Option<&'static str> {
    if !comment.contains("# pragma:") || !comment.contains("no mutate") {
        return None;
    }
    let tail = comment
        .split_once("no mutate")
        .map_or("", |(_, tail)| tail)
        .trim();
    let tail = tail.trim_start_matches([':', ' ']);
    let token = tail
        .split(',')
        .next()
        .unwrap_or("")
        .split_whitespace()
        .next();
    Some(match token {
        Some("block") => "block",
        Some("start") => "start",
        Some("end") => "end",
        _ => "bare",
    })
}

impl Pragmas {
    fn read(text: &Text<'_>, suite: &[Stmt]) -> Self {
        let mut pragmas = Self::default();
        let tokens = text
            .comments
            .iter()
            .filter_map(|&(start, end)| {
                pragma_token(&text.source[start..end]).map(|token| (start, token))
            })
            .collect::<Vec<_>>();
        if tokens.is_empty() {
            return pragmas;
        }
        let mut headers = Vec::new();
        let mut simple = Vec::new();
        let mut bodies = Vec::new();
        collect_bodies(text, suite, &mut headers, &mut simple, &mut bodies);
        let mut opened: Option<usize> = None;
        for &(at, token) in &tokens {
            let line = text.line(at);
            let line_start = text.line_start(line);
            let own_line = text.source[line_start..at]
                .bytes()
                .all(|byte| matches!(byte, b' ' | b'\t' | b'\x0c'));
            if own_line {
                match token {
                    "start" => {
                        pragmas.no_mutate.insert(line);
                        opened = Some(line);
                    }
                    "end" => {
                        if let Some(start) = opened.take() {
                            pragmas.no_mutate.extend(start..=line);
                        }
                    }
                    "block" => {
                        pragmas.no_mutate.insert(line);
                        let indent = at - line_start;
                        // The body whose next statement this comment leads:
                        // that statement and every one after it in the body.
                        let led = bodies.iter().find_map(|body| {
                            let next = body.iter().position(|(start, _)| *start > at)?;
                            let previous_end = match next {
                                0 => 0,
                                _ => body[next - 1].1,
                            };
                            let start = body[next].0;
                            let column = start - text.line_start(text.line(start));
                            (previous_end <= at && indent <= column).then(|| &body[next..])
                        });
                        for &(start, end) in led.into_iter().flatten() {
                            pragmas.no_mutate.extend(text.line(start)..=text.line(end));
                        }
                    }
                    _ => {
                        pragmas.no_mutate.insert(line);
                    }
                }
                continue;
            }
            // A trailing comment: of a simple statement's line, of a
            // compound statement's header, or inside brackets.
            if let Some(&(start, _)) = simple
                .iter()
                .find(|(_, end)| *end <= at && text.line(*end) == line)
            {
                pragmas.no_mutate.insert(text.line(start));
            } else if let Some(&(node_line, colon, last)) = headers
                .iter()
                .find(|(_, colon, _)| text.line(*colon) == line && *colon <= at)
            {
                if token == "block" {
                    pragmas.ignore_node.insert(node_line);
                    pragmas
                        .ignore_node
                        .extend(text.line(colon)..=text.line(last));
                } else {
                    pragmas.no_mutate.insert(node_line);
                }
            } else if token == "bare" {
                pragmas.no_mutate.insert(line);
            }
        }
        pragmas
    }
}

/// Every compound header's line, colon and body end; every simple
/// statement's range; and every body's statements' ranges.
fn collect_bodies(
    text: &Text<'_>,
    suite: &[Stmt],
    headers: &mut Vec<(usize, usize, usize)>,
    simple: &mut Vec<(usize, usize)>,
    bodies: &mut Vec<Vec<(usize, usize)>>,
) {
    bodies.push(
        suite
            .iter()
            .map(|statement| {
                (
                    statement.range().start().to_usize(),
                    statement.range().end().to_usize(),
                )
            })
            .collect(),
    );
    let mut header = |node: usize, body: &[Stmt]| {
        let (Some(first), Some(last)) = (body.first(), body.last()) else {
            return;
        };
        let colon = text.skip_backward(first.range().start().to_usize(), node);
        if text.indented_body(body) {
            headers.push((text.line(node), colon, last.range().end().to_usize()));
        }
    };
    let mut nested: Vec<&[Stmt]> = Vec::new();
    for statement in suite {
        let start = statement.range().start().to_usize();
        match statement {
            Stmt::FunctionDef(node) => {
                header(start, &node.body);
                nested.push(&node.body);
            }
            Stmt::ClassDef(node) => {
                header(start, &node.body);
                nested.push(&node.body);
            }
            Stmt::If(node) => {
                header(start, &node.body);
                nested.push(&node.body);
                for clause in &node.elif_else_clauses {
                    header(clause.range.start().to_usize(), &clause.body);
                    nested.push(&clause.body);
                }
            }
            Stmt::For(node) => {
                header(start, &node.body);
                nested.push(&node.body);
                else_header(text, &node.orelse, &mut header);
                nested.push(&node.orelse);
            }
            Stmt::While(node) => {
                header(start, &node.body);
                nested.push(&node.body);
                else_header(text, &node.orelse, &mut header);
                nested.push(&node.orelse);
            }
            Stmt::With(node) => {
                header(start, &node.body);
                nested.push(&node.body);
            }
            Stmt::Try(node) => {
                header(start, &node.body);
                nested.push(&node.body);
                for handler in &node.handlers {
                    let ast::ExceptHandler::ExceptHandler(handler) = handler;
                    header(handler.range.start().to_usize(), &handler.body);
                    nested.push(&handler.body);
                }
                else_header(text, &node.orelse, &mut header);
                nested.push(&node.orelse);
                else_header(text, &node.finalbody, &mut header);
                nested.push(&node.finalbody);
            }
            Stmt::Match(node) => {
                for case in &node.cases {
                    header(case.range.start().to_usize(), &case.body);
                    nested.push(&case.body);
                }
            }
            _ => simple.push((start, statement.range().end().to_usize())),
        }
    }
    for body in nested {
        collect_bodies(text, body, headers, simple, bodies);
    }
}

/// The header of an `else` or `finally` clause, which ruff gives no node:
/// its keyword is the word before the colon before its body.
fn else_header(text: &Text<'_>, body: &[Stmt], header: &mut impl FnMut(usize, &[Stmt])) {
    let Some(first) = body.first() else {
        return;
    };
    let colon = text.skip_backward(first.range().start().to_usize(), 0);
    let keyword_end = text.skip_backward(colon.saturating_sub(1), 0);
    let keyword_start = text.source[..keyword_end]
        .rfind(|character: char| !character.is_ascii_alphabetic())
        .map_or(0, |at| at + 1);
    header(keyword_start, body);
}

struct Walker<'t, 'a> {
    text: &'t Text<'a>,
    pragmas: &'t Pragmas,
    units: Vec<Unit>,
    mutants: Vec<PythonMutant>,
    /// The function whose mutants are being listed.
    unit: Option<usize>,
    /// The innermost brackets an expression's own parentheses can reach:
    /// its statement, or the call whose argument it is.
    bounds: Vec<(usize, usize)>,
    /// Methods mutmut leaves alone in enum classes, by their start.
    enum_hooks: BTreeSet<usize>,
}

fn range_of(range: TextRange) -> (usize, usize) {
    (range.start().to_usize(), range.end().to_usize())
}

fn binary(operator: Operator) -> &'static str {
    match operator {
        Operator::Add => "+",
        Operator::Sub => "-",
        Operator::Mult => "*",
        Operator::MatMult => "@",
        Operator::Div => "/",
        Operator::Mod => "%",
        Operator::Pow => "**",
        Operator::LShift => "<<",
        Operator::RShift => ">>",
        Operator::BitOr => "|",
        Operator::BitXor => "^",
        Operator::BitAnd => "&",
        Operator::FloorDiv => "//",
    }
}

/// mutmut's swap of a binary (or augmented) operator.
fn swapped(operator: Operator) -> Option<Operator> {
    Some(match operator {
        Operator::Add => Operator::Sub,
        Operator::Sub => Operator::Add,
        Operator::Mult | Operator::FloorDiv | Operator::Mod => Operator::Div,
        Operator::Div | Operator::Pow => Operator::Mult,
        Operator::LShift => Operator::RShift,
        Operator::RShift => Operator::LShift,
        Operator::BitAnd => Operator::BitOr,
        Operator::BitOr | Operator::BitXor => Operator::BitAnd,
        Operator::MatMult => return None,
    })
}

fn augmented(operator: Operator) -> &'static str {
    match operator {
        Operator::Add => "+=",
        Operator::Sub => "-=",
        Operator::Mult => "*=",
        Operator::MatMult => "@=",
        Operator::Div => "/=",
        Operator::Mod => "%=",
        Operator::Pow => "**=",
        Operator::LShift => "<<=",
        Operator::RShift => ">>=",
        Operator::BitOr => "|=",
        Operator::BitXor => "^=",
        Operator::BitAnd => "&=",
        Operator::FloorDiv => "//=",
    }
}

fn comparison(operator: CmpOp) -> &'static str {
    match operator {
        CmpOp::Eq => "==",
        CmpOp::NotEq => "!=",
        CmpOp::Lt => "<",
        CmpOp::LtE => "<=",
        CmpOp::Gt => ">",
        CmpOp::GtE => ">=",
        CmpOp::Is => "is",
        CmpOp::IsNot => "is not",
        CmpOp::In => "in",
        CmpOp::NotIn => "not in",
    }
}

impl<'t, 'a> Walker<'t, 'a> {
    fn line(&self, offset: usize) -> usize {
        self.text.line(offset)
    }

    /// Whether mutmut mutates a node that starts at `start`.
    fn mutable_at(&self, start: usize) -> bool {
        let line = self.line(start);
        !self.pragmas.no_mutate.contains(&line) && !self.pragmas.ignore_node.contains(&line)
    }

    /// Whether a mutation would delete code a pragma keeps: a node that
    /// starts on such a line among `dropped`, the ranges the mutated node no
    /// longer holds, where the original spans such a line.
    fn removes_ignored(&self, node: (usize, usize), dropped: &[(usize, usize)]) -> bool {
        let ignored = |line: &usize| {
            self.pragmas.no_mutate.contains(line) || self.pragmas.ignore_node.contains(line)
        };
        if (self.line(node.0)..=self.line(node.1.max(node.0))).all(|line| !ignored(&line)) {
            return false;
        }
        dropped.iter().any(|&(start, end)| {
            // Every line a token of the dropped code starts on.
            let mut at = start;
            while at < end {
                let next = self.text.skip_forward(at, end);
                if next >= end {
                    break;
                }
                if ignored(&self.line(next)) {
                    return true;
                }
                at = self.text.source[next..end]
                    .find(['\n', ' ', '\t'])
                    .map_or(end, |offset| next + offset);
            }
            false
        })
    }

    fn bounds(&self) -> (usize, usize) {
        self.bounds
            .last()
            .copied()
            .unwrap_or((0, self.text.source.len()))
    }

    #[allow(clippy::too_many_arguments)]
    fn push(
        &mut self,
        mutator: &'static str,
        descriptor: impl Into<String>,
        span: (usize, usize),
        splice: (usize, usize),
        replacement: impl Into<String>,
        dropped: &[(usize, usize)],
    ) {
        let Some(unit) = self.unit else {
            return;
        };
        if !self.mutable_at(span.0) || self.removes_ignored(span, dropped) {
            return;
        }
        self.mutants.push(PythonMutant {
            mutator,
            descriptor: descriptor.into(),
            span,
            splice,
            replacement: replacement.into(),
            unit,
            ordinal: 0,
            default_of: None,
            same_value: false,
            operator: None,
        });
    }

    /// The last mutant pushed, if `before` mutants were there: it changes
    /// the operator at `at`.
    fn note_operator(&mut self, before: usize, at: usize, text: &'static str) {
        if self.mutants.len() > before {
            self.mutants.last_mut().expect("pushed").operator = Some((at, text));
        }
    }

    fn unit_of(&mut self, statement: &Stmt, class: Option<&ast::Identifier>) {
        let Stmt::FunctionDef(function) = statement else {
            return;
        };
        let key = match class {
            Some(class) => format!("xǁ{}ǁ{}", class.as_str(), function.name.as_str()),
            None => format!("x_{}", function.name.as_str()),
        };
        let name_start = function.name.range.start().to_usize();
        let def_end = self.text.skip_backward(name_start, 0);
        let mut def = def_end.saturating_sub(3);
        if function.is_async {
            def = self.text.skip_backward(def, 0).saturating_sub(5);
        }
        let defaults = function
            .parameters
            .iter_non_variadic_params()
            .filter_map(|parameter| {
                let default = parameter.default.as_deref()?;
                let keyword_only = function
                    .parameters
                    .kwonlyargs
                    .iter()
                    .any(|kwonly| std::ptr::eq(kwonly, parameter));
                Some((
                    default.range(),
                    parameter.parameter.name.as_str().to_owned(),
                    keyword_only,
                ))
            })
            .collect();
        self.units.push(Unit {
            key,
            def,
            end: function.range.end().to_usize(),
            defaults,
        });
        self.unit = Some(self.units.len() - 1);
        self.visit_stmt(statement);
        self.unit = None;
    }

    fn note_enum_hooks(&mut self, class: &ast::StmtClassDef) {
        let is_enum = class.arguments.as_ref().is_some_and(|arguments| {
            arguments.args.iter().any(|base| {
                let name = match base {
                    Expr::Name(name) => name.id.as_str(),
                    Expr::Attribute(attribute) => attribute.attr.as_str(),
                    _ => return false,
                };
                ENUM_BASE_NAMES.contains(&name)
            })
        });
        if !is_enum {
            return;
        }
        for statement in &class.body {
            if let Stmt::FunctionDef(function) = statement
                && NEVER_MUTATE_ENUM_FUNCTION_NAMES.contains(&function.name.as_str())
            {
                self.enum_hooks.insert(function.range.start().to_usize());
            }
        }
    }

    fn skips_class(&self, class: &ast::StmtClassDef) -> bool {
        self.pragmas
            .ignore_node
            .contains(&self.line(class.range.start().to_usize()))
    }

    fn skips_function(&self, function: &ast::StmtFunctionDef) -> bool {
        let start = function.range.start().to_usize();
        if self.pragmas.ignore_node.contains(&self.line(start))
            || self.enum_hooks.contains(&start)
            || NEVER_MUTATE_FUNCTION_NAMES.contains(&function.name.as_str())
        {
            return true;
        }
        match function.decorator_list.as_slice() {
            [] => false,
            [only] => !matches!(
                &only.expression,
                Expr::Name(name) if matches!(name.id.as_str(), "staticmethod" | "classmethod")
            ),
            _ => true,
        }
    }

    fn statement_mutants(&mut self, statement: &Stmt) {
        let range = range_of(statement.range());
        match statement {
            Stmt::Assign(assign) => self.assignment(range, &assign.value),
            Stmt::AnnAssign(assign) => {
                if let Some(value) = &assign.value {
                    self.assignment(range, value);
                }
            }
            Stmt::AugAssign(assign) => {
                let target_end = self
                    .text
                    .closes_after(assign.target.range().end().to_usize(), range.1);
                let value_start = self
                    .text
                    .opens_before(assign.value.range().start().to_usize(), target_end);
                let operator_start = self.text.skip_forward(target_end, value_start);
                let operator = (operator_start, operator_start + binary(assign.op).len() + 1);
                let original = augmented(assign.op);
                self.push(
                    "AugmentedAssignment",
                    "=",
                    range,
                    (target_end, value_start),
                    " = ",
                    &[operator],
                );
                if let Some(new) = swapped(assign.op) {
                    let text = format!("{}=", binary(new));
                    let before = self.mutants.len();
                    self.push(
                        "AssignmentOperator",
                        text.clone(),
                        range,
                        (target_end, value_start),
                        format!(" {text} "),
                        &[operator],
                    );
                    self.note_operator(before, operator_start, original);
                }
            }
            Stmt::Break(_) => self.push("LoopControl", "return", range, range, "return", &[range]),
            Stmt::Continue(_) => {
                self.push("LoopControl", "break", range, range, "break", &[range]);
            }
            Stmt::Match(statement) if statement.cases.len() > 1 => {
                let regions = self.case_regions(statement);
                for (index, region) in regions.iter().enumerate() {
                    self.push(
                        "MatchCase",
                        format!("removed@{}", index + 1),
                        range,
                        *region,
                        "",
                        &[*region],
                    );
                }
            }
            _ => {}
        }
    }

    fn assignment(&mut self, range: (usize, usize), value: &Expr) {
        let value_range = self.text.own_parens(value.range(), range);
        let (descriptor, replacement) = if value.is_none_literal_expr() {
            ("\"\"", "\"\"")
        } else {
            ("None", "None")
        };
        self.push(
            "AssignmentValue",
            descriptor,
            range,
            value_range,
            replacement,
            &[value_range],
        );
    }

    /// The text each `case` of a `match` owns: its comment lines before it,
    /// at its indentation or less, and its lines, to the next case's.
    fn case_regions(&self, statement: &ast::StmtMatch) -> Vec<(usize, usize)> {
        let starts = statement
            .cases
            .iter()
            .enumerate()
            .map(|(index, case)| {
                let case_start = case.range.start().to_usize();
                let mut line = self.line(case_start);
                let indent = case_start - self.text.line_start(line);
                let floor = match index {
                    0 => self.line(statement.range.start().to_usize()),
                    _ => self.line(statement.cases[index - 1].range.end().to_usize()),
                };
                while line - 1 > floor {
                    let previous = self.text.line_start(line - 1);
                    let content = &self.text.source[previous..self.text.line_start(line)];
                    let trimmed = content.trim_start_matches([' ', '\t', '\x0c']);
                    let leading = content.len() - trimmed.len();
                    let blank = trimmed.trim().is_empty();
                    if blank || (trimmed.starts_with('#') && leading <= indent) {
                        line -= 1;
                    } else {
                        break;
                    }
                }
                self.text.line_start(line)
            })
            .collect::<Vec<_>>();
        let last = statement.cases.last().expect("a match with cases");
        let end = self
            .text
            .line_start(self.line(last.range.end().to_usize()) + 1)
            .min(self.text.source.len());
        starts
            .iter()
            .enumerate()
            .map(|(index, start)| (*start, starts.get(index + 1).copied().unwrap_or(end)))
            .collect()
    }

    fn expression_mutants(&mut self, expression: &Expr) {
        let range = range_of(expression.range());
        match expression {
            Expr::NumberLiteral(_) => {
                let literal = self.text.slice(expression.range());
                if let Some((replacement, same)) = number_mutant(literal) {
                    let before = self.mutants.len();
                    self.push(
                        "NumberLiteral",
                        replacement.clone(),
                        range,
                        range,
                        replacement,
                        &[range],
                    );
                    if same && self.mutants.len() > before {
                        self.mutants.last_mut().expect("pushed").same_value = true;
                    }
                }
            }
            Expr::BooleanLiteral(literal) => {
                let new = if literal.value { "False" } else { "True" };
                self.push("BooleanLiteral", new, range, range, new, &[range]);
            }
            Expr::Name(name) if name.id.as_str() == "deepcopy" => {
                self.push("Name", "copy", range, range, "copy", &[range]);
            }
            Expr::UnaryOp(unary) => {
                let operand_start = self.text.skip_forward(
                    range.0 + if unary.op == UnaryOp::Not { 3 } else { 1 },
                    range.1,
                );
                let operator = (range.0, range.0 + 1);
                match unary.op {
                    UnaryOp::Not | UnaryOp::Invert => {
                        let own = self.text.own_parens(expression.range(), self.bounds());
                        let mutator = if unary.op == UnaryOp::Not {
                            "BooleanLiteral"
                        } else {
                            "UnaryOperator"
                        };
                        self.push(
                            mutator,
                            "removed",
                            range,
                            own,
                            &self.text.source[operand_start..range.1],
                            &[operator],
                        );
                    }
                    UnaryOp::USub | UnaryOp::UAdd => {
                        let (old, new) = if unary.op == UnaryOp::USub {
                            ("-", "+")
                        } else {
                            ("+", "-")
                        };
                        let before = self.mutants.len();
                        self.push(
                            "UnaryOperator",
                            new,
                            range,
                            (range.0, operand_start),
                            new,
                            &[operator],
                        );
                        self.note_operator(before, range.0, old);
                    }
                }
            }
            Expr::BinOp(operation) => {
                let Some(new) = swapped(operation.op) else {
                    return;
                };
                let left_end = self
                    .text
                    .closes_after(operation.left.range().end().to_usize(), range.1);
                let right_start = self
                    .text
                    .opens_before(operation.right.range().start().to_usize(), left_end);
                let operator_start = self.text.skip_forward(left_end, right_start);
                let before = self.mutants.len();
                self.push(
                    "ArithmeticOperator",
                    binary(new),
                    range,
                    (left_end, right_start),
                    format!(" {} ", binary(new)),
                    &[(operator_start, operator_start + 1)],
                );
                self.note_operator(before, operator_start, binary(operation.op));
            }
            Expr::BoolOp(operation) => {
                let (old, new) = match operation.op {
                    BoolOp::And => ("and", "or"),
                    BoolOp::Or => ("or", "and"),
                };
                // libcst nests `a and b and c` to the left: the outermost
                // operation, then the one inside it, each up to its operand.
                for index in (1..operation.values.len()).rev() {
                    let right = &operation.values[index];
                    let left = &operation.values[index - 1];
                    let end = if index + 1 == operation.values.len() {
                        range.1
                    } else {
                        let next_start = operation.values[index + 1].range().start().to_usize();
                        self.text
                            .closes_after(right.range().end().to_usize(), next_start)
                    };
                    let left_end = self.text.closes_after(
                        left.range().end().to_usize(),
                        right.range().start().to_usize(),
                    );
                    let right_start = self
                        .text
                        .opens_before(right.range().start().to_usize(), left_end);
                    let operator_start = self.text.skip_forward(left_end, right_start);
                    let before = self.mutants.len();
                    self.push(
                        "LogicalOperator",
                        new,
                        (range.0, end),
                        (left_end, right_start),
                        format!(" {new} "),
                        &[(operator_start, operator_start + 1)],
                    );
                    self.note_operator(before, operator_start, old);
                }
            }
            Expr::Lambda(lambda) => {
                let body = self.text.own_parens(lambda.body.range(), range);
                let new = if lambda.body.is_none_literal_expr() {
                    "0"
                } else {
                    "None"
                };
                self.push("Lambda", new, range, body, new, &[body]);
            }
            Expr::If(conditional) => {
                let test = self.text.own_parens(conditional.test.range(), range);
                let inner = match &*conditional.test {
                    Expr::Tuple(ast::ExprTuple {
                        parenthesized: true,
                        ..
                    })
                    | Expr::Generator(ast::ExprGenerator {
                        parenthesized: true,
                        ..
                    }) => self.text.slice(conditional.test.range())[1..]
                        .strip_suffix(')')
                        .unwrap_or_default()
                        .trim(),
                    test => self.text.slice(test.range()),
                };
                for (descriptor, tail) in [("false", "and False"), ("true", "or True")] {
                    self.push(
                        "IfExpression",
                        descriptor,
                        range,
                        test,
                        format!("({inner}) {tail}"),
                        &[(test.0, test.0 + 1)],
                    );
                }
            }
            Expr::Call(call) => self.call_mutants(call, range),
            _ => {}
        }
    }

    fn call_mutants(&mut self, call: &ast::ExprCall, range: (usize, usize)) {
        let arguments = &call.arguments;
        let interior = (
            arguments.range.start().to_usize() + 1,
            arguments.range.end().to_usize() - 1,
        );
        let ordered = arguments.iter_source_order().collect::<Vec<_>>();
        // `dict(a=b)`: each keyword renamed, until an argument has none.
        if matches!(&*call.func, Expr::Name(name) if name.id.as_str() == "dict") {
            for argument in &ordered {
                let ast::ArgOrKeyword::Keyword(ast::Keyword {
                    arg: Some(name), ..
                }) = argument
                else {
                    break;
                };
                let renamed = format!("{}XX", name.as_str());
                let at = range_of(name.range);
                self.push("DictKeyword", renamed.clone(), range, at, renamed, &[at]);
            }
        }
        let starts = ordered
            .iter()
            .map(|argument| match argument {
                ast::ArgOrKeyword::Arg(value) => self.text.own_parens(value.range(), interior).0,
                ast::ArgOrKeyword::Keyword(keyword) => keyword.range.start().to_usize(),
            })
            .collect::<Vec<_>>();
        for (index, argument) in ordered.iter().enumerate() {
            let value = match argument {
                ast::ArgOrKeyword::Arg(Expr::Starred(_)) => continue,
                ast::ArgOrKeyword::Arg(value) => value,
                ast::ArgOrKeyword::Keyword(ast::Keyword { arg: None, .. }) => continue,
                ast::ArgOrKeyword::Keyword(keyword) => &keyword.value,
            };
            if value.is_none_literal_expr() {
                continue;
            }
            let at = self.text.own_parens(value.range(), interior);
            self.push(
                "CallArgument",
                format!("None@{}", index + 1),
                range,
                at,
                "None",
                &[at],
            );
        }
        if ordered.len() > 1 {
            for index in 0..ordered.len() {
                let region = (
                    starts[index],
                    starts.get(index + 1).copied().unwrap_or(interior.1),
                );
                self.push(
                    "CallArgument",
                    format!("removed@{}", index + 1),
                    range,
                    region,
                    "",
                    &[region],
                );
            }
        }
        if let Expr::Attribute(attribute) = &*call.func {
            let method = attribute.attr.as_str();
            let at = range_of(attribute.attr.range);
            for (old, new) in SYMMETRIC_STRING_METHODS {
                if *old == method {
                    self.push("MethodExpression", *new, range, at, *new, &[at]);
                }
            }
            let maxsplit = arguments.keywords.iter().any(|keyword| {
                keyword
                    .arg
                    .as_ref()
                    .is_some_and(|arg| arg.as_str() == "maxsplit")
            });
            for (old, new) in [("split", "rsplit"), ("rsplit", "split")] {
                if old == method && (ordered.len() == 2 || maxsplit) {
                    self.push("MethodExpression", new, range, at, new, &[at]);
                }
            }
        }
    }

    fn comparison(&mut self, compare: &ast::ExprCompare) {
        let range = range_of(compare.range);
        self.visit_expr(&compare.left);
        let mut previous = &*compare.left;
        for (operator, comparator) in compare.ops.iter().zip(&compare.comparators) {
            let previous_end = self.text.closes_after(
                previous.range().end().to_usize(),
                comparator.range().start().to_usize(),
            );
            let comparator_start = self
                .text
                .opens_before(comparator.range().start().to_usize(), previous_end);
            let operator_start = self.text.skip_forward(previous_end, comparator_start);
            let operator_end = match operator {
                CmpOp::IsNot | CmpOp::NotIn => {
                    let first = operator_start + 3.min(comparator_start - operator_start);
                    let second = self.text.skip_forward(
                        if *operator == CmpOp::IsNot {
                            operator_start + 2
                        } else {
                            first
                        },
                        comparator_start,
                    );
                    second + if *operator == CmpOp::IsNot { 3 } else { 2 }
                }
                other => operator_start + comparison(*other).len(),
            };
            let comparator_end = self
                .text
                .closes_after(comparator.range().end().to_usize(), range.1);
            let splice = (previous_end, comparator_start);
            let swap = match operator {
                CmpOp::Lt => Some(CmpOp::LtE),
                CmpOp::LtE => Some(CmpOp::Lt),
                CmpOp::Gt => Some(CmpOp::GtE),
                CmpOp::GtE => Some(CmpOp::Gt),
                CmpOp::Eq => Some(CmpOp::NotEq),
                CmpOp::NotEq => Some(CmpOp::Eq),
                _ => None,
            };
            if let Some(new) = swap {
                let before = self.mutants.len();
                self.push(
                    "EqualityOperator",
                    comparison(new),
                    (operator_start, comparator_end),
                    splice,
                    format!(" {} ", comparison(new)),
                    &[(operator_start, operator_end)],
                );
                self.note_operator(before, operator_start, comparison(*operator));
            }
            let keyword = match operator {
                CmpOp::Is => Some(CmpOp::IsNot),
                CmpOp::IsNot => Some(CmpOp::Is),
                CmpOp::In => Some(CmpOp::NotIn),
                CmpOp::NotIn => Some(CmpOp::In),
                _ => None,
            };
            if let Some(new) = keyword {
                let before = self.mutants.len();
                self.push(
                    "EqualityOperator",
                    comparison(new),
                    (operator_start, operator_end),
                    splice,
                    format!(" {} ", comparison(new)),
                    &[(operator_start, operator_end)],
                );
                self.note_operator(before, operator_start, comparison(*operator));
            }
            self.visit_expr(comparator);
            previous = comparator;
        }
    }

    fn string_part(&mut self, range: TextRange) {
        let at = range_of(range);
        let value = self.text.slice(range);
        let Some(quote_at) = value.find(['"', '\'']) else {
            return;
        };
        let (prefix, rest) = value.split_at(quote_at);
        if rest.starts_with("\"\"\"") || rest.starts_with("'''") || rest.len() < 2 {
            return;
        }
        let quote = &rest[..1];
        let body = &rest[1..rest.len() - 1];
        let raw = prefix.contains(['r', 'R']);
        for (descriptor, new_body) in [
            ("XX", format!("XX{body}XX")),
            ("lower", cased(body, raw, false)),
            ("upper", cased(body, raw, true)),
        ] {
            if new_body == body {
                continue;
            }
            let replacement = format!("{prefix}{quote}{new_body}{quote}");
            self.push("StringLiteral", descriptor, at, at, replacement, &[at]);
        }
    }

    fn identifier(&mut self, identifier: &ast::Identifier) {
        // A dotted import name is one identifier to ruff, a name per part
        // to libcst.
        let start = identifier.range.start().to_usize();
        let mut offset = 0;
        for part in identifier.as_str().split('.') {
            if part == "deepcopy" {
                let at = (start + offset, start + offset + part.len());
                self.push("Name", "copy", at, at, "copy", &[at]);
            }
            offset += part.len() + 1;
        }
    }
}

/// A string body with each run of characters lowered or raised as mutmut's
/// patterns pick them: escapes kept (a raw string's backslash and the
/// character after it).
fn cased(body: &str, raw: bool, upper: bool) -> String {
    let convert = |text: &str| {
        if upper {
            text.to_uppercase()
        } else {
            text.to_lowercase()
        }
    };
    let characters = body.char_indices().collect::<Vec<_>>();
    let mut out = String::with_capacity(body.len());
    let mut index = 0;
    while index < characters.len() {
        let (at, character) = characters[index];
        if character == '\\' {
            let escape = if raw { 1 } else { escape_length(&body[at..]) };
            if escape == 0 {
                // An escape the patterns cannot match: the backslash alone,
                // and the character after it never starts a run.
                out.push('\\');
                if let Some((_, next)) = characters.get(index + 1) {
                    out.push(*next);
                }
                index += 2;
                continue;
            }
            let end = at + escape;
            out.push_str(&body[at..end.min(body.len())]);
            while index < characters.len() && characters[index].0 < end {
                index += 1;
            }
            if raw {
                // The character after a raw backslash is never the start of
                // a run: a run may not follow a backslash.
                if let Some((_, next)) = characters.get(index) {
                    out.push(*next);
                    index += 1;
                }
            }
            continue;
        }
        let run_end = characters[index..]
            .iter()
            .position(|(_, character)| *character == '\\')
            .map_or(characters.len(), |offset| index + offset);
        let start = characters[index].0;
        let end = characters.get(run_end).map_or(body.len(), |(at, _)| *at);
        out.push_str(&convert(&body[start..end]));
        index = run_end;
    }
    out
}

/// The length of the escape at the start of `text` (a backslash) that
/// mutmut keeps as written, or 0 where its pattern matches none.
fn escape_length(text: &str) -> usize {
    let bytes = text.as_bytes();
    let hex = |count: usize| {
        bytes.len() > count + 1 && bytes[2..2 + count].iter().all(u8::is_ascii_hexdigit)
    };
    match bytes.get(1) {
        Some(b'x') if hex(2) => 4,
        Some(b'u') if hex(4) => 6,
        Some(b'U') if hex(8) => 10,
        Some(b'N') if bytes.get(2) == Some(&b'{') => match text[3..].find('}') {
            Some(close) if close > 0 => close + 4,
            _ => text[1..]
                .chars()
                .next()
                .map_or(0, |next| 1 + next.len_utf8()),
        },
        Some(b'\n') | None => 0,
        Some(_) => 1 + text[1..].chars().next().map_or(0, char::len_utf8),
    }
}

/// mutmut's number mutant of a literal: its value plus one, as Python's
/// `repr` writes it, and whether that is the same value (a float too large
/// to change by one). `None` where there is none (a float that becomes
/// infinite).
pub(crate) fn number_mutant(literal: &str) -> Option<(String, bool)> {
    let clean = literal.replace('_', "");
    if let Some(imaginary) = clean.strip_suffix(['j', 'J']) {
        let value = imaginary.parse::<f64>().ok()?;
        let mutated = value + 1.0;
        if mutated.is_infinite() {
            return None;
        }
        let shown = python_float_repr(mutated);
        let shown = shown.strip_suffix(".0").unwrap_or(&shown);
        return Some((format!("{shown}j"), mutated == value));
    }
    let lower = clean.to_ascii_lowercase();
    let integer = |digits: &str, radix: u32| integer_plus_one(digits, radix);
    if let Some(digits) = lower.strip_prefix("0x") {
        return Some((integer(digits, 16)?, false));
    }
    if let Some(digits) = lower.strip_prefix("0o") {
        return Some((integer(digits, 8)?, false));
    }
    if let Some(digits) = lower.strip_prefix("0b") {
        return Some((integer(digits, 2)?, false));
    }
    if lower.bytes().all(|byte| byte.is_ascii_digit()) {
        return Some((integer(&lower, 10)?, false));
    }
    let value = lower.parse::<f64>().ok()?;
    let mutated = value + 1.0;
    if mutated.is_infinite() {
        return None;
    }
    Some((python_float_repr(mutated), mutated == value))
}

/// `digits` in `radix`, plus one, in decimal: Python's integers have no
/// bound.
fn integer_plus_one(digits: &str, radix: u32) -> Option<String> {
    // Little-endian limbs of 10^9.
    const BASE: u64 = 1_000_000_000;
    let mut limbs: Vec<u64> = vec![0];
    for character in digits.chars() {
        let digit = u64::from(character.to_digit(radix)?);
        let mut carry = digit;
        for limb in &mut limbs {
            let value = *limb * u64::from(radix) + carry;
            *limb = value % BASE;
            carry = value / BASE;
        }
        while carry > 0 {
            limbs.push(carry % BASE);
            carry /= BASE;
        }
    }
    let mut carry = 1;
    for limb in &mut limbs {
        let value = *limb + carry;
        *limb = value % BASE;
        carry = value / BASE;
        if carry == 0 {
            break;
        }
    }
    if carry > 0 {
        limbs.push(carry);
    }
    let mut text = limbs.last().copied().unwrap_or(0).to_string();
    for limb in limbs.iter().rev().skip(1) {
        text.push_str(&format!("{limb:09}"));
    }
    Some(text)
}

/// `repr(value)` of a finite Python float: the shortest digits that read
/// back the same, in positional notation from 1e-4 to 1e16 and in
/// scientific notation outside it.
pub(crate) fn python_float_repr(value: f64) -> String {
    let scientific = format!("{:e}", value.abs());
    let (mantissa, exponent) = scientific.split_once('e').expect("{:e} writes an exponent");
    let exponent = exponent.parse::<i32>().expect("an integer exponent");
    let digits = mantissa.replace('.', "");
    let sign = if value.is_sign_negative() { "-" } else { "" };
    if (-4..16).contains(&exponent) {
        let point = exponent + 1;
        let shown = if point <= 0 {
            format!("0.{}{digits}", "0".repeat((-point) as usize))
        } else if digits.len() as i32 <= point {
            format!(
                "{digits}{}.0",
                "0".repeat((point - digits.len() as i32) as usize)
            )
        } else {
            let (whole, fraction) = digits.split_at(point as usize);
            format!("{whole}.{fraction}")
        };
        return format!("{sign}{shown}");
    }
    let (first, rest) = digits.split_at(1);
    let fraction = if rest.is_empty() {
        String::new()
    } else {
        format!(".{rest}")
    };
    let exponent_sign = if exponent < 0 { '-' } else { '+' };
    format!(
        "{sign}{first}{fraction}e{exponent_sign}{:02}",
        exponent.abs()
    )
}

impl<'a> SourceOrderVisitor<'a> for Walker<'_, '_> {
    fn visit_stmt(&mut self, statement: &'a Stmt) {
        match statement {
            Stmt::FunctionDef(function) if self.skips_function(function) => return,
            Stmt::ClassDef(class) => {
                if self.skips_class(class) {
                    return;
                }
                self.note_enum_hooks(class);
            }
            _ => {}
        }
        self.bounds.push(range_of(statement.range()));
        self.statement_mutants(statement);
        walk_stmt(self, statement);
        self.bounds.pop();
    }

    fn visit_expr(&mut self, expression: &'a Expr) {
        if let Expr::Call(call) = expression
            && let Expr::Name(name) = &*call.func
            && NEVER_MUTATE_FUNCTION_CALLS.contains(&name.id.as_str())
        {
            return;
        }
        self.expression_mutants(expression);
        if let Expr::Compare(compare) = expression {
            self.comparison(compare);
            return;
        }
        walk_expr(self, expression);
    }

    fn visit_arguments(&mut self, arguments: &'a ast::Arguments) {
        self.bounds.push((
            arguments.range.start().to_usize() + 1,
            arguments.range.end().to_usize() - 1,
        ));
        ruff_python_ast::visitor::source_order::walk_arguments(self, arguments);
        self.bounds.pop();
    }

    fn visit_parameter_with_default(&mut self, parameter: &'a ast::ParameterWithDefault) {
        // A default runs when the function is defined: only one that cannot
        // raise once mutated is mutated, and the parameter is otherwise left
        // alone whole.
        if let Some(default) = &parameter.default
            && !matches!(
                &**default,
                Expr::Name(_)
                    | Expr::BooleanLiteral(_)
                    | Expr::NoneLiteral(_)
                    | Expr::NumberLiteral(_)
                    | Expr::StringLiteral(_)
                    | Expr::BytesLiteral(_)
                    | Expr::FString(_)
            )
        {
            return;
        }
        ruff_python_ast::visitor::source_order::walk_parameter_with_default(self, parameter);
    }

    fn visit_annotation(&mut self, _annotation: &'a Expr) {}

    fn visit_decorator(&mut self, _decorator: &'a ast::Decorator) {}

    fn visit_pattern(&mut self, pattern: &'a ast::Pattern) {
        if let ast::Pattern::MatchSingleton(singleton) = pattern {
            let at = range_of(singleton.range);
            match singleton.value {
                ast::Singleton::True => {
                    self.push("BooleanLiteral", "False", at, at, "False", &[at])
                }
                ast::Singleton::False => self.push("BooleanLiteral", "True", at, at, "True", &[at]),
                ast::Singleton::None => {}
            }
        }
        walk_pattern(self, pattern);
    }

    fn visit_string_literal(&mut self, literal: &'a ast::StringLiteral) {
        self.string_part(literal.range);
    }

    fn visit_bytes_literal(&mut self, literal: &'a ast::BytesLiteral) {
        self.string_part(literal.range);
    }

    fn visit_identifier(&mut self, identifier: &'a ast::Identifier) {
        self.identifier(identifier);
    }
}

/// The statement and function points of a file, by range, as the
/// instrumenter numbers them: what a mutant's reach is read from.
struct Points {
    statements: Vec<((usize, usize), String)>,
    functions: Vec<((usize, usize), String)>,
    /// Each function's range and the first statement of its body that runs.
    firsts: Vec<((usize, usize), String)>,
}

impl Points {
    fn of(file: &str, suite: &[Stmt]) -> Self {
        struct Collector<'f> {
            file: &'f str,
            points: Points,
        }
        impl<'a> SourceOrderVisitor<'a> for Collector<'_> {
            fn visit_body(&mut self, body: &'a [Stmt]) {
                for (index, statement) in body.iter().enumerate() {
                    if crate::python_instrumenter::is_unobservable_statement(statement, index == 0)
                    {
                        continue;
                    }
                    self.visit_stmt(statement);
                }
            }

            fn visit_stmt(&mut self, statement: &'a Stmt) {
                let range = statement.range();
                self.points.statements.push((
                    range_of(range),
                    crate::python_instrumenter::stable_id(self.file, "statement", range, ""),
                ));
                if let Stmt::FunctionDef(function) = statement {
                    if let Some((_, first)) =
                        function.body.iter().enumerate().find(|(index, body)| {
                            !crate::python_instrumenter::is_unobservable_statement(
                                body,
                                *index == 0,
                            )
                        })
                    {
                        self.points.firsts.push((
                            range_of(range),
                            crate::python_instrumenter::stable_id(
                                self.file,
                                "statement",
                                first.range(),
                                "",
                            ),
                        ));
                    }
                    self.points.functions.push((
                        range_of(range),
                        crate::python_instrumenter::stable_id(
                            self.file,
                            "function",
                            range,
                            function.name.as_str(),
                        ),
                    ));
                }
                walk_stmt(self, statement);
            }

            fn visit_expr(&mut self, expression: &'a Expr) {
                if let Expr::Lambda(lambda) = expression {
                    self.points.functions.push((
                        range_of(lambda.range),
                        crate::python_instrumenter::stable_id(
                            self.file,
                            "function",
                            lambda.range,
                            "<lambda>",
                        ),
                    ));
                }
                walk_expr(self, expression);
            }
        }
        let mut collector = Collector {
            file,
            points: Points {
                statements: Vec::new(),
                functions: Vec::new(),
                firsts: Vec::new(),
            },
        };
        collector.visit_body(suite);
        collector.points
    }

    /// The innermost point of `points` that holds `span`.
    fn innermost(points: &[((usize, usize), String)], span: (usize, usize)) -> Option<String> {
        points
            .iter()
            .filter(|((start, end), _)| *start <= span.0 && span.1 <= *end)
            .min_by_key(|((start, end), _)| end - start)
            .map(|(_, id)| id.clone())
    }
}

/// A file's mutants as `mutants.json` lists them, each linked to the
/// statement and function points of the run's manifest that hold it, and
/// the file's point ids.
pub(crate) fn listed(
    file: &str,
    source: &str,
) -> Result<(Vec<crate::mutant_list::CandidateMutant>, BTreeSet<String>), PythonInstrumenterError> {
    let obligations = crate::python_instrumenter::build_python_obligations(file, source)?;
    let recorded = obligations
        .manifest
        .points
        .iter()
        .map(|point| point.id.clone())
        .collect::<BTreeSet<_>>();
    let parsed =
        parse_module(source).map_err(|error| PythonInstrumenterError::Parse(error.to_string()))?;
    let points = Points::of(file, parsed.suite());
    let enumeration = enumerate(source)?;
    let text = Text::new(source, &[]);
    let known = |id: Option<String>| id.filter(|id| recorded.contains(id));
    let mutants = enumeration
        .mutants
        .iter()
        .map(|mutant| crate::mutant_list::CandidateMutant {
            id: String::new(),
            mutator: mutant.mutator.to_owned(),
            replacement: mutant.descriptor.clone(),
            start: text.position(mutant.span.0),
            end: text.position(mutant.span.1),
            original: source[mutant.span.0..mutant.span.1].to_owned(),
            operator: mutant
                .operator
                .map(|(at, text)| crate::mutant_list::MutantOperator {
                    text: text.to_owned(),
                    at: at - mutant.span.0,
                }),
            value_used: false,
            // A parameter's default is used where its function is called:
            // reached by the function's first statement, which runs then.
            statement: known(match &mutant.default_of {
                Some(_) => Points::innermost(&points.firsts, mutant.span),
                None => Points::innermost(&points.statements, mutant.span),
            }),
            function: known(Points::innermost(&points.functions, mutant.span)),
            decision: None,
            branch: None,
            chain: None,
            compare: None,
            probe: None,
            arrow: None,
            equivalent: mutant.same_value.then(|| crate::mutant_list::StaticNote {
                rule: SAME_NUMBER.to_owned(),
                reason: format!(
                    "`{}` and `{}` are the same float: the literal plus one rounds back to it",
                    &source[mutant.span.0..mutant.span.1],
                    mutant.descriptor
                ),
            }),
            untestable: None,
            is_static: false,
            unplaceable: None,
        })
        .map(|mut mutant| {
            mutant.id = format!(
                "{file}:{}:{}-{}:{}:{}:{}",
                mutant.start.0,
                mutant.start.1,
                mutant.end.0,
                mutant.end.1,
                mutant.mutator,
                mutant.replacement
            );
            mutant
        })
        .collect();
    Ok((mutants, recorded))
}

/// The static rule of a number literal whose mutant has its value.
pub const SAME_NUMBER: &str = "same-number";

/// The mutants of the Python files in `recorded` (each file's point ids, as
/// the run's manifest has them), read from under `root`. A file whose
/// points are not the run's changed since the run, and is named so.
pub(crate) fn list_mutants(
    root: &std::path::Path,
    recorded: &std::collections::BTreeMap<String, BTreeSet<String>>,
) -> crate::mutant_list::ListedMutants {
    let files = recorded.keys().cloned().collect::<Vec<_>>();
    let listed = crate::threads::parallel_map(&files, |file| {
        let source = std::fs::read_to_string(root.join(file)).ok()?;
        let (mutants, points) = listed(file, &source).ok()?;
        (Some(&points) == recorded.get(file)).then_some(mutants)
    });
    let mut mutants = std::collections::BTreeMap::new();
    let mut changed = Vec::new();
    for (file, listed) in files.into_iter().zip(listed) {
        match listed {
            Some(list) if list.is_empty() => {}
            Some(list) => {
                mutants.insert(file, list);
            }
            None => changed.push(file),
        }
    }
    crate::mutant_list::ListedMutants {
        mutants: crate::mutant_list::MutantsFile::listed(PYTHON, MUTANT_SET, mutants),
        changed,
    }
}

/// The language name lists and plans give Python.
pub const PYTHON: &str = "python";

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/mutmut-parity");

    /// The shortest edit that turns `source` into `mutated`, as the parity
    /// script finds it: what is left between their common start and end.
    fn edit(text: &Text<'_>, source: &str, mutated: &str) -> String {
        let (original, changed) = (source.as_bytes(), mutated.as_bytes());
        let limit = original.len().min(changed.len());
        let mut prefix = 0;
        while prefix < limit && original[prefix] == changed[prefix] {
            prefix += 1;
        }
        while !source.is_char_boundary(prefix) || !mutated.is_char_boundary(prefix) {
            prefix -= 1;
        }
        let mut suffix = 0;
        while suffix < limit - prefix
            && original[original.len() - 1 - suffix] == changed[changed.len() - 1 - suffix]
        {
            suffix += 1;
        }
        while !source.is_char_boundary(original.len() - suffix)
            || !mutated.is_char_boundary(changed.len() - suffix)
        {
            suffix -= 1;
        }
        let (start, end) = (
            text.position(prefix),
            text.position(original.len() - suffix),
        );
        format!(
            "{}:{}-{}:{} {:?}",
            start.0,
            start.1,
            end.0,
            end.1,
            &mutated[prefix..changed.len() - suffix]
        )
    }

    fn rendered(source: &str) -> Vec<String> {
        let enumeration = enumerate(source).expect("the fixture parses");
        let parsed = parse_module(source).unwrap();
        let comments = parsed
            .tokens()
            .iter()
            .filter(|token| token.kind() == TokenKind::Comment)
            .map(Ranged::range)
            .collect::<Vec<_>>();
        let text = Text::new(source, &comments);
        enumeration
            .mutants
            .iter()
            .map(|mutant| {
                let mutated = format!(
                    "{}{}{}",
                    &source[..mutant.splice.0],
                    mutant.replacement,
                    &source[mutant.splice.1..]
                );
                let (start, end) = (text.position(mutant.span.0), text.position(mutant.span.1));
                format!(
                    "{}__mutmut_{} {}:{}-{}:{} {} {} edit {}",
                    enumeration.units[mutant.unit].key,
                    mutant.ordinal,
                    start.0,
                    start.1,
                    end.0,
                    end.1,
                    mutant.mutator,
                    mutant.descriptor,
                    edit(&text, source, &mutated)
                )
            })
            .collect()
    }

    fn golden(path: &std::path::Path) -> Vec<String> {
        let value: Value =
            serde_json::from_slice(&std::fs::read(path).expect("a golden")).expect("JSON");
        assert_eq!(value["mutantSet"], MUTANT_SET);
        let pair = |value: &Value| format!("{}:{}", value[0], value[1]);
        value["mutants"]
            .as_array()
            .expect("mutants")
            .iter()
            .map(|mutant| {
                format!(
                    "{} {}-{} {} {} edit {}-{} {:?}",
                    mutant["mutmut"].as_str().unwrap(),
                    pair(&mutant["start"]),
                    pair(&mutant["end"]),
                    mutant["mutator"].as_str().unwrap(),
                    mutant["replacement"].as_str().unwrap(),
                    pair(&mutant["edit"]["start"]),
                    pair(&mutant["edit"]["end"]),
                    mutant["edit"]["text"].as_str().unwrap()
                )
            })
            .collect()
    }

    /// Every fixture's mutants are mutmut's: the same mutants, in the same
    /// order, with the same spans, numbers and edits.
    #[test]
    fn the_mutants_are_mutmuts() {
        assert!(compare_directory(std::path::Path::new(FIXTURES)) >= 5);
    }

    /// The same over a directory of real projects' files and the goldens
    /// `scripts/mutmut-parity.py --write` made of them:
    /// `SUPERCOV_MUTMUT_PARITY=<dir> cargo test -p supercov-engine --lib
    /// python_mutants -- --ignored`.
    #[test]
    #[ignore = "needs goldens written from a corpus"]
    fn a_corpus_s_mutants_are_mutmuts() {
        let directory = std::env::var("SUPERCOV_MUTMUT_PARITY").expect("a directory");
        assert!(compare_directory(std::path::Path::new(&directory)) > 0);
    }

    fn compare_directory(directory: &std::path::Path) -> usize {
        let mut checked = 0;
        let mut failures = Vec::new();
        let mut entries = std::fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        entries.sort();
        for path in entries {
            if path.extension().and_then(|e| e.to_str()) != Some("py") {
                continue;
            }
            let source = std::fs::read_to_string(&path).unwrap();
            let ours = rendered(&source);
            let theirs = golden(&path.with_extension("py.mutmut.json"));
            let mut first = None;
            for (index, (ours, theirs)) in ours.iter().zip(&theirs).enumerate() {
                if ours != theirs {
                    first = Some(format!("#{index}\n ours:   {ours}\n mutmut: {theirs}"));
                    break;
                }
            }
            if first.is_some() || ours.len() != theirs.len() {
                failures.push(format!(
                    "{}: {} mutants, mutmut {}; first difference {}",
                    path.display(),
                    ours.len(),
                    theirs.len(),
                    first.unwrap_or_default()
                ));
            }
            checked += 1;
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
        checked
    }
}
