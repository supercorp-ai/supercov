//! The mutants the mutant gem (0.17.1, its default light operators) makes
//! for one Ruby file, enumerated from the source without running mutant.
//!
//! mutant mutates methods: each `def` and `def self.x` is a subject, and its
//! mutations are whole methods, each with one node replaced. This module
//! converts each method to the parser tree mutant reads (`tree`), runs a
//! port of mutant's mutators on it (`mutators`), keeps the mutations mutant
//! keeps (those Unparser prints and parses back to the same tree), and
//! locates each: the smallest node whose replacement it is.
//! `scripts/mutant-parity.rb` prints mutant's own list the same way, and the
//! tests compare the two on the fixtures in `tests/fixtures/mutant-parity`.
//!
//! # Ids
//!
//! A mutant is `file:startLine:startCol-endLine:endCol:Mutator:descriptor`:
//! the located node's span (1-based lines, 1-based columns in characters,
//! the end exclusive), the mutant mutator class that handles that node
//! (`Send`, `If`, `Integer`, ...), and the descriptor: `removed` where the
//! node is taken out (a statement, a branch, a body), else the replacement
//! code on one line, with anything past 40 characters replaced by a short
//! hash of the whole.

mod mutators;
pub(crate) mod regexp;
pub(crate) mod tree;

use std::rc::Rc;

use ruby_prism::{Node, Visit};

use self::tree::{Child, Converter, N, syntax};

/// The mutant gem's version these mutants follow.
pub const MUTANT_SET: &str = "mutant@0.17.1";

/// One mutant of a Ruby file.
#[derive(Debug, Clone)]
// The located node, its replacement and the edit are what the parity tests
// compare.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct RubyMutant {
    /// The located node's byte span.
    pub start: usize,
    pub end: usize,
    /// The located node's parser type.
    pub node: &'static str,
    /// The mutant mutator class of that node.
    pub mutator: &'static str,
    /// The replacing node, or none where the node is removed.
    pub replacement: Option<Rc<N>>,
    /// The change to the source that makes it.
    pub edit: Edit,
    /// The method as written, and as the mutant makes it.
    pub method: Rc<N>,
    pub mutated: Rc<N>,
    /// The descriptor in the mutant's id.
    pub descriptor: String,
}

/// A file's mutants, and the methods whose mutants could not be listed.
#[derive(Debug, Default)]
pub(crate) struct Enumeration {
    pub mutants: Vec<RubyMutant>,
    /// Methods holding syntax the conversion does not cover yet, with why.
    pub skipped: Vec<(String, String)>,
    /// Mutations the port made but could not print.
    pub unprintable: usize,
}

/// mutant's mutator class for a node type.
pub(crate) fn mutator_name(ty: &str) -> &'static str {
    match ty {
        "send" | "csend" => "Send",
        "int" => "Integer",
        "float" => "Float",
        "str" => "String",
        "sym" => "Symbol",
        "dstr" | "dsym" | "xstr" => "DynamicLiteral",
        "regexp" => "Regex",
        "regopt" => "Regopt",
        "array" => "Array",
        "hash" => "Hash",
        "pair" => "Pair",
        "irange" | "erange" => "Range",
        "true" | "false" => "Boolean",
        "rational" => "Rational",
        "complex" => "Complex",
        "if" => "If",
        "and" | "or" => "Binary",
        "begin" => "Begin",
        "kwbegin" => "Kwbegin",
        "block" => "Block",
        "numblock" => "Numblock",
        "itblock" => "Itblock",
        "block_pass" => "BlockPass",
        "def" | "defs" => "Define",
        "args" => "Arguments",
        "arg" | "kwarg" | "optarg" | "kwoptarg" => "Argument",
        "procarg0" => "ProcargZero",
        "lvar" | "ivar" | "gvar" | "cvar" | "self" => "NamedValue",
        "lvasgn" | "ivasgn" | "gvasgn" | "cvasgn" => "VariableAssignment",
        "casgn" => "ConstantAssignment",
        "op_asgn" => "OpAsgn",
        "or_asgn" => "OrAsgn",
        "and_asgn" => "AndAsgn",
        "index" | "indexasgn" => "Index",
        "return" => "Return",
        "next" => "Next",
        "break" => "Break",
        "yield" => "Yield",
        "super" => "Super",
        "zsuper" => "ZSuper",
        "case" => "Case",
        "when" => "When",
        "case_match" => "CaseMatch",
        "in_pattern" => "InPattern",
        "if_guard" | "unless_guard" => "Guard",
        "match_alt" => "MatchAlt",
        "match_pattern_p" => "MatchPatternP",
        "while" | "until" | "while_post" | "until_post" => "ConditionalLoop",
        "rescue" => "Rescue",
        "resbody" => "Resbody",
        "ensure" => "Ensure",
        "splat" => "Splat",
        "const" => "Const",
        "defined?" => "Defined",
        "kwargs" => "Kwargs",
        "masgn" => "Masgn",
        "nth_ref" => "NthRef",
        _ => "Generic",
    }
}

/// Every method's mutants, in the order mutant lists them.
pub(crate) fn enumerate(source: &[u8]) -> Enumeration {
    let result = ruby_prism::parse(source);
    if result.errors().next().is_some() {
        return Enumeration::default();
    }
    let mut finder = Methods::default();
    finder.visit(&result.node());
    let mut enumeration = Enumeration::default();
    for def in finder.defs {
        let def = def.as_def_node().expect("a def");
        let mut converter = Converter::new(source);
        let line = line_of(source, def.location().start_offset());
        let subject = format!("{}:{line}", String::from_utf8_lossy(def.name().as_slice()));
        let tree = match converter.method(&def) {
            Ok(tree) => tree,
            Err(reason) => {
                enumeration.skipped.push((subject, reason.0));
                continue;
            }
        };
        method_mutants(source, &tree, &mut enumeration);
    }
    enumeration
}

#[derive(Default)]
struct Methods<'pr> {
    defs: Vec<Node<'pr>>,
}

impl<'pr> Visit<'pr> for Methods<'pr> {
    fn visit_def_node(&mut self, node: &ruby_prism::DefNode<'pr>) {
        self.defs.push(node.as_node());
        ruby_prism::visit_def_node(self, node);
    }
}

fn line_of(source: &[u8], offset: usize) -> usize {
    1 + source[..offset]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count()
}

/// The 1-based line and 1-based character column of a byte offset.
pub(crate) fn position(source: &[u8], offset: usize) -> (u32, u32) {
    let line_start = source[..offset]
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |index| index + 1);
    let column = String::from_utf8_lossy(&source[line_start..offset])
        .chars()
        .count();
    (line_of(source, offset) as u32, column as u32 + 1)
}

fn method_mutants(source: &[u8], tree: &Rc<N>, into: &mut Enumeration) {
    for mutated in mutators::mutate(tree, None) {
        if !validates(&mutated) {
            continue;
        }
        let (located, replacement) = localize(tree, &mutated);
        let Some((start, end)) = located.span else {
            continue;
        };
        // A mutant whose code cannot be written in place (one in a line
        // holding a heredoc's opening) is still a mutant: its descriptor is
        // its replacement's code.
        let edit = edit(source, tree, &mutated);
        let descriptor = match (&replacement, &edit) {
            (None, _) => "removed".to_owned(),
            (Some(_), Some(edit)) if edit.start == start && edit.end == end => {
                descriptor(&edit.text)
            }
            (Some(replacement), _) => descriptor(
                &print::one_line(source, tree, replacement)
                    .unwrap_or_else(|| tree::sexp(replacement)),
            ),
        };
        if edit.is_none() {
            into.unprintable += 1;
        }
        let edit = edit.unwrap_or(Edit {
            start,
            end,
            text: String::new(),
        });
        into.mutants.push(RubyMutant {
            start,
            end,
            node: located.ty,
            mutator: mutator_name(located.ty),
            replacement,
            edit,
            descriptor,
            method: tree.clone(),
            mutated: mutated.clone(),
        });
    }
}

/// The change to the source that makes a mutant: its span's code replaced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Edit {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

/// Where code can be left out: a node removed there leaves valid code.
fn may_be_empty(parent: &N) -> bool {
    match parent.ty {
        "if" => !parent.has(syntax::TERNARY) && !parent.has(syntax::MODIFIER),
        "while" | "until" => !parent.has(syntax::MODIFIER),
        "def" | "defs" => !parent.has(syntax::ENDLESS),
        _ => true,
    }
}

/// The pairs of original and mutated nodes from the method down to the
/// located one (whose mutated side is none where it is removed).
pub(crate) fn path(tree: &Rc<N>, mutated: &Rc<N>) -> Vec<(Rc<N>, Option<Rc<N>>)> {
    // The pairs of original and mutated nodes from the method down to the
    // located one.
    let mut path: Vec<(Rc<N>, Option<Rc<N>>)> = vec![(tree.clone(), Some(mutated.clone()))];
    loop {
        let (original, mutated) = path.last().cloned().expect("a pair");
        let Some(mutated) = mutated else { break };
        let mut next = None;
        if original.ty == mutated.ty && original.children.len() == mutated.children.len() {
            let differing: Vec<usize> = (0..original.children.len())
                .filter(|index| original.children[*index] != mutated.children[*index])
                .collect();
            if differing.len() == 1 {
                let index = differing[0];
                match (&original.children[index], &mutated.children[index]) {
                    (Child::Node(from), Child::Node(to)) => {
                        next = Some((from.clone(), Some(to.clone())))
                    }
                    (Child::Node(from), Child::Nil)
                        if body_slot(original.ty, index, original.children.len()) =>
                    {
                        next = Some((from.clone(), None));
                    }
                    _ => {}
                }
            }
        }
        if next.is_none()
            && matches!(original.ty, "begin" | "kwbegin")
            && mutated.ty == original.ty
            && mutated.children.len() + 1 == original.children.len()
        {
            let index = (0..original.children.len())
                .find(|index| original.children.get(*index) != mutated.children.get(*index))
                .unwrap_or(original.children.len() - 1);
            let mut dropped = original.children.clone();
            dropped.remove(index);
            if dropped == mutated.children
                && let Some(removed) = original.children[index].node()
            {
                next = Some((removed.clone(), None));
            }
        }
        match next {
            Some(pair) => path.push(pair),
            None => break,
        }
    }
    path
}

/// The edit that turns `tree` into `mutated`: at the located node, or, where
/// that node's slot cannot hold the replacement's code (a `%w` word, a
/// ternary's branch removed), at the nearest node around it that can.
pub(crate) fn edit(source: &[u8], tree: &Rc<N>, mutated: &Rc<N>) -> Option<Edit> {
    let mut path = path(tree, mutated);
    while let Some((original, mutated)) = path.pop() {
        let Some((start, end)) = original.span else {
            continue;
        };
        let text = match &mutated {
            None if print::holds_heredoc(source, &original) => continue,
            None => {
                let parent = &path.last()?.0;
                if may_be_empty(parent) {
                    String::new()
                } else {
                    "nil".to_owned()
                }
            }
            Some(mutated) => match print::replacement(source, tree, &original, mutated) {
                Some(text) => text,
                None => continue,
            },
        };
        return Some(Edit { start, end, text });
    }
    None
}

/// The replacement as a descriptor: on one line, and past 40 characters,
/// its start and a hash of the whole.
fn descriptor(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= 40 {
        return flat;
    }
    let digest = crate::stable_ids::digest(&[&flat]);
    let start: String = flat.chars().take(32).collect();
    format!("{start}…{}", &digest[..8.min(digest.len())])
}

/// Slots where a node may become nil: an empty body.
fn body_slot(ty: &str, index: usize, length: usize) -> bool {
    let slots: &[isize] = match ty {
        "if" => &[1, 2],
        "while" | "until" => &[1],
        "block" | "numblock" | "itblock" | "def" | "in_pattern" | "resbody" => &[2],
        "defs" => &[3],
        "ensure" => &[0, 1],
        "case" | "case_match" => &[-1],
        _ => &[],
    };
    slots.iter().any(|slot| {
        let resolved = if *slot < 0 {
            length as isize + slot
        } else {
            *slot
        };
        resolved == index as isize
    })
}

/// The smallest node of `original` that `mutated` replaces, and its
/// replacement (none where it is removed). See `scripts/mutant-parity.rb`.
pub(crate) fn localize(original: &Rc<N>, mutated: &Rc<N>) -> (Rc<N>, Option<Rc<N>>) {
    let mut original = original.clone();
    let mut mutated = mutated.clone();
    loop {
        if original.ty == mutated.ty && original.children.len() == mutated.children.len() {
            let differing: Vec<usize> = (0..original.children.len())
                .filter(|index| original.children[*index] != mutated.children[*index])
                .collect();
            if differing.len() == 1 {
                let index = differing[0];
                match (&original.children[index], &mutated.children[index]) {
                    (Child::Node(from), Child::Node(to)) => {
                        let (from, to) = (from.clone(), to.clone());
                        original = from;
                        mutated = to;
                        continue;
                    }
                    (Child::Node(from), Child::Nil)
                        if body_slot(original.ty, index, original.children.len()) =>
                    {
                        return (from.clone(), None);
                    }
                    _ => {}
                }
            }
        }
        if matches!(original.ty, "begin" | "kwbegin")
            && mutated.ty == original.ty
            && mutated.children.len() + 1 == original.children.len()
        {
            let index = (0..original.children.len())
                .find(|index| original.children.get(*index) != mutated.children.get(*index))
                .unwrap_or(original.children.len() - 1);
            let mut dropped = original.children.clone();
            dropped.remove(index);
            if dropped == mutated.children
                && let Some(removed) = original.children[index].node()
            {
                return (removed.clone(), None);
            }
        }
        return (original, Some(mutated));
    }
}

/// Whether Unparser prints `method` and parses it back to the same tree,
/// which is when mutant keeps a mutation. The way back fails where a local
/// variable is read before any assignment to it: parsed again, the read is
/// a method call.
pub(crate) fn validates(method: &N) -> bool {
    let mut scope = Scope::default();
    scope.walk(method) && !misplaced(method, None)
}

/// Statement lists and bodies: where `a || b and c` reads as written.
const STATEMENT_PARENTS: &[&str] = &[
    "begin",
    "kwbegin",
    "def",
    "defs",
    "block",
    "numblock",
    "itblock",
    "if",
    "while",
    "until",
    "while_post",
    "until_post",
    "when",
    "in_pattern",
    "resbody",
    "rescue",
    "ensure",
    "case",
    "case_match",
    "for",
];

/// Whether a node parses back as another where Unparser puts it: a regexp
/// literal as a condition is a match against `$_`, and `(a || b) && c`
/// prints with `and`, which only a statement holds.
fn misplaced(node: &N, parent: Option<&N>) -> bool {
    fn holds_regexp(condition: &N) -> bool {
        match condition.ty {
            "regexp" => true,
            "and" | "or" => condition
                .children
                .iter()
                .filter_map(Child::node)
                .any(|operand| holds_regexp(operand)),
            "begin" if condition.children.len() == 1 => {
                condition.node(0).is_some_and(|inner| holds_regexp(inner))
            }
            "send" if condition.child(1).sym() == Some("!") && condition.children.len() == 2 => {
                condition.node(0).is_some_and(|inner| holds_regexp(inner))
            }
            _ => false,
        }
    }
    if matches!(
        node.ty,
        "if" | "while" | "until" | "while_post" | "until_post"
    ) && node
        .node(0)
        .is_some_and(|condition| holds_regexp(condition))
    {
        return true;
    }
    // `begin (a; b) end` is printed `begin a; b end`, whose statements are
    // the `begin`'s own once parsed.
    if node.ty == "kwbegin"
        && node.children.len() == 1
        && node.node(0).is_some_and(|only| only.ty == "begin")
    {
        return true;
    }
    if node.ty == "and"
        && node.node(0).is_some_and(|left| left.ty == "or")
        && parent.is_some_and(|parent| !STATEMENT_PARENTS.contains(&parent.ty))
    {
        return true;
    }
    node.children
        .iter()
        .filter_map(Child::node)
        .any(|child| misplaced(child, Some(node)))
}

/// Binary operators by how tightly they bind, loosest first. Unparser
/// prints a right operand that is itself an operator as a method call
/// (`a - b.+(c)`), which parses back the same, but a left operand as it is:
/// one that binds more loosely than its operator, or as loosely where the
/// operator does not associate to the left, parses back as another tree.
fn precedence(selector: &str) -> Option<u8> {
    Some(match selector {
        "<=>" | "==" | "===" | "!=" | "=~" | "!~" => 1,
        "<" | "<=" | ">" | ">=" => 2,
        "|" | "^" => 3,
        "&" => 4,
        "<<" | ">>" => 5,
        "+" | "-" => 6,
        "*" | "/" | "%" => 7,
        "**" => 9,
        _ => return None,
    })
}

fn operator(value: &N) -> Option<u8> {
    if value.ty != "send" || value.children.len() != 3 || value.child(0).is_nil() {
        return None;
    }
    precedence(value.child(1).sym()?)
}

/// Whether Unparser prints `value` so that it parses back as another tree.
fn misprinted(value: &N) -> bool {
    match value.ty {
        // `a || (b || c)` prints as `a || b || c`.
        "and" | "or" => value.node(1).is_some_and(|right| right.ty == value.ty),
        "send" => {
            let Some(own) = operator(value) else {
                return false;
            };
            let left = value.node(0).and_then(|left| operator(left));
            left.is_some_and(|left| left < own || (left == own && matches!(own, 1 | 9)))
        }
        _ => false,
    }
}

#[derive(Default, Clone)]
struct Scope {
    locals: Vec<String>,
    /// Inside a rescue clause, where `retry` is valid.
    rescuing: bool,
    /// Inside a loop or a block, where `break`, `next` and `redo` are.
    looping: bool,
}

impl Scope {
    fn declare(&mut self, name: &str) {
        if !self.locals.iter().any(|each| each == name) {
            self.locals.push(name.to_owned());
        }
    }

    fn known(&self, name: &str) -> bool {
        self.locals.iter().any(|each| each == name)
    }

    fn walk_children(&mut self, value: &N) -> bool {
        value
            .children
            .iter()
            .filter_map(Child::node)
            .all(|child| self.walk(child))
    }

    fn walk(&mut self, value: &N) -> bool {
        if misprinted(value) {
            return false;
        }
        match value.ty {
            "retry" => self.rescuing,
            "break" | "next" | "redo" => self.looping && self.walk_children(value),
            "resbody" => {
                let saved = self.rescuing;
                self.rescuing = true;
                let valid = self.walk_children(value);
                self.rescuing = saved;
                valid
            }
            "while" | "until" | "while_post" | "until_post" | "for" => {
                let saved = self.looping;
                self.looping = true;
                // A `begin ... end while c` is written body first.
                let valid = if value.ty.ends_with("_post") {
                    value.node(1).is_none_or(|body| self.walk(body))
                        && value.node(0).is_none_or(|condition| self.walk(condition))
                } else {
                    self.walk_children(value)
                };
                self.looping = saved;
                valid
            }
            "def" | "defs" => {
                let mut inner = Scope::default();
                let (arguments, body) = if value.ty == "def" { (1, 2) } else { (2, 3) };
                if value.ty == "defs"
                    && let Some(receiver) = value.node(0)
                    && !self.walk(receiver)
                {
                    return false;
                }
                value
                    .node(arguments)
                    .is_none_or(|arguments| inner.walk(arguments))
                    && value.node(body).is_none_or(|body| inner.walk(body))
            }
            "class" | "module" | "sclass" => {
                let mut inner = Scope::default();
                inner.walk_children(value)
            }
            "block" | "numblock" | "itblock" => {
                if let Some(call) = value.node(0)
                    && !self.walk(call)
                {
                    return false;
                }
                let mut inner = self.clone();
                inner.looping = true;
                inner.rescuing = false;
                match value.child(1) {
                    Child::Int(count) => {
                        for number in 1..=*count {
                            inner.declare(&format!("_{number}"));
                        }
                    }
                    Child::Sym(_) => inner.declare("it"),
                    Child::Node(arguments) if !inner.walk(arguments) => return false,
                    _ => {}
                }
                value.node(2).is_none_or(|body| inner.walk(body))
            }
            "arg" | "restarg" | "kwarg" | "kwrestarg" | "blockarg" | "shadowarg" | "match_var" => {
                if let Some(name) = value.child(0).sym() {
                    self.declare(name);
                }
                true
            }
            "optarg" | "kwoptarg" => {
                let valid = value.node(1).is_none_or(|default| self.walk(default));
                if let Some(name) = value.child(0).sym() {
                    self.declare(name);
                }
                valid
            }
            "lvasgn" => {
                if let Some(name) = value.child(0).sym() {
                    self.declare(name);
                }
                value.node(1).is_none_or(|assigned| self.walk(assigned))
            }
            "lvar" => value.child(0).sym().is_some_and(|name| self.known(name)),
            "match_with_lvasgn" => {
                let valid = self.walk_children(value);
                if let Some(regexp) = value.node(0)
                    && let Some(body) = mutators::regexp_body(regexp)
                {
                    for name in named_captures(&body) {
                        self.declare(&name);
                    }
                }
                valid
            }
            _ => self.walk_children(value),
        }
    }
}

fn named_captures(body: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = body;
    while let Some(index) = rest.find("(?<") {
        rest = &rest[index + 3..];
        if rest.starts_with('=') || rest.starts_with('!') {
            continue;
        }
        if let Some(end) = rest.find('>') {
            names.push(rest[..end].to_owned());
        }
    }
    names
}

pub(crate) mod print;
pub(crate) mod schemata;

#[cfg(test)]
mod tests;

/// The mutant set's language name in `mutants.json`.
pub const RUBY: &str = "ruby";

/// A Ruby file's mutants as `mutants.json` lists them, each linked to the
/// innermost statement and method points of `obligations` around it.
pub(crate) fn candidates(
    file: &str,
    source: &[u8],
    obligations: &crate::ruby_instrumenter::RubyFileObligations,
) -> Vec<crate::mutant_list::CandidateMutant> {
    use crate::coverage_analysis::PointKind;
    let enumeration = enumerate(source);
    let innermost = |kind: PointKind, start: usize, end: usize| {
        obligations
            .point_spans
            .iter()
            .filter(|(_, (each, from, to))| *each == kind && *from <= start && end <= *to)
            .min_by_key(|(_, (_, from, to))| to - from)
            .map(|(id, _)| id.clone())
    };
    let mut out = Vec::new();
    for (mutant, start, end, descriptor) in described(source, enumeration) {
        out.push(crate::mutant_list::CandidateMutant {
            id: String::new(),
            mutator: mutant.mutator.to_owned(),
            replacement: descriptor,
            start,
            end,
            original: String::from_utf8_lossy(&source[mutant.start..mutant.end]).into_owned(),
            probe: probe_kind(&mutant).and_then(|kind| {
                let site = format!("{}:{}:{}-{}:{}", file, start.0, start.1, end.0, end.1);
                obligations
                    .value_sites
                    .contains(&site)
                    .then(|| format!("{site}#{kind}"))
            }),
            operator: None,
            value_used: false,
            statement: innermost(PointKind::Statement, mutant.start, mutant.end),
            function: innermost(PointKind::Function, mutant.start, mutant.end),
            decision: None,
            branch: None,
            chain: None,
            compare: None,
            arrow: None,
            equivalent: None,
            untestable: None,
            is_static: false,
            unplaceable: None,
        });
    }
    out
}

/// A mutant, its span's start and end positions, and its descriptor.
type Described = (RubyMutant, (u32, u32), (u32, u32), String);

/// Each mutant with its span's positions and its descriptor, made unique
/// within its span and mutator (`#2`, `#3` after the first).
fn described(source: &[u8], enumeration: Enumeration) -> Vec<Described> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for mutant in enumeration.mutants {
        let start = position(source, mutant.start);
        let end = position(source, mutant.end);
        let mut descriptor = mutant.descriptor.clone();
        let mut copy = 2;
        while !seen.insert((start, end, mutant.mutator, descriptor.clone())) {
            descriptor = format!("{}#{copy}", mutant.descriptor);
            copy += 1;
        }
        out.push((mutant, start, end, descriptor));
    }
    out
}

/// The ids of `file`'s mutants, in the order of its list.
pub(crate) fn mutant_ids(file: &str, source: &[u8]) -> Vec<String> {
    described(source, enumerate(source))
        .into_iter()
        .map(|(mutant, start, end, descriptor)| {
            format!(
                "{file}:{}:{}-{}:{}:{}:{descriptor}",
                start.0, start.1, end.0, end.1, mutant.mutator
            )
        })
        .collect()
}

/// The mutants of the Ruby files in `recorded` (each file's point ids, as
/// the run's manifest has them), read from under `root`. A file whose points
/// are not the ones the run recorded changed since the run.
pub(crate) fn list_mutants(
    root: &std::path::Path,
    recorded: &std::collections::BTreeMap<String, std::collections::BTreeSet<String>>,
) -> crate::mutant_list::ListedMutants {
    let files = recorded.keys().cloned().collect::<Vec<_>>();
    let listed = crate::threads::parallel_map(&files, |file| {
        let source = std::fs::read(root.join(file)).ok()?;
        let mut next_probe = 0;
        let obligations =
            crate::ruby_instrumenter::build_ruby_obligations(file, &source, &mut next_probe)
                .ok()?;
        let points = obligations
            .manifest
            .points
            .iter()
            .map(|point| point.id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        (Some(&points) == recorded.get(file)).then(|| candidates(file, &source, &obligations))
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
        mutants: crate::mutant_list::MutantsFile::listed(RUBY, MUTANT_SET, mutants),
        changed,
    }
}

/// Whether `bytes` are the mutant gem's session results
/// (`.mutant/results/<session>.json.gz`, or that JSON unpacked).
pub fn is_mutant_session(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0x1f, 0x8b])
        || serde_json::from_slice::<serde_json::Value>(bytes)
            .is_ok_and(|value| value.get("subject_results").is_some())
}

/// The mutant gem's verdicts from its session results, each matched to
/// Supercov's mutant by the method it makes: mutant records each mutation
/// as the method's code (`mutation_source`), which is parsed and compared
/// with each of Supercov's mutants of that method. `path_of` gives a source
/// path from the project root; `read` reads a file under it. A mutation no
/// mutant of Supercov's makes keeps mutant's identification as its id.
pub fn parse_mutant_session(
    bytes: &[u8],
    path_of: impl Fn(&str) -> String,
    read: impl Fn(&str) -> Option<Vec<u8>>,
) -> Result<Vec<crate::mutants_query::StrykerReportMutant>, String> {
    use std::io::Read;
    let text = if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut text = Vec::new();
        flate2::read::GzDecoder::new(bytes)
            .read_to_end(&mut text)
            .map_err(|error| error.to_string())?;
        text
    } else {
        bytes.to_vec()
    };
    let session: serde_json::Value =
        serde_json::from_slice(&text).map_err(|error| error.to_string())?;
    let subjects = session["subject_results"]
        .as_array()
        .ok_or("it has no `subject_results`; is it mutant's session results?")?;
    // Each file read once: its source and its mutants.
    type Listed = Option<(Vec<u8>, Vec<Described>)>;
    let mut cache: std::collections::HashMap<String, Listed> = Default::default();
    let mut out = Vec::new();
    for subject in subjects {
        let Some(source_path) = subject["source_path"].as_str() else {
            continue;
        };
        // Results made in another checkout name its paths: the file is the
        // longest tail of the path that is under this project.
        let mut file = path_of(source_path);
        if read(&file).is_none() {
            let parts: Vec<&str> = source_path
                .split('/')
                .filter(|part| !part.is_empty())
                .collect();
            if let Some(tail) = (0..parts.len())
                .map(|from| parts[from..].join("/"))
                .find(|tail| read(tail).is_some())
            {
                file = tail;
            }
        }
        // `Name#method:/path/file.rb:LINE`: the method's first line.
        let line = subject["identification"]
            .as_str()
            .and_then(|identification| identification.rsplit(':').next())
            .and_then(|line| line.parse::<u32>().ok());
        let entry = cache.entry(file.clone()).or_insert_with(|| {
            let source = read(&file)?;
            let described = described(&source, enumerate(&source));
            Some((source, described))
        });
        for result in subject["coverage_results"].as_array().into_iter().flatten() {
            let mutation = &result["mutation_result"];
            if mutation["mutation_type"].as_str() != Some("evil") {
                continue;
            }
            let criteria = &result["criteria_result"];
            let status = if criteria["timeout"] == true {
                "Timeout"
            } else if criteria["test_result"] == true || criteria["process_abort"] == true {
                "Killed"
            } else {
                "Survived"
            };
            let identification = mutation["mutation_identification"]
                .as_str()
                .unwrap_or_default();
            let matched = entry.as_ref().and_then(|(source, described)| {
                let code = mutation["mutation_source"].as_str()?;
                let tree = method_of(code.as_bytes())?;
                described.iter().find(|(mutant, start, _, _)| {
                    let _ = start;
                    let method_line =
                        position(source, mutant.method.span.map_or(0, |span| span.0)).0;
                    line.is_none_or(|line| method_line == line)
                        && same_method(&mutant.mutated, &tree)
                })
            });
            match matched {
                Some((mutant, start, end, descriptor)) => {
                    out.push(crate::mutants_query::StrykerReportMutant {
                        file: file.clone(),
                        start: *start,
                        end: *end,
                        mutator: mutant.mutator.to_owned(),
                        replacement: Some(descriptor.clone()),
                        original: None,
                        status: status.to_owned(),
                        id: Some(format!(
                            "{file}:{}:{}-{}:{}:{}:{descriptor}",
                            start.0, start.1, end.0, end.1, mutant.mutator
                        )),
                    })
                }
                None => out.push(crate::mutants_query::StrykerReportMutant {
                    file: file.clone(),
                    start: (line.unwrap_or(0), 0),
                    end: (line.unwrap_or(0), 0),
                    mutator: "Unmatched".into(),
                    replacement: None,
                    original: None,
                    status: status.to_owned(),
                    id: Some(format!("{file}:mutant:{identification}")),
                }),
            }
        }
    }
    Ok(out)
}

/// The first method defined in `code`, as mutant prints a mutation: the
/// method alone, or inside `class << self` or beside `memoize`.
fn method_of(code: &[u8]) -> Option<Rc<N>> {
    let result = ruby_prism::parse(code);
    if result.errors().next().is_some() {
        return None;
    }
    let mut finder = Methods::default();
    finder.visit(&result.node());
    let def = finder.defs.first()?.as_def_node()?;
    Converter::new(code).method(&def).ok()
}

/// Whether two methods are the same tree, whatever the spans.
fn same_method(left: &N, right: &N) -> bool {
    left == right
}

/// The kinds an operator's infection probe records, besides `left` and
/// `right`: each operator the light set replaces it with, and for `!=`,
/// `not:eql?` and `not:equal?`.
pub(crate) fn operator_kinds(operator: &str) -> Vec<String> {
    let mut kinds: Vec<String> = mutators::selector_replacements(operator)
        .iter()
        .map(|each| (*each).to_owned())
        .collect();
    if operator == "!=" {
        kinds.extend(["not:eql?".to_owned(), "not:equal?".to_owned()]);
    }
    kinds
}

/// The infection probe kind that records `mutant`'s value, where its
/// located node is a binary operator and its replacement one the probe
/// computes: another operator, an operand alone, or `!=`'s negations.
fn probe_kind(mutant: &RubyMutant) -> Option<String> {
    let replacement = mutant.replacement.as_ref()?;
    let located = path(&mutant.method, &mutant.mutated)
        .last()
        .map(|(original, _)| original.clone())?;
    if located.ty != "send" || located.children.len() != 3 || located.child(0).is_nil() {
        return None;
    }
    let operator = located.child(1).sym()?;
    if replacement.ty == "send"
        && replacement.children.len() == 3
        && replacement.child(0) == located.child(0)
        && replacement.child(2) == located.child(2)
    {
        let other = replacement.child(1).sym()?;
        return operator_kinds(operator)
            .into_iter()
            .find(|kind| kind == other);
    }
    if Some(replacement) == located.node(0) {
        return Some("left".into());
    }
    if Some(replacement) == located.node(2) {
        return Some("right".into());
    }
    if operator == "!=" && replacement.ty == "send" && replacement.child(1).sym() == Some("!") {
        let inner = replacement.node(0)?;
        let name = inner.child(1).sym()?;
        if inner.child(0) == located.child(0) && inner.child(2) == located.child(2) {
            return Some(format!("not:{name}"));
        }
    }
    None
}
