//! Mutant schemata for Ruby (Untch 1992): every mutant a strong pass runs,
//! written into its file behind a switch, so one loaded program holds them
//! all and a forked process turns one on by setting a global.
//!
//! A switch goes around the smallest node holding the mutation that can be
//! wrapped in parentheses without changing the program: a statement, an
//! expression in a slot that takes any expression (a condition, an operand,
//! a receiver, a value), a method body's statements. It reads
//!
//! ```ruby
//! (if ::Supercov::Strong::MUTANT == 3000017 then ::Supercov::Strong.h; (copy) elsif ... else (original) end)
//! ```
//!
//! where each copy is the node with one mutant applied, printed on one line,
//! so no line after the switch moves. `h` counts the active mutant's
//! evaluations against the hit limit. A mutant's key is its file's number
//! times `FILE_KEY` plus its index in the file's list.
//!
//! Every edit inserts text and removes none, as the Ruby runtime's load hook
//! applies edits: the original stays in place, between the switch's two
//! halves.

use std::collections::BTreeMap;
use std::rc::Rc;

use super::tree::{N, syntax};
use super::{RubyMutant, path, print};

/// A file's number times this, plus a mutant's index, is the mutant's key.
pub const FILE_KEY: u64 = 1_000_000;

/// What a strong pass inserts into one file.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Switches {
    /// `(offset, text)` insertions, in the order they apply at an offset.
    pub edits: Vec<(usize, String)>,
    /// Mutants with no switch, by index, and why.
    pub unplaceable: BTreeMap<usize, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Shape {
    /// `(if ... else (original) end)`.
    Expression,
    /// Statements: `(if ... else original end)`.
    Statements,
    /// A body with rescue or ensure clauses: `(if ... else begin original end end)`.
    Body,
    /// An empty method body: `(if ... end); ` before its `end`.
    Empty,
}

/// How a switch goes around `node` (a node of the method, with its span).
fn placeable(node: &N) -> Option<Shape> {
    if node.has(syntax::SYNTHETIC) {
        return match node.ty {
            "begin" => Some(Shape::Statements),
            "rescue" | "ensure" => Some(Shape::Body),
            _ => None,
        };
    }
    (node.has(syntax::STATEMENT) || node.has(syntax::EXPRESSION)).then_some(Shape::Expression)
}

/// Where `mutant`'s switch goes, and its copy there.
pub(super) fn place(
    source: &[u8],
    mutant: &RubyMutant,
) -> Result<(usize, usize, Shape, String), String> {
    let pairs = path(&mutant.method, &mutant.mutated);
    // An optional parameter made required: its default, evaluated exactly
    // where a call leaves the parameter out, raises what the required one
    // would.
    if let Some((original, Some(mutated))) = pairs.last()
        && matches!(
            (original.ty, mutated.ty),
            ("optarg", "arg") | ("kwoptarg", "kwarg")
        )
    {
        let default = original.node(1).expect("an optional parameter's default");
        let (start, end) = default.span.expect("a node of the method has its span");
        let error = if original.ty == "optarg" {
            "wrong number of arguments".to_owned()
        } else {
            format!(
                "missing keyword: :{}",
                original.child(0).sym().unwrap_or_default()
            )
        };
        return Ok((
            start,
            end,
            Shape::Expression,
            format!("::Kernel.raise(::ArgumentError, {error:?})"),
        ));
    }
    // A removed node: a statement in a list that is not the list's value
    // (`nil` there is the same program), or else its parent's place.
    let mut index = pairs.len() - 1;
    if pairs[index].1.is_none() {
        let removed = &pairs[index].0;
        let parent = &pairs[index - 1].0;
        let last_of_list = parent.ty == "begin"
            && parent
                .children
                .last()
                .and_then(|last| last.node())
                .is_some_and(|last| Rc::ptr_eq(last, removed));
        let in_list = matches!(parent.ty, "begin" | "kwbegin");
        match placeable(removed) {
            Some(Shape::Expression) if in_list && last_of_list => {}
            Some(shape) => {
                let (start, end) = removed.span.expect("placeable");
                return Ok((start, end, shape, "nil".into()));
            }
            None => {}
        }
        index -= 1;
    }
    loop {
        let (original, mutated) = &pairs[index];
        let mutated = mutated.as_ref().expect("an ancestor of the located node");
        // `self` as a receiver calls private methods only written as
        // `self.`: in a switch, or switched from, it is an expression, so the
        // call around it takes the switch.
        let private_receiver = index > 0
            && (mutated.ty == "self" || original.ty == "self")
            && matches!(
                pairs[index - 1].0.ty,
                "send" | "csend" | "op_asgn" | "or_asgn" | "and_asgn" | "index" | "indexasgn"
            )
            && pairs[index - 1]
                .0
                .node(0)
                .is_some_and(|receiver| Rc::ptr_eq(receiver, original));
        if !private_receiver
            && let Some(shape) = placeable(original)
            && let Some(copy) = print::one_line(source, &mutant.method, mutated)
        {
            let (start, end) = original.span.expect("placeable");
            return Ok((start, end, shape, copy));
        }
        if index == 0 {
            // The method itself: an empty body takes its switch before `end`.
            let body = if original.ty == "def" { 2 } else { 3 };
            // (An endless method always has a body.)
            if original.child(body).is_nil()
                && let Some(body) = mutated.node(body)
                && let Some(copy) = print::one_line(source, &mutant.method, body)
            {
                let (_, end) = original.span.expect("a method");
                return Ok((end - 3, end - 3, Shape::Empty, copy));
            }
            break;
        }
        index -= 1;
    }
    Err(if pairs.iter().any(|(node, _)| node.ty == "args") {
        "arguments".into()
    } else {
        "no-placeable-ancestor".into()
    })
}

/// The switches for a file's mutants (`file` is its number), all of them or
/// those `wanted` names by index.
pub(crate) fn switches(
    source: &[u8],
    file: u64,
    wanted: Option<&std::collections::BTreeSet<usize>>,
) -> Switches {
    let enumeration = super::enumerate(source);
    // Each switch's arms, by where it goes and its shape.
    let mut placements: BTreeMap<(usize, usize, Shape), Vec<(u64, String)>> = BTreeMap::new();
    let mut unplaceable = BTreeMap::new();
    for (index, mutant) in enumeration.mutants.iter().enumerate() {
        if wanted.is_some_and(|wanted| !wanted.contains(&index)) {
            continue;
        }
        match place(source, mutant) {
            Ok((start, end, shape, copy)) => {
                let key = file * FILE_KEY + index as u64;
                placements
                    .entry((start, end, shape))
                    .or_default()
                    .push((key, copy));
            }
            Err(reason) => {
                unplaceable.insert(index, reason);
            }
        }
    }
    // Openers: outer before inner at one offset; closers: inner before outer.
    let mut edits: Vec<(usize, u8, usize, String)> = Vec::new();
    for (&(start, end, shape), arms) in &placements {
        let mut opener = String::from("(if ");
        let arms: Vec<String> = arms
            .iter()
            .map(|(key, copy)| {
                format!("::Supercov::Strong::MUTANT == {key} then ::Supercov::Strong.h; ({copy})")
            })
            .collect();
        opener.push_str(&arms.join(" elsif "));
        let (open_tail, closer) = match shape {
            Shape::Expression => (" else (", ") end)"),
            Shape::Statements => (" else ", " end)"),
            // `;` before the `end`: the body may end in a clause header
            // (`rescue NameError`) that needs one before its own `end`.
            Shape::Body => (" else begin ", "; end end)"),
            Shape::Empty => (" end); ", ""),
        };
        opener.push_str(open_tail);
        let width = end - start;
        // Closers sort before openers at one offset; among openers the wider
        // first, among closers the narrower first.
        edits.push((start, 1, usize::MAX - width, opener));
        if !closer.is_empty() {
            edits.push((end, 0, width, closer.to_owned()));
        }
    }
    edits.sort_by_key(|edit| (edit.0, edit.1, edit.2));
    Switches {
        edits: edits
            .into_iter()
            .map(|(offset, _, _, text)| (offset, text))
            .collect(),
        unplaceable,
    }
}

/// The source with `edits` inserted.
pub(crate) fn apply(source: &[u8], edits: &[(usize, String)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(source.len());
    let mut cursor = 0;
    for (offset, text) in edits {
        out.extend_from_slice(&source[cursor..*offset]);
        out.extend_from_slice(text.as_bytes());
        cursor = *offset;
    }
    out.extend_from_slice(&source[cursor..]);
    out
}
