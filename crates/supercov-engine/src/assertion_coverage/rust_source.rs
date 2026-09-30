//! Rust: statements by rust-analyzer's parser, tests by function name and
//! doctests by the line of their code block.
use super::{Change, Located, Starts, line_of, line_starts};
use ra_ap_syntax::{
    AstNode, Edition, SourceFile,
    ast::{self, BinaryOp},
};
use std::path::Path;

/// Every statement and block tail starting on each line, with its change:
/// `if` (and `else if`) inverted, `return x` and a block's value becoming
/// `Default::default()`, a `let` or assignment's value becoming
/// `Default::default()`, anything else skipped. Items and `let` without a
/// value are not assessed.
pub(super) fn statement_starts(source: &str) -> Starts {
    let mut out = Starts::new();
    let parsed = [
        Edition::Edition2024,
        Edition::Edition2021,
        Edition::Edition2018,
        Edition::Edition2015,
    ]
    .into_iter()
    .map(|edition| SourceFile::parse(source, edition))
    .find(|p| p.errors().is_empty())
    .unwrap_or_else(|| SourceFile::parse(source, Edition::Edition2021));
    let root = parsed.tree();
    let starts = line_starts(source);
    // A test's own code is the test, not code it checks: `#[test]`
    // functions and `#[cfg(test)]` modules are not assessed.
    let tests = root
        .syntax()
        .descendants()
        .filter(|node| ast::Fn::can_cast(node.kind()) || ast::Module::can_cast(node.kind()))
        .filter(|node| {
            node.children().filter_map(ast::Attr::cast).any(|attr| {
                let text = attr.syntax().text().to_string().replace(' ', "");
                text == "#[test]" || text.starts_with("#[cfg(test") || text.ends_with("::test]")
            })
        })
        .map(|node| node.text_range())
        .collect::<Vec<_>>();
    let in_test =
        |node: &ra_ap_syntax::SyntaxNode| tests.iter().any(|r| r.contains_range(node.text_range()));
    let mut push = |node: &ra_ap_syntax::SyntaxNode, change: Option<Change>| {
        let change = change.filter(|_| !in_test(node));
        let range = node.text_range();
        let (start, end) = (usize::from(range.start()), usize::from(range.end()));
        out.entry(line_of(&starts, start))
            .or_default()
            .push((source.get(start..end).unwrap_or("").to_owned(), change));
    };
    for list in root.syntax().descendants().filter_map(ast::StmtList::cast) {
        for statement in list.statements() {
            let change = match &statement {
                ast::Stmt::LetStmt(s) => s.initializer().map(|_| Change::ValueUndefined),
                ast::Stmt::ExprStmt(s) => Some(match s.expr() {
                    Some(e) => expression(&e, false),
                    None => Change::Skip,
                }),
                ast::Stmt::Item(_) => None,
            };
            push(statement.syntax(), change);
        }
        if let Some(tail) = list.tail_expr() {
            push(tail.syntax(), Some(expression(&tail, true)));
        }
    }
    // `else if`: its own `if`, recorded by coverage as a decision.
    for chained in root.syntax().descendants().filter_map(ast::IfExpr::cast) {
        if chained
            .syntax()
            .parent()
            .is_some_and(|p| ast::IfExpr::can_cast(p.kind()))
        {
            push(chained.syntax(), Some(Change::Invert));
            if let Some(condition) = chained.condition() {
                push(condition.syntax(), Some(Change::Invert));
            }
        }
    }
    out
}

fn expression(e: &ast::Expr, tail: bool) -> Change {
    match e {
        ast::Expr::IfExpr(_) => Change::Invert,
        ast::Expr::ReturnExpr(r) => {
            if r.expr().is_some() {
                Change::ReturnUndefined
            } else {
                Change::Skip
            }
        }
        ast::Expr::BinExpr(b) if matches!(b.op_kind(), Some(BinaryOp::Assignment { op: None })) => {
            Change::ValueUndefined
        }
        ast::Expr::ForExpr(_) | ast::Expr::WhileExpr(_) | ast::Expr::LoopExpr(_) => Change::Skip,
        _ if tail => Change::ReturnUndefined,
        _ => Change::Skip,
    }
}

/// `if c {`, `} else if let Some(x) = y {` -> the condition as written.
pub(super) fn condition(line: &str) -> String {
    super::go::condition(line)
}

/// Whether a file belongs to the tests rather than the crate: an integration
/// test, a benchmark or an example, and the modules they declare.
pub(super) fn test_path(file: &str) -> bool {
    ["tests/", "benches/", "examples/"]
        .iter()
        .any(|d| file.starts_with(d) || file.contains(&format!("/{d}")))
}

/// A libtest test by its function name (`tests/a.rs::module::name`), or a
/// doctest by the line of its code block (`src/lib.rs - Item (line 19)`).
pub(super) fn locate(lines: &[&str], name: &str) -> Option<Located> {
    if let Some(rest) = name.rsplit_once("(line ").map(|(_, r)| r) {
        let line: usize = rest.trim_end_matches(')').trim().parse().ok()?;
        let start = line.checked_sub(1)?;
        let end = (start + 1..lines.len())
            .find(|&i| lines[i].contains("```"))
            .unwrap_or(lines.len().saturating_sub(1).min(start + 40));
        let item = (end + 1..lines.len().min(end + 20)).find(|&i| {
            let t = lines[i].trim_start();
            !t.starts_with("///") && !t.starts_with("//!") && !t.starts_with('#') && !t.is_empty()
        });
        return Some(Located {
            context: Vec::new(),
            start,
            end: item.unwrap_or(end),
        });
    }
    let function = name.rsplit("::").next().unwrap_or(name).trim();
    let start = lines.iter().position(|l| {
        let t = l.trim_start();
        let t = t.strip_prefix("pub ").unwrap_or(t);
        let t = t.strip_prefix("async ").unwrap_or(t);
        t.strip_prefix("fn ")
            .and_then(|r| r.strip_prefix(function))
            .is_some_and(|r| r.starts_with('(') || r.starts_with('<'))
    })?;
    let mut first = start;
    while first > 0 && lines[first - 1].trim_start().starts_with("#[") {
        first -= 1;
    }
    Some(Located {
        context: (first..start).collect(),
        start,
        end: super::brace_end(lines, start),
    })
}

/// A Rust test's helpers: the modules an integration test declares
/// (`mod common;` -> `tests/common/mod.rs` or `tests/common.rs`).
pub(super) fn helpers(root: &Path, from: &str, text: &str) -> Vec<String> {
    // A unit test or doctest's modules are the crate's own source.
    if !from.starts_with("tests/") && !from.contains("/tests/") {
        return Vec::new();
    }
    let here = Path::new(from).parent().unwrap_or(Path::new(""));
    text.split('\n')
        .filter_map(|l| {
            let t = l.trim_start();
            let t = t.strip_prefix("pub ").unwrap_or(t);
            let name = t.strip_prefix("mod ")?.strip_suffix(';')?.trim();
            [
                here.join(name).join("mod.rs"),
                here.join(format!("{name}.rs")),
            ]
            .into_iter()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .find(|p| root.join(p).is_file())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assertion_coverage::{Language, change_at};

    #[test]
    fn changes_follow_the_audit() {
        let source = "use std::fmt;\n\nfn f(x: u32) -> u32 {\n    let y;\n    let z = x + 1;\n    if z > 2 {\n        return z;\n    } else if z == 0 {\n        y = 1;\n    }\n    println!(\"{z}\");\n    z * 2\n}\n";
        let starts = statement_starts(source);
        let rs = Language::Rust;
        assert_eq!(change_at(&starts, 4, "let y;", rs), None);
        assert_eq!(
            change_at(&starts, 5, "let z = x + 1;", rs),
            Some(Change::ValueUndefined)
        );
        assert_eq!(
            change_at(&starts, 6, "if z > 2 {", rs),
            Some(Change::Invert)
        );
        assert_eq!(
            change_at(&starts, 7, "return z;", rs),
            Some(Change::ReturnUndefined)
        );
        assert_eq!(change_at(&starts, 8, "z == 0", rs), Some(Change::Invert));
        assert_eq!(
            change_at(&starts, 9, "y = 1;", rs),
            Some(Change::ValueUndefined)
        );
        assert_eq!(
            change_at(&starts, 11, "println!(\"{z}\");", rs),
            Some(Change::Skip)
        );
        assert_eq!(
            change_at(&starts, 12, "z * 2", rs),
            Some(Change::ReturnUndefined)
        );
    }

    #[test]
    fn tests_and_doctests_are_found() {
        let text = "//! ```\n//! assert!(a::f());\n//! ```\n\n#[test]\nfn works() {\n    assert!(f());\n}\n";
        let lines = text.split('\n').collect::<Vec<_>>();
        let doc = locate(&lines, "src/lib.rs - (line 1)").unwrap();
        assert_eq!((doc.start, doc.end), (0, 5));
        let test = locate(&lines, "tests/a.rs::works").unwrap();
        assert_eq!(
            (test.context.clone(), test.start, test.end),
            (vec![4], 5, 7)
        );
    }
}
