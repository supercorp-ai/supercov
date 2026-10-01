//! Rust: statements by rust-analyzer's parser, tests by function name and
//! doctests by the line of their code block.
use super::{Change, Located, Starts, line_of, line_starts};
use ra_ap_syntax::{
    AstNode, Edition, SourceFile,
    ast::{self, BinaryOp},
};
use std::collections::BTreeMap;
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

/// For each line, the statements starting there whose value changes, with
/// the value that replaces it.
pub(super) type Replacements = BTreeMap<usize, Vec<(String, String)>>;

/// What replaces each returned or assigned value: a value of its type that
/// differs from it, rather than `Default::default()` -- which some types lack
/// (`fmt::Result`, `Ordering`) and which is the value itself for
/// `return false;`. A boolean is negated, an `Ok` becomes an `Err` and an
/// `Err` an `Ok`, a literal is swapped for another; the type comes from the
/// function's signature or the `let`'s annotation, else from the
/// expression's shape.
pub(super) fn replacements(source: &str) -> Replacements {
    let root = parse(source).tree();
    let starts = line_starts(source);
    let mut out = Replacements::new();
    let text = |node: &ra_ap_syntax::SyntaxNode| node.text().to_string();
    let mut push = |node: &ra_ap_syntax::SyntaxNode, replacement: String| {
        let start = usize::from(node.text_range().start());
        out.entry(line_of(&starts, start))
            .or_default()
            .push((node.text().to_string(), replacement));
    };
    for list in root.syntax().descendants().filter_map(ast::StmtList::cast) {
        for statement in list.statements() {
            match &statement {
                ast::Stmt::LetStmt(s) => {
                    if let Some(value) = s.initializer() {
                        let ty = s.ty().map(|t| text(t.syntax()));
                        push(
                            s.syntax(),
                            replacement(&text(value.syntax()), ty.as_deref()),
                        );
                    }
                }
                ast::Stmt::ExprStmt(s) => {
                    if let Some(e) = s.expr() {
                        value_change(&e, &mut push, None);
                    }
                }
                ast::Stmt::Item(_) => {}
            }
        }
        if let Some(tail) = list.tail_expr() {
            value_change(&tail, &mut push, Some(&list));
        }
    }
    out
}

fn value_change(
    e: &ast::Expr,
    push: &mut impl FnMut(&ra_ap_syntax::SyntaxNode, String),
    tail_of: Option<&ast::StmtList>,
) {
    let text = |node: &ra_ap_syntax::SyntaxNode| node.text().to_string();
    match e {
        ast::Expr::ReturnExpr(r) => {
            if let Some(value) = r.expr() {
                let ty = return_type(r.syntax());
                push(
                    r.syntax(),
                    replacement(&text(value.syntax()), ty.as_deref()),
                );
            }
        }
        ast::Expr::BinExpr(b) if matches!(b.op_kind(), Some(BinaryOp::Assignment { op: None })) => {
            if let Some(rhs) = b.rhs() {
                push(b.syntax(), replacement(&text(rhs.syntax()), None));
            }
        }
        ast::Expr::IfExpr(_)
        | ast::Expr::ForExpr(_)
        | ast::Expr::WhileExpr(_)
        | ast::Expr::LoopExpr(_) => {}
        _ => {
            if let Some(list) = tail_of {
                let ty = tail_type(list);
                push(e.syntax(), replacement(&text(e.syntax()), ty.as_deref()));
            }
        }
    }
}

fn parse(source: &str) -> ra_ap_syntax::Parse<SourceFile> {
    [
        Edition::Edition2024,
        Edition::Edition2021,
        Edition::Edition2018,
        Edition::Edition2015,
    ]
    .into_iter()
    .map(|edition| SourceFile::parse(source, edition))
    .find(|p| p.errors().is_empty())
    .unwrap_or_else(|| SourceFile::parse(source, Edition::Edition2021))
}

/// The declared return type of the function or closure a `return` leaves.
fn return_type(node: &ra_ap_syntax::SyntaxNode) -> Option<String> {
    for ancestor in node.ancestors().skip(1) {
        if let Some(f) = ast::Fn::cast(ancestor.clone()) {
            return Some(
                f.ret_type()
                    .and_then(|r| r.ty())
                    .map_or("()".into(), |t| t.syntax().text().to_string()),
            );
        }
        if let Some(c) = ast::ClosureExpr::cast(ancestor) {
            return c
                .ret_type()
                .and_then(|r| r.ty())
                .map(|t| t.syntax().text().to_string());
        }
    }
    None
}

/// The type a block's value has when it is what the function returns: the
/// body's value, or a branch or arm of an `if` or `match` that is.
fn tail_type(list: &ast::StmtList) -> Option<String> {
    let mut node = list.syntax().parent()?; // the block
    loop {
        let parent = node.parent()?;
        if let Some(f) = ast::Fn::cast(parent.clone()) {
            return Some(
                f.ret_type()
                    .and_then(|r| r.ty())
                    .map_or("()".into(), |t| t.syntax().text().to_string()),
            );
        }
        // Through `if`/`else if`/`else` and `match` arms to the expression
        // they form; it must itself be a block's value.
        let mut expression = node.clone();
        while let Some(up) = expression.parent() {
            if ast::IfExpr::can_cast(up.kind())
                || ast::MatchArm::can_cast(up.kind())
                || ast::MatchArmList::can_cast(up.kind())
                || ast::MatchExpr::can_cast(up.kind())
            {
                expression = up;
            } else {
                break;
            }
        }
        if !(ast::IfExpr::can_cast(expression.kind())
            || ast::MatchExpr::can_cast(expression.kind())
            || ast::BlockExpr::can_cast(expression.kind()))
        {
            return None;
        }
        let holder = expression.parent()?;
        let list = ast::StmtList::cast(holder)?;
        if list.tail_expr().map(|t| t.syntax().clone()) != Some(expression) {
            return None;
        }
        node = list.syntax().parent()?;
    }
}

const INTEGERS: &[&str] = &[
    "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128", "isize",
];

/// A value of the expression's type that differs from it.
pub(super) fn replacement(expr: &str, ty: Option<&str>) -> String {
    let e = expr.trim();
    match e {
        "true" => return "false".into(),
        "false" => return "true".into(),
        "Ok(())" => return "Err(Default::default())".into(),
        "()" => return "()".into(),
        _ => {}
    }
    if let Some((digits, suffix)) = integer_literal(e) {
        let zero = digits.chars().all(|c| c == '0' || c == '_');
        return format!("{}{suffix}", if zero { "1" } else { "0" });
    }
    if e.starts_with('"') && e.ends_with('"') {
        return if e == "\"\"" {
            "\"supercov\"".into()
        } else {
            "\"\"".into()
        };
    }
    let ty = ty.map(|t| t.split_whitespace().collect::<String>());
    let result_like = |t: &str| {
        t.starts_with("Result")
            || t.ends_with("::Result")
            || t.contains("::Result<")
            || t.ends_with("Result")
    };
    let ordering = |e: &str| {
        for (from, to) in [("Less", "Greater"), ("Greater", "Less"), ("Equal", "Less")] {
            if e == format!("Ordering::{from}") || e.ends_with(&format!("::Ordering::{from}")) {
                return Some(format!("Ordering::{to}"));
            }
        }
        None
    };
    let err_or_ok = |e: &str| {
        if e.starts_with("Err(") {
            "Ok(Default::default())".to_owned()
        } else {
            "Err(Default::default())".to_owned()
        }
    };
    match ty.as_deref() {
        Some("bool") => format!("!({e})"),
        Some(t) if INTEGERS.contains(&t) => "0".into(),
        Some("f32" | "f64") => "0.0".into(),
        Some("String") => "String::new()".into(),
        Some(t) if t.starts_with('&') && t.ends_with("str") => "\"\"".into(),
        Some(t) if t.starts_with("Option<") && !e.starts_with("None") => "None".into(),
        Some(t) if result_like(t) => err_or_ok(e),
        Some(t) if t.ends_with("Ordering") => {
            ordering(e).unwrap_or_else(|| format!("({e}).reverse()"))
        }
        Some(t) if t.starts_with("Vec<") => "Vec::new()".into(),
        Some("()") => "()".into(),
        Some(_) => "Default::default()".into(),
        None => {
            if boolean(e) {
                format!("!({e})")
            } else if e.starts_with("Some(") {
                "None".into()
            } else if e.starts_with("Ok(") || e.starts_with("Err(") {
                err_or_ok(e)
            } else if let Some(o) = ordering(e) {
                o
            } else if e.starts_with("vec![") || e.starts_with("Vec::new()") {
                "Vec::new()".into()
            } else if e.starts_with("String::from(")
                || e.starts_with("format!(")
                || e.ends_with(".to_string()")
                || e.ends_with(".to_owned()")
            {
                "String::new()".into()
            } else if e.ends_with(".len()")
                || e.ends_with(".count()")
                || e.ends_with(".leading_zeros()")
                || e.contains("size_of::<")
                || INTEGERS.iter().any(|t| e.ends_with(&format!(" as {t}")))
            {
                "0".into()
            } else if [".trim()", ".trim_start()", ".trim_end()", ".as_str()"]
                .iter()
                .any(|m| e.ends_with(m))
                || e.contains(".trim_start_matches(")
                || e.contains(".trim_end_matches(")
            {
                "\"\"".into()
            } else {
                "Default::default()".into()
            }
        }
    }
}

/// `0`, `42u8`, `1_000` -> the digits and the suffix.
fn integer_literal(e: &str) -> Option<(&str, &str)> {
    let digits = e.len()
        - e.trim_start_matches(|c: char| c.is_ascii_digit() || c == '_')
            .len();
    if digits == 0 || !e.as_bytes()[0].is_ascii_digit() {
        return None;
    }
    let suffix = &e[digits..];
    (suffix.is_empty() || INTEGERS.contains(&suffix)).then(|| (&e[..digits], suffix))
}

/// Whether an expression is a boolean by its shape: a comparison or logical
/// operator outside any brackets or closure, a negation, or a predicate call.
fn boolean(e: &str) -> bool {
    let block_like = [
        "|", "move ", "if ", "match ", "loop", "unsafe", "{", "async", "while ", "for ",
    ];
    if block_like.iter().any(|k| e.starts_with(k)) {
        return false;
    }
    if e.starts_with('!') && !e.starts_with("!=") {
        return true;
    }
    let mut depth = 0i32;
    let bytes = e.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            _ if depth == 0 => {
                let rest = &e[i..];
                if ["==", "!=", "<=", ">=", "&&", "||", " < ", " > "]
                    .iter()
                    .any(|op| rest.starts_with(op))
                {
                    return true;
                }
            }
            _ => {}
        }
    }
    let last_call = e.rsplit('.').next().unwrap_or("");
    [
        "is_",
        "contains(",
        "starts_with(",
        "ends_with(",
        "eq(",
        "matches(",
    ]
    .iter()
    .any(|p| last_call.starts_with(p))
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

#[cfg(test)]
mod replacement_tests {
    use super::*;

    #[test]
    fn a_value_is_replaced_by_a_different_one_of_its_type() {
        assert_eq!(replacement("false", Some("bool")), "true");
        assert_eq!(replacement("a == b", Some("bool")), "!(a == b)");
        assert_eq!(replacement("0", None), "1");
        assert_eq!(replacement("42u8", None), "0u8");
        assert_eq!(
            replacement("Ok(())", Some("fmt::Result")),
            "Err(Default::default())"
        );
        assert_eq!(
            replacement("write!(f, \"x\")", Some("fmt::Result")),
            "Err(Default::default())"
        );
        assert_eq!(
            replacement("Err(e)", Some("Result<u64, Error>")),
            "Ok(Default::default())"
        );
        assert_eq!(
            replacement("Ordering::Less", Some("Ordering")),
            "Ordering::Greater"
        );
        assert_eq!(
            replacement("a.cmp(&b)", Some("cmp::Ordering")),
            "(a.cmp(&b)).reverse()"
        );
        assert_eq!(replacement("Some(x)", Some("Option<u8>")), "None");
        assert_eq!(replacement("text", Some("&str")), "\"\"");
        assert_eq!(replacement("name.len()", None), "0");
        assert_eq!(
            replacement("bits - len.leading_zeros() as usize", None),
            "0"
        );
        assert_eq!(replacement("text.trim_start_matches(' ')", None), "\"\"");
        assert_eq!(
            replacement("if a && b { 1 } else { 2 }", None),
            "Default::default()"
        );
        assert_eq!(
            replacement("self.inner", Some("Version")),
            "Default::default()"
        );
        assert_eq!(replacement("|x| x == 1", None), "Default::default()");
    }

    #[test]
    fn a_block_value_takes_the_function_type_only_when_it_is_returned() {
        let source = "fn f(x: u8) -> bool {\n    if x > 1 {\n        x == 2\n    } else {\n        false\n    }\n}\nfn g() -> u8 {\n    let y = {\n        3\n    };\n    y\n}\n";
        let r = replacements(source);
        assert_eq!(r[&3][0].1, "!(x == 2)");
        assert_eq!(r[&5][0].1, "true");
        // A block assigned to `y` is not the function's value.
        assert_eq!(r[&10][0].1, "0");
        assert_eq!(r[&12][0].1, "0");
    }
}
