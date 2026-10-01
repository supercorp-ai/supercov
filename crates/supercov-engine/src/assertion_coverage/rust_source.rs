//! Rust: statements by rust-analyzer's parser, tests by function name and
//! doctests by the line of their code block.
use super::{Change, Located, Starts, line_of, line_starts};
use ra_ap_syntax::{
    AstNode, Edition, SourceFile,
    ast::{self, BinaryOp},
    ast::{HasArgList, HasGenericParams, HasName, HasVisibility},
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
            // A loop that is the function's value returns from inside it:
            // skipping it would leave the function without one.
            let change = match &tail {
                ast::Expr::LoopExpr(_) if tail_type(&list).is_some_and(|t| t != "()") => {
                    Change::ReturnUndefined
                }
                _ => expression(&tail, true),
            };
            push(tail.syntax(), Some(change));
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
/// function's signature or the `let`'s annotation, else from the crate's own
/// signatures, else from the expression's shape.
pub(super) fn replacements(file: &str, source: &str, signatures: &Signatures) -> Replacements {
    let root = parse(source).tree();
    let starts = line_starts(source);
    let crate_view = Crate {
        signatures,
        module: module_of(file),
        source,
        locals: BTreeMap::new(),
    };
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
                        let pattern = s.pat().map(|p| text(p.syntax())).unwrap_or_default();
                        // `let Some(x) = e else { .. }`: the value that makes
                        // the `else` run.
                        let changed = if s.let_else().is_some() && pattern.starts_with("Some(") {
                            "None".to_owned()
                        } else {
                            typed(&value, ty.as_deref(), &crate_view)
                        };
                        push(s.syntax(), changed);
                    }
                }
                ast::Stmt::ExprStmt(s) => {
                    if let Some(e) = s.expr() {
                        value_change(&e, &mut push, None, &crate_view);
                    }
                }
                ast::Stmt::Item(_) => {}
            }
        }
        if let Some(tail) = list.tail_expr() {
            value_change(&tail, &mut push, Some(&list), &crate_view);
        }
    }
    out
}

fn value_change(
    e: &ast::Expr,
    push: &mut impl FnMut(&ra_ap_syntax::SyntaxNode, String),
    tail_of: Option<&ast::StmtList>,
    crate_view: &Crate,
) {
    match e {
        ast::Expr::ReturnExpr(r) => {
            if let Some(value) = r.expr() {
                let ty = return_type(r.syntax());
                push(r.syntax(), typed(&value, ty.as_deref(), crate_view));
            }
        }
        ast::Expr::BinExpr(b) if matches!(b.op_kind(), Some(BinaryOp::Assignment { op: None })) => {
            if let Some(rhs) = b.rhs() {
                push(b.syntax(), typed(&rhs, None, crate_view));
            }
        }
        // A loop that is the function's value -- it returns from inside --
        // is replaced whole by another value of the function's type.
        ast::Expr::LoopExpr(_) => {
            if let Some(ty) = tail_of.and_then(tail_type).filter(|t| t != "()") {
                let value =
                    value_of(&ty, crate_view, 0).unwrap_or_else(|| "Default::default()".to_owned());
                push(e.syntax(), value);
            }
        }
        ast::Expr::IfExpr(_) | ast::Expr::ForExpr(_) | ast::Expr::WhileExpr(_) => {}
        _ => {
            if let Some(list) = tail_of {
                let ty = tail_type(list);
                push(e.syntax(), typed(e, ty.as_deref(), crate_view));
            }
        }
    }
}

/// What the crate itself says about types: each function's return type, by
/// name; each enum's unit variants; each struct's fields; and the constants
/// and constructors that make a value of each type. A function name two
/// functions share with different return types says nothing.
#[derive(Debug, Default)]
pub(super) struct Signatures {
    returns: BTreeMap<String, Option<String>>,
    enums: BTreeMap<String, Vec<String>>,
    /// Each struct's named fields, their types, and whether code outside
    /// the struct's module can set them all.
    structs: BTreeMap<String, Vec<(String, String)>>,
    open_structs: BTreeMap<String, bool>,
    /// The module each type is defined in, `crate::a::b`.
    homes: BTreeMap<String, String>,
    makers: BTreeMap<String, Vec<Maker>>,
}

/// One way the crate makes a value of a type: an associated constant, or a
/// constructor and the types of its parameters.
#[derive(Debug, Clone)]
struct Maker {
    name: String,
    parameters: Option<Vec<String>>,
    module: String,
    public: bool,
}

/// The module a source file is, from its path: `src/lib.rs` is the crate,
/// `src/a/mod.rs` and `src/a.rs` are `crate::a`.
fn module_of(file: &str) -> String {
    let path = file.rsplit_once("src/").map_or(file, |(_, rest)| rest);
    let path = path.trim_end_matches(".rs");
    let mut parts = path.split('/').collect::<Vec<_>>();
    if matches!(parts.last(), Some(&"mod" | &"lib" | &"main")) {
        parts.pop();
    }
    std::iter::once("crate")
        .chain(parts.into_iter().filter(|p| !p.is_empty()))
        .collect::<Vec<_>>()
        .join("::")
}

pub(super) fn signatures<'a>(sources: impl Iterator<Item = (&'a str, &'a str)>) -> Signatures {
    let mut out = Signatures::default();
    let words = |t: &str| t.split_whitespace().collect::<String>();
    for (file, source) in sources {
        // A test's helpers are not the crate's API, and share its names.
        if test_path(file) {
            continue;
        }
        let module = module_of(file);
        let root = parse(source).tree();
        for node in root.syntax().descendants() {
            let owner = node
                .ancestors()
                .skip(1)
                .find_map(ast::Impl::cast)
                .filter(|i| i.trait_().is_none())
                .and_then(|i| i.self_ty())
                .map(|t| words(&t.syntax().text().to_string()));
            let resolve = |ty: &str| match &owner {
                Some(owner) => replace_word(&words(ty), "Self", owner),
                None => words(ty),
            };
            if let Some(f) = ast::Fn::cast(node.clone()) {
                let (Some(name), Some(ty)) = (f.name(), f.ret_type().and_then(|r| r.ty())) else {
                    continue;
                };
                let ty = resolve(&ty.syntax().text().to_string());
                out.returns
                    .entry(name.text().to_string())
                    .and_modify(|known| {
                        if known.as_deref() != Some(ty.as_str()) {
                            *known = None;
                        }
                    })
                    .or_insert(Some(ty.clone()));
                // A constructor: an associated function of the type, taking
                // no `self`, returning the type, safe to call.
                let Some(owner) = &owner else { continue };
                let takes_self = f.param_list().is_some_and(|p| p.self_param().is_some());
                if *owner != ty
                    || takes_self
                    || f.unsafe_token().is_some()
                    || f.generic_param_list().is_some()
                {
                    continue;
                }
                let parameters = f
                    .param_list()
                    .map(|list| {
                        list.params()
                            .filter_map(|p| p.ty())
                            .map(|t| resolve(&t.syntax().text().to_string()))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                out.makers.entry(owner.clone()).or_default().push(Maker {
                    name: name.text().to_string(),
                    parameters: Some(parameters),
                    module: module.clone(),
                    public: f.visibility().is_some(),
                });
            } else if let Some(c) = ast::Const::cast(node.clone()) {
                let (Some(owner), Some(name), Some(ty)) = (&owner, c.name(), c.ty()) else {
                    continue;
                };
                if resolve(&ty.syntax().text().to_string()) == *owner {
                    out.makers.entry(owner.clone()).or_default().push(Maker {
                        name: name.text().to_string(),
                        parameters: None,
                        module: module.clone(),
                        public: c.visibility().is_some(),
                    });
                }
            } else if let Some(e) = ast::Enum::cast(node.clone()) {
                let Some(name) = e.name() else { continue };
                let units = e
                    .variant_list()
                    .map(|list| {
                        list.variants()
                            .filter(|v| v.field_list().is_none())
                            .filter_map(|v| v.name().map(|n| n.text().to_string()))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                out.enums.insert(name.text().to_string(), units);
                out.homes.insert(name.text().to_string(), module.clone());
            } else if let Some(s) = ast::Struct::cast(node) {
                let Some(name) = s.name() else { continue };
                let open = match s.field_list() {
                    Some(ast::FieldList::RecordFieldList(list)) => {
                        list.fields().all(|f| f.visibility().is_some())
                    }
                    _ => false,
                };
                out.open_structs.insert(name.text().to_string(), open);
                let fields = match s.field_list() {
                    Some(ast::FieldList::RecordFieldList(list)) => list
                        .fields()
                        .filter_map(|f| {
                            Some((
                                f.name()?.text().to_string(),
                                words(&f.ty()?.syntax().text().to_string()),
                            ))
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                out.structs.insert(name.text().to_string(), fields);
                out.homes.insert(name.text().to_string(), module.clone());
            }
        }
    }
    out
}

/// The crate's signatures, seen from one of its files, and the types of the
/// parameters in scope where a value changes.
#[derive(Clone)]
pub(super) struct Crate<'a> {
    signatures: &'a Signatures,
    module: String,
    source: &'a str,
    locals: BTreeMap<String, String>,
}

impl Crate<'_> {
    /// How this file names a type the crate defines: as written where the
    /// file already uses the name or defines it, else by its full path.
    fn name(&self, ty: &str) -> String {
        let home = self.signatures.homes.get(ty);
        if home.is_none_or(|h| *h == self.module) || contains_word(self.source, ty) {
            return ty.to_owned();
        }
        format!("{}::{ty}", home.expect("checked"))
    }
}

fn contains_word(text: &str, word: &str) -> bool {
    text.match_indices(word).any(|(at, _)| {
        let before = text[..at].chars().last();
        let after = text[at + word.len()..].chars().next();
        let part = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
        !part(before) && !part(after)
    })
}

/// `text` with every whole-word `from` replaced by `to`.
fn replace_word(text: &str, from: &str, to: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find(from) {
        let before = rest[..at].chars().last();
        let after = rest[at + from.len()..].chars().next();
        let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
        out.push_str(&rest[..at]);
        out.push_str(if word(before) || word(after) {
            from
        } else {
            to
        });
        rest = &rest[at + from.len()..];
    }
    out.push_str(rest);
    out
}

/// The type of an expression, where the crate's own signatures say it: a
/// call to one of its functions or methods, through `?`, parentheses and an
/// `unsafe` block; or a `match` or `if` whose arms say it.
fn infer(e: &ast::Expr, crate_view: &Crate) -> Option<String> {
    let signatures = crate_view.signatures;
    let returns = |name: &str| signatures.returns.get(name).cloned().flatten();
    match e {
        ast::Expr::ParenExpr(p) => infer(&p.expr()?, crate_view),
        ast::Expr::TryExpr(t) => unwrap_try(&infer(&t.expr()?, crate_view)?),
        ast::Expr::PathExpr(p) => crate_view
            .locals
            .get(&p.syntax().text().to_string())
            .cloned(),
        ast::Expr::TupleExpr(t) => {
            let types = t
                .fields()
                .map(|f| infer(&f, crate_view).or_else(|| shape_type(&f)))
                .collect::<Vec<_>>();
            (types.len() > 1 && types.iter().any(Option::is_some)).then(|| {
                let types = types
                    .into_iter()
                    .map(|t| t.unwrap_or_else(|| "_".into()))
                    .collect::<Vec<_>>();
                format!("({})", types.join(","))
            })
        }
        ast::Expr::CallExpr(c) => match c.expr()? {
            ast::Expr::PathExpr(p) => {
                let path = p.path()?;
                let name = path.segment()?.name_ref()?.text().to_string();
                if matches!(name.as_str(), "alloc" | "alloc_zeroed" | "realloc") {
                    return Some("*mut u8".into());
                }
                if path.qualifier().is_none() {
                    // `Some(x)`; a closure parameter called, `impl Fn() -> T`.
                    if name == "Some" {
                        let argument = c.arg_list()?.args().next()?;
                        return Some(format!("Option<{}>", infer(&argument, crate_view)?));
                    }
                    if let Some(local) = crate_view.locals.get(&name) {
                        return local.rsplit_once("->").map(|(_, r)| r.to_owned());
                    }
                }
                returns(&name).or_else(|| {
                    // `Layout::from_size_align(..)`, `Version::new(..)`: a
                    // type's constructors are named for what they make.
                    let owner = path.qualifier()?.syntax().text().to_string();
                    let made =
                        name == "new" || name.starts_with("from_") || name.starts_with("with_");
                    (made && owner.rsplit("::").next()?.starts_with(char::is_uppercase))
                        .then(|| owner.rsplit("::").next().unwrap_or(&owner).to_owned())
                })
            }
            _ => None,
        },
        ast::Expr::MethodCallExpr(m) => {
            let name = m.name_ref()?.text().to_string();
            let receiver = m.receiver()?;
            let receiver_text = receiver.syntax().text().to_string();
            // Raw pointers: a pointer moved is the same pointer type; a
            // cast names its target; a read gives what it points at.
            let pointer = || -> Option<String> {
                let ty = infer(&receiver, crate_view).or_else(|| {
                    if receiver_text.starts_with("ptr::addr_of!(")
                        || receiver_text.ends_with(".as_ptr()")
                    {
                        Some("*const u8".into())
                    } else if receiver_text.starts_with("ptr::addr_of_mut!(")
                        || receiver_text.ends_with(".as_mut_ptr()")
                    {
                        Some("*mut u8".into())
                    } else {
                        None
                    }
                })?;
                ty.starts_with('*').then_some(ty)
            };
            match name.as_str() {
                "add" | "sub" | "offset" | "wrapping_add" | "wrapping_sub" | "byte_add" => {
                    if let Some(p) = pointer() {
                        return Some(p);
                    }
                }
                "cast" => {
                    let target = m
                        .syntax()
                        .children()
                        .find_map(ast::GenericArgList::cast)?
                        .syntax()
                        .text()
                        .to_string();
                    let target = target
                        .trim_start_matches("::")
                        .trim_start_matches('<')
                        .trim_end_matches('>');
                    let kind = if pointer()?.starts_with("*mut") {
                        "*mut"
                    } else {
                        "*const"
                    };
                    return Some(format!(
                        "{kind} {}",
                        target.split_whitespace().collect::<String>()
                    ));
                }
                "read" | "read_unaligned" => {
                    if let Some(p) = pointer() {
                        return Some(
                            p.trim_start_matches("*const")
                                .trim_start_matches("*mut")
                                .trim()
                                .to_owned(),
                        );
                    }
                }
                _ => {}
            }
            returns(&name)
        }
        ast::Expr::BlockExpr(b) => infer(&b.stmt_list()?.tail_expr()?, crate_view),
        ast::Expr::MatchExpr(m) => {
            let arms = m.match_arm_list()?.arms().collect::<Vec<_>>();
            // `Ok(x) => x`: what the scrutinee holds when it is `Ok`.
            let unwraps = arms.iter().any(|arm| {
                let pattern = arm
                    .pat()
                    .map(|p| p.syntax().text().to_string())
                    .unwrap_or_default();
                let value = arm
                    .expr()
                    .map(|e| e.syntax().text().to_string())
                    .unwrap_or_default();
                pattern
                    .strip_prefix("Ok(")
                    .and_then(|p| p.strip_suffix(')'))
                    == Some(value.as_str())
            });
            if unwraps
                && let Some(ty) = m.expr().and_then(|s| infer(&s, crate_view))
                && let Some(inner) = unwrap_try(&ty)
            {
                return Some(inner);
            }
            best(
                arms.iter()
                    .filter_map(|arm| arm.expr())
                    .filter_map(|arm| infer(&arm, crate_view).or_else(|| shape_type(&arm))),
            )
        }
        ast::Expr::IfExpr(i) => {
            let branches = [
                i.then_branch(),
                i.else_branch().and_then(|b| match b {
                    ast::ElseBranch::Block(b) => Some(b),
                    ast::ElseBranch::IfExpr(_) => None,
                }),
            ];
            let mut tails = branches
                .into_iter()
                .flatten()
                .filter_map(|b| b.stmt_list()?.tail_expr())
                .collect::<Vec<_>>();
            // `else if`: its branches are this one's too.
            let mut chained = i.else_branch();
            while let Some(ast::ElseBranch::IfExpr(next)) = chained {
                tails.extend(next.then_branch().and_then(|b| b.stmt_list()?.tail_expr()));
                chained = next.else_branch();
                if let Some(ast::ElseBranch::Block(b)) = &chained {
                    tails.extend(b.stmt_list().and_then(|l| l.tail_expr()));
                }
            }
            best(
                tails
                    .iter()
                    .filter_map(|tail| infer(tail, crate_view).or_else(|| shape_type(tail))),
            )
        }
        _ => None,
    }
}

/// The type an expression's shape says it has: a literal, or what
/// [`replacement`] reads from a call (`.trim()` is text, `.is_empty()` a
/// boolean).
fn shape_type(e: &ast::Expr) -> Option<String> {
    let text = e.syntax().text().to_string();
    let text = text.trim();
    if ["Less", "Equal", "Greater"]
        .iter()
        .any(|v| text == format!("Ordering::{v}") || text.ends_with(&format!("::Ordering::{v}")))
    {
        return Some("Ordering".into());
    }
    if text.starts_with('"') {
        return Some("&str".into());
    }
    match replacement(text, None).as_str() {
        "true" | "false" => Some("bool".into()),
        r if r.starts_with("!(") => Some("bool".into()),
        "\"\"" => Some("&str".into()),
        "String::new()" => Some("String".into()),
        _ => None,
    }
}

/// Of several readings of one value's type, the one that knows most.
fn best(types: impl Iterator<Item = String>) -> Option<String> {
    types.min_by_key(|t| t.matches('_').count())
}

/// `Result<T, E>` or `Option<T>` -> `T`: what `?` leaves.
fn unwrap_try(ty: &str) -> Option<String> {
    let (head, arguments) = generic(ty)?;
    if !(head.ends_with("Result") || head.ends_with("Option")) {
        return None;
    }
    arguments.into_iter().next()
}

/// `Result<T, E>` -> (`Result`, [`T`, `E`]).
fn generic(ty: &str) -> Option<(&str, Vec<String>)> {
    let open = ty.find('<')?;
    if !ty.ends_with('>') {
        return None;
    }
    Some((&ty[..open], top_level(&ty[open + 1..ty.len() - 1], ',')))
}

/// `text` split at each `separator` outside brackets.
fn top_level(text: &str, separator: char) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut current = String::new();
    for c in text.chars() {
        match c {
            '(' | '[' | '{' | '<' => depth += 1,
            ')' | ']' | '}' | '>' => depth -= 1,
            _ => {}
        }
        if c == separator && depth == 0 {
            parts.push(current.trim().to_owned());
            current.clear();
        } else {
            current.push(c);
        }
    }
    if !current.trim().is_empty() {
        parts.push(current.trim().to_owned());
    }
    parts
}

/// The text inside `open`..`close` when they enclose the whole of `text`.
fn enclosed(text: &str, open: char, close: char) -> Option<&str> {
    let inner = text.trim().strip_prefix(open)?.strip_suffix(close)?;
    let mut depth = 0i32;
    for c in inner.chars() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            _ => {}
        }
        if depth < 0 {
            return None;
        }
    }
    Some(inner)
}

/// A tuple type or expression's elements, or nothing if it is not one.
fn tuple(text: &str) -> Option<Vec<String>> {
    let parts = top_level(enclosed(text, '(', ')')?, ',');
    (parts.len() > 1).then_some(parts)
}

/// A value change for an expression: [`replacement`] where it knows the
/// type, else from the crate's own signatures, a struct's fields, a tuple's
/// elements, an enum's other variants, a closure's value or the
/// expression's shape.
fn typed(e: &ast::Expr, ty: Option<&str>, crate_view: &Crate) -> String {
    let crate_view = &scoped(e, crate_view);
    let ty = ty
        .map(|t| resolve_self(&t.split_whitespace().collect::<String>(), e.syntax()))
        .or_else(|| infer(e, crate_view));
    let text = e.syntax().text().to_string();
    // A closure's type is a callable one, `impl Fn(u32) -> u32`; its value is
    // what its body returns, never the closure itself.
    if let ast::Expr::ClosureExpr(c) = e
        && let Some(changed) = closure(c, ty.as_deref().and_then(callable_output), crate_view)
    {
        return changed;
    }
    if let Some(changed) = changed(&text, ty.as_deref(), crate_view) {
        return changed;
    }
    let structural = match e {
        ast::Expr::RecordExpr(r) => record(r, crate_view),
        _ => None,
    };
    structural
        .or_else(|| value_of(ty.as_deref()?, crate_view, 0).filter(|v| *v != text.trim()))
        .unwrap_or_else(|| replacement(&text, ty.as_deref()))
}

/// The crate seen from `e`: with the types of the parameters of the
/// functions and closures around it.
fn scoped<'a>(e: &ast::Expr, crate_view: &Crate<'a>) -> Crate<'a> {
    let mut out = crate_view.clone();
    out.locals.clear();
    let words = |t: &str| t.split_whitespace().collect::<String>();
    let at = e.syntax().text_range().start();
    // Outermost first, so an inner binding shadows an outer one.
    let ancestors = e.syntax().ancestors().collect::<Vec<_>>();
    for ancestor in ancestors.into_iter().rev() {
        let params = if let Some(f) = ast::Fn::cast(ancestor.clone()) {
            f.param_list()
        } else if let Some(c) = ast::ClosureExpr::cast(ancestor.clone()) {
            c.param_list()
        } else if let Some(list) = ast::StmtList::cast(ancestor.clone()) {
            // The `let`s before `e`, typed by annotation or by what they
            // are given.
            for statement in list.statements() {
                if statement.syntax().text_range().end() > at {
                    break;
                }
                let ast::Stmt::LetStmt(s) = statement else {
                    continue;
                };
                let (Some(pat), Some(init)) = (s.pat(), s.initializer()) else {
                    continue;
                };
                let ty = match s.ty() {
                    Some(t) => Some(resolve_self(
                        &words(&t.syntax().text().to_string()),
                        e.syntax(),
                    )),
                    None => infer(&init, &out),
                };
                if let Some(ty) = ty {
                    bind(&pat, &ty, &mut out.locals);
                }
            }
            continue;
        } else {
            continue;
        };
        for param in params.iter().flat_map(|list| list.params()) {
            let (Some(pat), Some(ty)) = (param.pat(), param.ty()) else {
                continue;
            };
            let ty = resolve_self(&words(&ty.syntax().text().to_string()), e.syntax());
            bind(&pat, &ty, &mut out.locals);
        }
    }
    out
}

/// A pattern's names with their types: `x`, `mut x`, `(a, b)`.
fn bind(pat: &ast::Pat, ty: &str, locals: &mut BTreeMap<String, String>) {
    match pat {
        ast::Pat::IdentPat(p) => {
            if let Some(name) = p.name() {
                locals.insert(name.text().to_string(), ty.to_owned());
            }
        }
        ast::Pat::TuplePat(t) => {
            if let Some(types) = tuple(ty) {
                let fields = t.fields().collect::<Vec<_>>();
                if fields.len() == types.len() {
                    for (field, ty) in fields.iter().zip(&types) {
                        bind(field, ty, locals);
                    }
                }
            }
        }
        _ => {}
    }
}

/// `Self` and `Self::Err` as the impl around `node` defines them.
fn resolve_self(ty: &str, node: &ra_ap_syntax::SyntaxNode) -> String {
    if !ty.contains("Self") {
        return ty.to_owned();
    }
    let Some(imp) = node.ancestors().find_map(ast::Impl::cast) else {
        return ty.to_owned();
    };
    let words = |t: &str| t.split_whitespace().collect::<String>();
    let mut out = ty.to_owned();
    for item in imp.assoc_item_list().iter().flat_map(|l| l.assoc_items()) {
        if let ast::AssocItem::TypeAlias(alias) = item
            && let (Some(name), Some(value)) = (alias.name(), alias.ty())
        {
            out = out.replace(
                &format!("Self::{}", name.text()),
                &words(&value.syntax().text().to_string()),
            );
        }
    }
    match imp.self_ty() {
        Some(owner) => replace_word(&out, "Self", &words(&owner.syntax().text().to_string())),
        None => out,
    }
}

/// A different value for an expression known by its text and, perhaps, its
/// type; nothing where only `Default::default()` would do.
fn changed(e: &str, ty: Option<&str>, crate_view: &Crate) -> Option<String> {
    let e = e.trim();
    let local = crate_view.locals.get(e).cloned();
    let ty = ty.or(local.as_deref());
    let first = replacement(e, ty);
    if !first.contains("Default::default()") {
        return Some(first);
    }
    if let Some(swapped) = result_swap(e, ty, crate_view) {
        return Some(swapped);
    }
    if let Some(variant) = other_variant(e, ty, crate_view) {
        return Some(variant);
    }
    if e == "None"
        && let Some(inner) = ty
            .and_then(generic)
            .filter(|(head, _)| head.ends_with("Option"))
            .and_then(|(_, args)| args.into_iter().next())
    {
        return value_of(&inner, crate_view, 0).map(|v| format!("Some({v})"));
    }
    let types = ty.and_then(tuple);
    // A tuple written out: one element changed in place.
    if let Some(elements) = tuple(e) {
        for (i, element) in elements.iter().enumerate() {
            let element_ty = types
                .as_ref()
                .filter(|t| t.len() == elements.len())
                .map(|t| t[i].as_str())
                .filter(|t| *t != "_")
                .or_else(|| crate_view.locals.get(element.as_str()).map(String::as_str));
            if let Some(changed) = changed(element, element_ty, crate_view)
                && changed != *element
            {
                let mut out = elements.clone();
                out[i] = changed;
                return Some(format!("({})", out.join(", ")));
            }
        }
        return None;
    }
    // A tuple computed: computed, then one element changed.
    if let Some(types) = types {
        for (i, element_ty) in types.iter().enumerate().filter(|(_, t)| *t != "_") {
            let field = format!("v.{i}");
            let changed = if INTEGERS.contains(&element_ty.as_str()) {
                Some(format!("{field} ^ 1"))
            } else {
                changed(&field, Some(element_ty), crate_view)
            };
            if let Some(changed) = changed {
                return Some(format!("{{ let mut v = {e}; v.{i} = {changed}; v }}"));
            }
        }
        return None;
    }
    // `[x; n]` and `[a, b]`: the first element changed.
    if let Some(inner) = enclosed(e, '[', ']') {
        let repeat = top_level(inner, ';');
        if repeat.len() == 2 {
            return changed(&repeat[0], None, crate_view).map(|x| format!("[{x}; {}]", repeat[1]));
        }
        let elements = top_level(inner, ',');
        if let Some(first) = elements.first()
            && let Some(x) = changed(first, None, crate_view)
        {
            let mut out = elements.clone();
            out[0] = x;
            return Some(format!("[{}]", out.join(", ")));
        }
        return None;
    }
    let method = last_method(e);
    if e.starts_with("Ord::cmp(") || method == Some("cmp") {
        return Some(format!("({e}).reverse()"));
    }
    if e.starts_with("PartialOrd::partial_cmp(") || method == Some("partial_cmp") {
        return Some(format!("({e}).map(std::cmp::Ordering::reverse)"));
    }
    // A slice: the empty one.
    if e.starts_with('&')
        && e.ends_with(']')
        && let Some(open) = e.rfind('[')
        && e[open..].contains("..")
    {
        return Some(format!("{}[..0]", &e[..open]));
    }
    // Text read from a string: read from the empty one.
    if let Some((_, rest)) = e.split_once(".as_str().") {
        return Some(format!("\"\".{rest}"));
    }
    if e.ends_with(".chars().next().unwrap()") || e.ends_with(".chars().next_back().unwrap()") {
        return Some("'\\0'".into());
    }
    let integer_local = top_level_operands(e).iter().any(|operand| {
        crate_view
            .locals
            .get(operand.as_str())
            .is_some_and(|t| INTEGERS.contains(&t.as_str()))
    });
    if ty.is_some_and(|t| INTEGERS.contains(&t)) || integer_shaped(e) || integer_local {
        return Some(format!("({e}) ^ 1"));
    }
    None
}

/// The operands of arithmetic outside brackets: `a - b * c` -> `a`, `b`, `c`;
/// nothing for an expression with no such operator.
fn top_level_operands(e: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut last = 0usize;
    let bytes = e.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            _ if depth == 0 => {
                if let Some(op) = [" - ", " * ", " / ", " % ", " + "]
                    .iter()
                    .find(|op| e.get(i..).is_some_and(|rest| rest.starts_with(**op)))
                {
                    parts.push(e[last..i].trim().to_owned());
                    i += op.len();
                    last = i;
                    continue;
                }
            }
            _ => {}
        }
        i += 1;
    }
    if parts.is_empty() {
        return parts;
    }
    parts.push(e[last..].trim().to_owned());
    parts
}

/// The method an expression ends by calling: `a.b().cmp(&c)` -> `cmp`.
fn last_method(e: &str) -> Option<&str> {
    let body = e.strip_suffix(')')?;
    let mut depth = 1i32;
    let mut open = None;
    for (i, c) in body.char_indices().rev() {
        match c {
            ')' | ']' | '}' => depth += 1,
            '(' | '[' | '{' => {
                depth -= 1;
                if depth == 0 {
                    open = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let head = &body[..open?];
    let name_start = head
        .rfind(|c: char| !(c.is_alphanumeric() || c == '_'))
        .map_or(0, |i| i + 1);
    head[..name_start]
        .ends_with('.')
        .then(|| &head[name_start..])
}

/// `Ok(x)` -> `Err(e)` and `Err(e)` -> `Ok(x)`, with a value the crate can
/// make for the other side.
fn result_swap(e: &str, ty: Option<&str>, crate_view: &Crate) -> Option<String> {
    let ty = ty?;
    let (ok, err) = if ty == "fmt::Result" || ty == "std::fmt::Result" {
        ("()".to_owned(), "fmt::Error".to_owned())
    } else {
        let (head, arguments) = generic(ty)?;
        if !head.ends_with("Result") || arguments.len() != 2 {
            return None;
        }
        (arguments[0].clone(), arguments[1].clone())
    };
    if e.starts_with("Err(") {
        value_of(&ok, crate_view, 0).map(|v| format!("Ok({v})"))
    } else {
        value_of(&err, crate_view, 0).map(|v| format!("Err({v})"))
    }
}

/// Another variant of an enum the crate defines: `Op::Exact` ->
/// `Op::Greater`, `ErrorKind::Leading(pos)` -> `ErrorKind::Empty`, or for a
/// value only known by its type, a `match` that turns each into another.
fn other_variant(e: &str, ty: Option<&str>, crate_view: &Crate) -> Option<String> {
    let enums = &crate_view.signatures.enums;
    let head = e.split(['(', '{']).next().unwrap_or(e).trim();
    if let Some((path, variant)) = head.rsplit_once("::")
        && variant.chars().all(|c| c.is_alphanumeric() || c == '_')
        && path
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == ':')
    {
        let name = path.rsplit("::").next()?;
        let name = if name == "Self" { ty? } else { name };
        if let Some(units) = enums.get(name) {
            return units
                .iter()
                .find(|v| **v != variant)
                .map(|other| format!("{path}::{other}"));
        }
    }
    let ty = ty?;
    let units = enums.get(ty).filter(|u| u.len() >= 2)?;
    let name = crate_view.name(ty);
    Some(format!(
        "match {e} {{ {name}::{} => {name}::{}, _ => {name}::{} }}",
        units[0], units[1], units[0]
    ))
}

/// A struct literal with one field changed, typed by the struct's own
/// definition.
fn record(r: &ast::RecordExpr, crate_view: &Crate) -> Option<String> {
    let path = r.path()?.syntax().text().to_string();
    let name = path.rsplit("::").next()?;
    let types = crate_view.signatures.structs.get(name)?;
    let list = r.record_expr_field_list()?;
    let fields = list
        .fields()
        .map(|f| {
            let name = f
                .field_name()
                .map(|n| n.text().to_string())
                .unwrap_or_default();
            let value = f
                .expr()
                .map_or_else(|| name.clone(), |e| e.syntax().text().to_string());
            (name, value)
        })
        .collect::<Vec<_>>();
    for (i, (field, value)) in fields.iter().enumerate() {
        let ty = types
            .iter()
            .find(|(n, _)| n == field)
            .map(|(_, t)| t.as_str());
        let Some(new) = changed(value, ty, crate_view)
            .or_else(|| value_of(ty?, crate_view, 0).filter(|v| v != value))
        else {
            continue;
        };
        let mut parts = fields
            .iter()
            .map(|(n, v)| {
                if n == v {
                    n.clone()
                } else {
                    format!("{n}: {v}")
                }
            })
            .collect::<Vec<_>>();
        parts[i] = format!("{field}: {new}");
        if let Some(spread) = list.spread() {
            parts.push(format!("..{}", spread.syntax().text()));
        }
        return Some(format!("{path} {{ {} }}", parts.join(", ")));
    }
    None
}

/// What a callable type returns: `T` of `impl Fn(A) -> T` or
/// `Box<dyn FnMut() -> T>`, whitespace already removed.
fn callable_output(ty: &str) -> Option<&str> {
    if !["Fn(", "FnMut(", "FnOnce("]
        .iter()
        .any(|callable| ty.contains(callable))
    {
        return None;
    }
    let mut output = ty.rsplit_once("->")?.1;
    // `Box<dynFn()->T>` leaves the box's own closing bracket behind.
    while output.matches('>').count() > output.matches('<').count() {
        output = &output[..output.len() - 1];
    }
    (!output.is_empty()).then_some(output)
}

/// A closure that returns a different value: of its declared return type,
/// else of `output`, what the callable type it is given as returns.
fn closure(c: &ast::ClosureExpr, output: Option<&str>, crate_view: &Crate) -> Option<String> {
    let body = c.body()?;
    let ty = c
        .ret_type()
        .and_then(|r| r.ty())
        .map(|t| t.syntax().text().to_string())
        .or_else(|| output.map(str::to_owned));
    let head_end = usize::from(body.syntax().text_range().start())
        - usize::from(c.syntax().text_range().start());
    let head = c.syntax().text().to_string()[..head_end].to_owned();
    let body_text = body.syntax().text().to_string();
    let new = changed(&body_text, ty.as_deref(), crate_view)?;
    Some(format!("{head}{new}"))
}

/// Some value of a type, built from what the crate and the standard library
/// provide; nothing for a type neither can make.
fn value_of(ty: &str, crate_view: &Crate, depth: u8) -> Option<String> {
    let ty = ty.split_whitespace().collect::<String>();
    let ty = ty.as_str();
    let last = ty.rsplit("::").next().unwrap_or(ty);
    if INTEGERS.contains(&ty) {
        return Some("0".into());
    }
    if let Some(zero) = last.strip_prefix("NonZero")
        && INTEGERS.iter().any(|i| i.eq_ignore_ascii_case(zero))
    {
        return Some(format!("std::num::{last}::MAX"));
    }
    match last {
        "bool" => return Some("false".into()),
        "char" => return Some("'\\0'".into()),
        "f32" | "f64" => return Some("0.0".into()),
        "String" => return Some("String::new()".into()),
        "()" => return Some("()".into()),
        "Ordering" => return Some("std::cmp::Ordering::Less".into()),
        "Layout" => return Some("std::alloc::Layout::new::<u8>()".into()),
        _ => {}
    }
    if ty == "fmt::Error" || ty == "std::fmt::Error" || ty == "core::fmt::Error" {
        return Some("std::fmt::Error".into());
    }
    if ty.starts_with('&') && ty.ends_with("str") {
        return Some("\"\"".into());
    }
    if ty.starts_with("*const") {
        return Some("std::ptr::null()".into());
    }
    if ty.starts_with("*mut") {
        return Some("std::ptr::null_mut()".into());
    }
    if let Some(inner) = enclosed(ty, '[', ']') {
        let parts = top_level(inner, ';');
        if parts.len() == 2 {
            return Some(format!(
                "[{}; {}]",
                value_of(&parts[0], crate_view, depth)?,
                parts[1]
            ));
        }
    }
    if let Some(elements) = tuple(ty) {
        let values = elements
            .iter()
            .map(|t| value_of(t, crate_view, depth))
            .collect::<Option<Vec<_>>>()?;
        return Some(format!("({})", values.join(", ")));
    }
    if let Some((head, arguments)) = generic(ty) {
        let head = head.rsplit("::").next().unwrap_or(head);
        return match head {
            "Option" => Some("None".into()),
            "Vec" => Some("Vec::new()".into()),
            "NonNull" => Some("std::ptr::NonNull::dangling()".into()),
            "Result" if arguments.len() == 2 => {
                value_of(&arguments[0], crate_view, depth).map(|v| format!("Ok({v})"))
            }
            _ => None,
        };
    }
    if depth > 2 {
        return None;
    }
    let signatures = crate_view.signatures;
    if let Some(first) = signatures.enums.get(last).and_then(|units| units.first()) {
        return Some(format!("{}::{first}", crate_view.name(last)));
    }
    let mut makers = signatures.makers.get(last).cloned().unwrap_or_default();
    makers.retain(|m| m.public || m.module == crate_view.module);
    if makers.is_empty() {
        // No constructor: a literal, where every field can be set from here.
        let fields = signatures.structs.get(last).filter(|f| !f.is_empty())?;
        let home = signatures.homes.get(last);
        let reachable =
            signatures.open_structs.get(last) == Some(&true) || home == Some(&crate_view.module);
        if !reachable {
            return None;
        }
        let values = fields
            .iter()
            .map(|(name, ty)| Some(format!("{name}: {}", value_of(ty, crate_view, depth + 1)?)))
            .collect::<Option<Vec<_>>>()?;
        return Some(format!(
            "{} {{ {} }}",
            crate_view.name(last),
            values.join(", ")
        ));
    }
    makers.sort_by_key(|m| m.parameters.as_ref().map_or(0, |p| p.len() + 1));
    makers.into_iter().find_map(|maker| {
        let owner = crate_view.name(last);
        match maker.parameters {
            None => Some(format!("{owner}::{}", maker.name)),
            Some(parameters) => {
                let arguments = parameters
                    .iter()
                    .map(|p| value_of(p, crate_view, depth + 1))
                    .collect::<Option<Vec<_>>>()?;
                Some(format!("{owner}::{}({})", maker.name, arguments.join(", ")))
            }
        }
    })
}

/// Whether an expression is integer arithmetic by its shape: an operator
/// outside brackets, with a length, an integer literal or an integer cast
/// among its operands, or an integer-only method.
fn integer_shaped(e: &str) -> bool {
    if e.contains('"') || e.contains('\'') || float_literal(e) {
        return false;
    }
    let methods = [
        ".wrapping_add(",
        ".wrapping_sub(",
        ".wrapping_mul(",
        ".rotate_left(",
        ".rotate_right(",
        ".count_ones()",
        ".leading_zeros()",
        ".trailing_zeros()",
    ];
    if methods.iter().any(|m| e.contains(m)) {
        return true;
    }
    let mut depth = 0i32;
    let mut operator = false;
    for (i, c) in e.char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            _ if depth == 0 => {
                // A byte inside a multi-byte character starts no operator.
                let Some(rest) = e.get(i..) else { continue };
                if [" - ", " * ", " / ", " % ", " << ", " >> ", " + ", " ^ "]
                    .iter()
                    .any(|op| rest.starts_with(op))
                {
                    operator = true;
                }
            }
            _ => {}
        }
    }
    let evidence = e.contains(".len()")
        || e.contains(".get()")
        || INTEGERS.iter().any(|t| e.contains(&format!(" as {t}")))
        || e.split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .any(|word| integer_literal(word).is_some());
    operator && evidence
}

/// Whether an expression holds a float literal (`0.5`, `1e3`).
fn float_literal(e: &str) -> bool {
    let bytes = e.as_bytes();
    bytes
        .windows(3)
        .any(|w| w[0].is_ascii_digit() && w[1] == b'.' && w[2].is_ascii_digit())
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
        // The value of a block, `match` or `if` is its arms', and an array's
        // its elements'; shape says nothing about them.
        None if [
            "match ", "if ", "loop", "unsafe ", "{", "while ", "for ", "async ", "[",
        ]
        .iter()
        .any(|k| e.starts_with(k)) =>
        {
            "Default::default()".into()
        }
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
                // A byte inside a multi-byte character starts no operator.
                let Some(rest) = e.get(i..) else { continue };
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

    /// dtolnay/semver's shapes that used to fall back to
    /// `Default::default()`, for types that have none: the crate's own
    /// signatures, tuples, enums, `let`-`else` and integer arithmetic.
    #[test]
    fn a_value_with_no_default_takes_one_from_the_crate() {
        let lib =
            "pub enum Op {\n    Exact,\n    Greater,\n    Wildcard,\n}\npub struct Version;\n";
        let source = "fn numeric_identifier(input: &str, pos: Position) -> Result<(u64, &str), Error> {\n    todo!()\n}\nfn dot(input: &str, pos: Position) -> Result<&str, Error> {\n    todo!()\n}\nfn op(input: &str) -> (Op, &str) {\n    if input.starts_with('=') {\n        (Op::Exact, &input[1..])\n    } else {\n        (Op::Wildcard, input)\n    }\n}\nfn minor(text: &str, pos: Position) -> Result<(Option<u64>, &str), Error> {\n    let (major, text) = numeric_identifier(text, pos)?;\n    let text = dot(text, pos)?;\n    let Some(minor) = cmp.minor else {\n        return Ok((None, text));\n    };\n    let size = bytes_for_varint(len) + len.get();\n    let diff = modified.wrapping_sub(original as usize);\n    let v = Version::parse(text);\n    Ok((Some(minor), text))\n}\n";
        let signatures = signatures([("src/lib.rs", lib), ("src/parse.rs", source)].into_iter());
        let r = replacements("src/parse.rs", source, &signatures);
        let at = |line: usize| r[&line][0].1.as_str();
        // The tuple a call returns, one element changed after it runs.
        assert_eq!(
            at(15),
            "{ let mut v = numeric_identifier(text, pos)?; v.0 = v.0 ^ 1; v }"
        );
        assert_eq!(at(16), "\"\"");
        assert_eq!(at(17), "None");
        // A tuple written out: an enum's other variant, in place.
        assert_eq!(at(9), "(Op::Greater, &input[1..])");
        assert_eq!(at(11), "(Op::Exact, input)");
        assert_eq!(at(20), "(bytes_for_varint(len) + len.get()) ^ 1");
        assert_eq!(at(21), "(modified.wrapping_sub(original as usize)) ^ 1");
        // A struct the crate gives no value for stays as it was.
        assert_eq!(at(22), "Default::default()");
    }

    /// The rest of semver's sample: values the crate can make (its error
    /// through `Error::new(ErrorKind::Empty)`, a version through its
    /// constructor), a struct literal, a closure, a comparison, a slice, a
    /// split, an array and a loop that is its function's value.
    #[test]
    fn a_value_the_crate_can_make_is_made_from_its_own_parts() {
        let error = "pub struct Error {\n    kind: ErrorKind,\n}\npub(crate) enum ErrorKind {\n    Empty,\n    UnexpectedEnd(Position),\n}\nimpl Error {\n    pub(crate) fn new(kind: ErrorKind) -> Self {\n        Error { kind }\n    }\n}\n";
        let lib = "pub struct Version {\n    pub major: u64,\n    pub minor: u64,\n}\nimpl Version {\n    pub const fn new(major: u64, minor: u64) -> Self {\n        Version { major, minor }\n    }\n}\npub enum Op {\n    Exact,\n    Greater,\n}\npub struct Comparator {\n    pub op: Op,\n    pub major: u64,\n}\n";
        let source = "fn version(text: &str) -> Result<Version, Error> {\n    if text.is_empty() {\n        return Err(Error::new(ErrorKind::Empty));\n    }\n    Ok(Version { major: 1, minor: 0 })\n}\nfn cmp(a: &Ident, b: &Ident) -> Ordering {\n    let lhs = a.as_str().split('.');\n    let string_cmp = || Ord::cmp(lhs, rhs);\n    let digit = |b: u8| b.is_ascii_digit();\n    let text = &text[1..];\n    let mut bytes = [0u8; mem::size_of::<Ident>()];\n    let comparator = Comparator {\n        op,\n        major,\n    };\n    error.kind = ErrorKind::UnexpectedEnd(pos);\n    Ord::cmp(&lhs.len(), &rhs.len())\n}\nfn minor(text: &str) -> (Option<u64>, &str) {\n    (None, text)\n}\nfn decode(ptr: *const u8) -> NonZeroUsize {\n    loop {\n        return x;\n    }\n}\n";
        let signatures = signatures(
            [
                ("src/error.rs", error),
                ("src/lib.rs", lib),
                ("src/parse.rs", source),
            ]
            .into_iter(),
        );
        let r = replacements("src/parse.rs", source, &signatures);
        let at = |line: usize| r[&line][0].1.as_str();
        assert_eq!(at(3), "Ok(Version::new(0, 0))");
        assert_eq!(at(5), "Err(Error::new(ErrorKind::Empty))");
        assert_eq!(at(8), "\"\".split('.')");
        assert_eq!(at(9), "|| (Ord::cmp(lhs, rhs)).reverse()");
        assert_eq!(at(10), "|b: u8| !(b.is_ascii_digit())");
        assert_eq!(at(11), "&text[..0]");
        assert_eq!(at(12), "[1u8; mem::size_of::<Ident>()]");
        assert_eq!(
            at(13),
            "Comparator { op: match op { crate::Op::Exact => crate::Op::Greater, _ => crate::Op::Exact }, major }",
            "this file never names `Op`, so it gets the enum's path"
        );
        assert_eq!(at(17), "ErrorKind::Empty");
        assert_eq!(at(18), "(Ord::cmp(&lhs.len(), &rhs.len())).reverse()");
        assert_eq!(at(21), "(Some(0), text)");
        assert_eq!(at(24), "std::num::NonZeroUsize::MAX");
        // A loop that is its function's value is a value, not skipped.
        let starts = statement_starts(source);
        assert_eq!(starts[&24][0].1, Some(Change::ReturnUndefined));
        // Seen from elsewhere, a type the file does not name gets its path.
        let elsewhere = replacements(
            "src/other.rs",
            "fn f() -> Result<(), Error> {\n    Ok(())\n}\n",
            &signatures,
        );
        assert_eq!(
            elsewhere[&2][0].1,
            "Err(Error::new(crate::error::ErrorKind::Empty))"
        );
    }

    /// Supercov's own test fixture `"é🚀x".repeat(700)`: reading an
    /// expression byte by byte must not slice inside a character.
    #[test]
    fn an_expression_with_multibyte_text_is_read_without_slicing_a_character() {
        let e = "\"é🚀x\".repeat(700) == text && n - 1 > 0";
        assert!(boolean(e));
        assert!(!integer_shaped("\"é🚀x\".repeat(700)"));
        assert_eq!(
            replacement("\"é🚀x\".repeat(700)", None),
            "Default::default()"
        );
        assert_eq!(top_level_operands("\"é\".len() - 1"), ["\"é\".len()", "1"]);
    }

    #[test]
    fn a_block_value_takes_the_function_type_only_when_it_is_returned() {
        let source = "fn f(x: u8) -> bool {\n    if x > 1 {\n        x == 2\n    } else {\n        false\n    }\n}\nfn g() -> u8 {\n    let y = {\n        3\n    };\n    y\n}\n";
        let r = replacements("src/lib.rs", source, &Signatures::default());
        assert_eq!(r[&3][0].1, "!(x == 2)");
        assert_eq!(r[&5][0].1, "true");
        // A block assigned to `y` is not the function's value.
        assert_eq!(r[&10][0].1, "0");
        assert_eq!(r[&12][0].1, "0");
    }
}
