//! Rust mutants behind switches: one build holds every mutant of a strong
//! pass (mutant schemata, Untch 1992), and the runtime's active mutant picks
//! which one runs.
//!
//! A mutant is its file and its index in the file's list;
//! `crate::<runtime>::on("src/a.rs", n)` is true only in a process started
//! with `SUPERCOV_ACTIVE_MUTANT=src/a.rs#n`, and counts that mutant's
//! evaluations against the hit limit. A switch is:
//!
//! - a function body: `if on(n) { return <value>; }` before its statements;
//! - an operator: `(if on(n) { <the expression with the operator replaced> }
//!   else if on(m) { .. } else { <the expression> })`, and the same around a
//!   match guard (`true`, `false`) and a struct literal (without a field);
//! - a match arm deleted: `<pattern> if !on(n) => ..`, so the value falls
//!   through to the arms after it, as it would with the arm gone.
//!
//! A replacement branch holds the code as written with only its mutant
//! applied; the original branch holds the switches of the mutants inside it.
//! Rust type-checks every branch, so a mutant that does not compile breaks
//! the build: the strong driver reads the compiler's errors by their byte
//! ranges in the switched file (`SwitchedFile::replacements`,
//! `SwitchedFile::switches`) to find the mutants behind them.
//!
//! Some mutants cannot be switched at run time, and `placeable` says why:
//! code the compiler evaluates (`const`, `static`, a `const fn`, an array
//! length, an enum discriminant), a body whose value is an opaque
//! `impl Trait` (a second value would be a second type), a `let` chain (an
//! `if` around it ends the chain), and an allocator's own code (the
//! runtime reads the environment once, which allocates). The driver builds
//! such a mutant on its own, written into the source, as cargo-mutants does.

use std::ops::Range;

use ra_ap_syntax::{AstNode, SyntaxKind, SyntaxNode, TextRange, ast};

use crate::rust_mutants::{RustMutant, Site};

/// A file with its mutants behind switches.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SwitchedFile {
    pub code: String,
    /// Each placed mutant's own code in `code`: what is new with it.
    pub replacements: Vec<(u64, Range<usize>)>,
    /// Each switch in `code`, and the mutants it holds.
    pub switches: Vec<(Range<usize>, Vec<u64>)>,
}

/// Why a mutant cannot be switched at run time, or `None` where it can.
pub(crate) fn placeable(root: &SyntaxNode, mutant: &RustMutant) -> Option<&'static str> {
    let at = match &mutant.site {
        Site::Body { list, .. } => *list,
        Site::Operator { expression } => *expression,
        Site::Arm { arm, .. } => *arm,
        Site::Guard => mutant.range,
        Site::Field { record } => *record,
    };
    let node = match root.covering_element(at) {
        ra_ap_syntax::NodeOrToken::Node(node) => node,
        ra_ap_syntax::NodeOrToken::Token(token) => token.parent()?,
    };
    if let Site::Body { opaque: true, .. } = mutant.site {
        return Some("opaque-return");
    }
    if crate::rust_instrumenter::cannot_carry_probe(&node)
        || node
            .ancestors()
            .take_while(|ancestor| !ast::Fn::can_cast(ancestor.kind()))
            .any(|ancestor| ast::Variant::can_cast(ancestor.kind()))
    {
        return Some("const-context");
    }
    if let Site::Operator { .. } = mutant.site
        && node
            .descendants()
            .any(|inner| inner.kind() == SyntaxKind::LET_EXPR)
    {
        return Some("let-chain");
    }
    None
}

/// What a site turns into.
enum Kind {
    /// Code put in at `at`, for one mutant.
    Insert {
        ordinal: u64,
        before: String,
        own: String,
        after: String,
    },
    /// A switch around `range`: each mutant's replacement code.
    Wrap { cases: Vec<(u64, String)> },
}

struct Placed {
    range: TextRange,
    /// Among sites with the same range, the outer first.
    rank: u8,
    kind: Kind,
}

/// `source` with `mutants` (each with its ordinal) behind switches that read
/// `runtime` (`crate::<module>`). Every mutant given is switched: the caller
/// leaves out what `placeable` refuses.
pub(crate) fn switch(
    source: &str,
    runtime: &str,
    file: &str,
    mutants: &[(u64, &RustMutant)],
) -> SwitchedFile {
    let on = |ordinal: u64| format!("{runtime}::on({file:?}, {ordinal})");
    let mut placed: Vec<Placed> = Vec::new();
    // Mutants that share a site share one switch.
    let mut wrap = |range: TextRange, rank: u8, ordinal: u64, code: String| {
        if let Some(Placed {
            kind: Kind::Wrap { cases },
            ..
        }) = placed
            .iter_mut()
            .find(|site| site.range == range && site.rank == rank)
        {
            cases.push((ordinal, code));
        } else {
            placed.push(Placed {
                range,
                rank,
                kind: Kind::Wrap {
                    cases: vec![(ordinal, code)],
                },
            });
        }
    };
    let mut inserts = Vec::new();
    for (ordinal, mutant) in mutants {
        let ordinal = *ordinal;
        match &mutant.site {
            Site::Body { list, .. } => {
                let at = body_entry(source, *list);
                inserts.push(Placed {
                    range: TextRange::empty(ra_ap_syntax::TextSize::from(at as u32)),
                    rank: 0,
                    kind: Kind::Insert {
                        ordinal,
                        before: format!(" if {} {{ return ", on(ordinal)),
                        own: mutant.replacement.clone(),
                        after: "; }".to_owned(),
                    },
                });
            }
            Site::Arm { pattern, .. } => inserts.push(Placed {
                range: TextRange::empty(pattern.end()),
                rank: 0,
                kind: Kind::Insert {
                    ordinal,
                    before: " if !".to_owned(),
                    own: on(ordinal),
                    after: String::new(),
                },
            }),
            Site::Operator { expression } => {
                let code = spliced(source, *expression, mutant.range, &mutant.replacement);
                wrap(*expression, 2, ordinal, code);
            }
            Site::Guard => wrap(mutant.range, 1, ordinal, mutant.replacement.clone()),
            Site::Field { record } => {
                let code = spliced(source, *record, mutant.range, "");
                wrap(*record, 0, ordinal, code);
            }
        }
    }
    placed.extend(inserts);
    // Outer sites first: by start, then the longer, then the lower rank.
    placed.sort_by(|left, right| {
        left.range
            .start()
            .cmp(&right.range.start())
            .then(right.range.len().cmp(&left.range.len()))
            .then(left.rank.cmp(&right.rank))
    });
    let mut out = SwitchedFile::default();
    let mut next = 0;
    let runtime = format!("{runtime}::on({file:?}, ");
    emit(
        source,
        &runtime,
        &placed,
        &mut next,
        0..source.len(),
        &mut out,
    );
    out
}

/// Writes `source[range]` with every site from `*next` on that starts in it.
fn emit(
    source: &str,
    runtime: &str,
    placed: &[Placed],
    next: &mut usize,
    range: Range<usize>,
    out: &mut SwitchedFile,
) {
    let mut at = range.start;
    while let Some(site) = placed.get(*next) {
        let start = usize::from(site.range.start());
        let end = usize::from(site.range.end());
        let inside = match site.kind {
            Kind::Insert { .. } => start >= range.start && start < range.end,
            Kind::Wrap { .. } => start >= range.start && end <= range.end,
        };
        if !inside {
            break;
        }
        *next += 1;
        out.code.push_str(&source[at..start]);
        match &site.kind {
            Kind::Insert {
                ordinal,
                before,
                own,
                after,
            } => {
                let switch_start = out.code.len();
                out.code.push_str(before);
                let own_start = out.code.len();
                out.code.push_str(own);
                out.replacements.push((*ordinal, own_start..out.code.len()));
                out.code.push_str(after);
                out.switches
                    .push((switch_start..out.code.len(), vec![*ordinal]));
                at = start;
            }
            Kind::Wrap { cases } => {
                let switch_start = out.code.len();
                out.code.push('(');
                for (ordinal, code) in cases {
                    out.code.push_str(&format!("if {runtime}{ordinal}) {{ "));
                    let own_start = out.code.len();
                    out.code.push_str(code);
                    out.replacements.push((*ordinal, own_start..out.code.len()));
                    out.code.push_str(" } else ");
                }
                out.code.push_str("{ ");
                emit(source, runtime, placed, next, start..end, out);
                out.code.push_str(" })");
                out.switches.push((
                    switch_start..out.code.len(),
                    cases.iter().map(|(ordinal, _)| *ordinal).collect(),
                ));
                at = end;
            }
        }
    }
    out.code.push_str(&source[at..range.end]);
}

/// `source[outer]` with `inner` replaced by `with`.
fn spliced(source: &str, outer: TextRange, inner: TextRange, with: &str) -> String {
    format!(
        "{}{with}{}",
        &source[usize::from(outer.start())..usize::from(inner.start())],
        &source[usize::from(inner.end())..usize::from(outer.end())]
    )
}

/// Where a body's switch goes: after its `{` and its inner attributes.
fn body_entry(source: &str, list: TextRange) -> usize {
    let text = &source[usize::from(list.start())..usize::from(list.end())];
    let Some((tree, _)) = crate::rust_instrumenter::parse_source(&format!("fn f() {text}")) else {
        return usize::from(list.start()) + 1;
    };
    let shift = "fn f() ".len();
    tree.syntax()
        .descendants()
        .find_map(ast::StmtList::cast)
        .map(|parsed| {
            parsed
                .syntax()
                .children()
                .filter(|child| child.kind() == SyntaxKind::ATTR)
                .last()
                .map_or(1, |attribute| {
                    usize::from(attribute.text_range().end()) - shift
                })
        })
        .map_or(usize::from(list.start()) + 1, |offset| {
            usize::from(list.start()) + offset
        })
}

/// The strong runtime a crate root gets, named `module`: `on`, the active
/// mutant (`SUPERCOV_ACTIVE_MUTANT`) and the hit limit
/// (`SUPERCOV_MUTANT_HIT_LIMIT`, none where unset or 0). In wasm, where the
/// host names the active mutant, its exports carry `key`, unique to the
/// crate.
pub(crate) fn runtime(module: &str, key: &str) -> String {
    STRONG_RUNTIME
        .replace("__SUPERCOV_MODULE__", module)
        .replace(
            "__SUPERCOV_BUFFER_EXPORT__",
            &format!("__supercov_strong_buffer_{key}"),
        )
        .replace(
            "__SUPERCOV_ACTIVATE_EXPORT__",
            &format!("__supercov_strong_activate_{key}"),
        )
}

const STRONG_RUNTIME: &str = include_str!("../runtime-assets/rust-strong-runtime.rs");

/// The variable that names the active mutant's ordinal.
pub(crate) const ACTIVE_MUTANT_ENV: &str = "SUPERCOV_ACTIVE_MUTANT";
/// The variable that bounds the active mutant's evaluations.
pub(crate) const HIT_LIMIT_ENV: &str = "SUPERCOV_MUTANT_HIT_LIMIT";
/// What a process writes to standard error as it stops at the hit limit.
pub(crate) const TIMEOUT_MARKER: &str = "SUPERCOV_MUTANT_TIMEOUT";

#[cfg(test)]
mod tests {
    use super::*;

    fn switched(source: &str) -> (SwitchedFile, Vec<RustMutant>) {
        let mutants = crate::rust_mutants::enumerate(source).unwrap();
        let (tree, _) = crate::rust_instrumenter::parse_source(source).unwrap();
        let chosen = mutants
            .iter()
            .enumerate()
            .filter(|(_, mutant)| placeable(tree.syntax(), mutant).is_none())
            .map(|(index, mutant)| (index as u64, mutant))
            .collect::<Vec<_>>();
        (switch(source, "crate::rt", "a.rs", &chosen), mutants)
    }

    #[test]
    fn operators_bodies_arms_guards_and_fields_are_switched() {
        let source = "pub fn f(a: i32, b: i32) -> i32 {\n    a + b * 2\n}\n";
        let (file, mutants) = switched(source);
        assert_eq!(mutants.len(), 7);
        assert_eq!(
            file.code,
            "pub fn f(a: i32, b: i32) -> i32 { if crate::rt::on(\"a.rs\", 0) { return 0; } if crate::rt::on(\"a.rs\", 1) { return 1; } if crate::rt::on(\"a.rs\", 2) { return -1; }\n    (if crate::rt::on(\"a.rs\", 3) { a - b * 2 } else if crate::rt::on(\"a.rs\", 4) { a * b * 2 } else { a + (if crate::rt::on(\"a.rs\", 5) { b + 2 } else if crate::rt::on(\"a.rs\", 6) { b / 2 } else { b * 2 }) })\n}\n"
        );
        let own = |ordinal: u64| {
            let (_, range) = file
                .replacements
                .iter()
                .find(|(at, _)| *at == ordinal)
                .unwrap();
            &file.code[range.clone()]
        };
        assert_eq!(own(1), "1");
        assert_eq!(own(4), "a * b * 2");
        assert_eq!(own(6), "b / 2");
        assert_eq!(file.switches.len(), 5);

        let source = "fn g(n: Option<u8>, s: S) -> S {\n    #![allow(unused)]\n    match n {\n        Some(0) => s,\n        Some(x) if x > 1 => S { a: x, ..s },\n        _ => s,\n    }\n}\n";
        let (file, _) = switched(source);
        assert!(
            file.code.contains(
                "#![allow(unused)] if crate::rt::on(\"a.rs\", 0) { return Default::default(); }"
            ),
            "{}",
            file.code
        );
        assert!(
            file.code
                .contains("Some(0) if !crate::rt::on(\"a.rs\", 1) => s,"),
            "{}",
            file.code
        );
        assert!(file.code.contains("Some(x) if (if crate::rt::on(\"a.rs\", 2) { true } else if crate::rt::on(\"a.rs\", 3) { false } else { (if crate::rt::on(\"a.rs\", 4) { x == 1 }"), "{}", file.code);
        assert!(
            file.code.contains(
                "(if crate::rt::on(\"a.rs\", 7) { S {  ..s } } else { S { a: x, ..s } })"
            ),
            "{}",
            file.code
        );
    }

    #[test]
    fn code_the_compiler_evaluates_is_not_switched() {
        let source = "const N: usize = 1 + 2;\nconst fn c(a: u8) -> u8 { a + 1 }\nfn i() -> impl Iterator<Item = u8> { std::iter::once(1) }\nfn l(a: Option<u8>, b: bool) -> bool { if let Some(_) = a && b { true } else { false } }\nenum E { A = 1 << 2 }\nfn r(a: u8) -> u8 { a + 1 }\n";
        let mutants = crate::rust_mutants::enumerate(source).unwrap();
        let (tree, _) = crate::rust_instrumenter::parse_source(source).unwrap();
        let reasons = mutants
            .iter()
            .map(|mutant| placeable(tree.syntax(), mutant))
            .collect::<Vec<_>>();
        assert!(reasons.contains(&Some("const-context")));
        assert!(reasons.contains(&Some("opaque-return")));
        assert!(reasons.contains(&Some("let-chain")));
        assert!(reasons.contains(&None));
        assert!(runtime("rt", "k").contains("mod rt"));
        assert!(runtime("rt", "k").contains("__supercov_strong_activate_k"));
    }
}
