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
//! (if $__supercov_mutant == 3000017 then $__supercov_strong.h; (copy) elsif ... else (original) end)
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

/// Where one switch goes and what it switches between.
struct Placement {
    start: usize,
    end: usize,
    /// The text before the original, after the switch's arms.
    shape: Shape,
    arms: Vec<(u64, String)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// `(if ... else (original) end)`.
    Expression,
    /// Statements: `(if ... else original end)`.
    Statements,
    /// A body with rescue or ensure clauses: `(if ... else begin original end end)`.
    Body,
    /// An empty method body: `(if ... end); ` before its `end`.
    Empty,
}

fn placeable(node: &N) -> Option<Shape> {
    node.span?;
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
fn place(source: &[u8], mutant: &RubyMutant) -> Result<(usize, usize, Shape, String), String> {
    let pairs = path(&mutant.method, &mutant.mutated);
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
        if let Some(shape) = placeable(original)
            && let Some(copy) = print::one_line(source, mutated)
        {
            let (start, end) = original.span.expect("placeable");
            return Ok((start, end, shape, copy));
        }
        if index == 0 {
            // The method itself: an empty body takes its switch before `end`.
            let body = if original.ty == "def" { 2 } else { 3 };
            if original.child(body).is_nil()
                && !original.has(syntax::ENDLESS)
                && let Some(body) = mutated.node(body)
                && let Some(copy) = print::one_line(source, body)
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
    let mut placements: Vec<Placement> = Vec::new();
    let mut unplaceable = BTreeMap::new();
    for (index, mutant) in enumeration.mutants.iter().enumerate() {
        if wanted.is_some_and(|wanted| !wanted.contains(&index)) {
            continue;
        }
        match place(source, mutant) {
            Ok((start, end, shape, copy)) => {
                let key = file * FILE_KEY + index as u64;
                match placements.iter_mut().find(|placement| {
                    placement.start == start && placement.end == end && placement.shape == shape
                }) {
                    Some(placement) => placement.arms.push((key, copy)),
                    None => placements.push(Placement {
                        start,
                        end,
                        shape,
                        arms: vec![(key, copy)],
                    }),
                }
            }
            Err(reason) => {
                unplaceable.insert(index, reason);
            }
        }
    }
    // Openers: outer before inner at one offset; closers: inner before outer.
    let mut edits: Vec<(usize, u8, usize, String)> = Vec::new();
    for placement in &placements {
        let mut opener = String::from("(if ");
        for (position, (key, copy)) in placement.arms.iter().enumerate() {
            if position > 0 {
                opener.push_str(" elsif ");
            }
            opener.push_str(&format!(
                "$__supercov_mutant == {key} then $__supercov_strong.h; ({copy})"
            ));
        }
        let (open_tail, closer) = match placement.shape {
            Shape::Expression => (" else (", ") end)"),
            Shape::Statements => (" else ", " end)"),
            Shape::Body => (" else begin ", " end end)"),
            Shape::Empty => (" end); ", ""),
        };
        opener.push_str(open_tail);
        let width = placement.end - placement.start;
        // Closers sort before openers at one offset; among openers the wider
        // first, among closers the narrower first.
        edits.push((placement.start, 1, usize::MAX - width, opener));
        if !closer.is_empty() {
            edits.push((placement.end, 0, width, closer.to_owned()));
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
