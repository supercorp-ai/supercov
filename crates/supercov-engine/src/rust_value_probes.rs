//! Weak mutation's value probes for Rust (`SUPERCOV_MUTANTS=1`).
//!
//! A probe records, at the mutated code, what the mutant would change there,
//! so the weak report can say `infected` or `not-infected` where it would
//! otherwise say only that the code ran:
//!
//! - a relational comparison (`a < b`): its operands' relation, `lt`, `eq`,
//!   `gt` or `unordered`, which decides each of cargo-mutants' replacements;
//! - an arithmetic, bitwise or shift operator, plain or compound
//!   (`a + b`, `a += b`): for each of its mutants, whether the mutant's value
//!   is the original's (`same`, `differs`), an operation that panics in a
//!   test build (overflow, division by zero) counted as its own value;
//! - `-x`: whether `x` is zero, where deleting the `-` changes nothing;
//! - a function returning `bool` or a built-in number: what it returned,
//!   by cargo-mutants' replacement values (`0`, `1`, `-1`, other), and
//!   whether it ended by a panic (`unwound`).
//!
//! Each outcome is a hit of its own id, `<site>#<outcome>`, where `<site>`
//! is `rust_mutants::value_site`'s id for the mutated code. The runtime reads
//! an operand by its type's name and records `unknown` for any type that is
//! not a built-in number, `bool` or `char`, so no probe carries a trait bound
//! and none can change what compiles.
//!
//! An operator's probe evaluates its operands again, so it goes only where
//! they are pure (`value_operand`): paths, fields, indexes, literals, casts,
//! dereferences and parentheses of those. A literal operand takes the other
//! operand's type, as the operator gives it. Probes go only where the
//! instrumenter places probes at all, outside macros and test code.

use ra_ap_syntax::{
    AstNode, SyntaxKind, SyntaxNode, TextRange,
    ast::{self, ArithOp, BinaryOp, CmpOp, HasGenericArgs, HasName, UnaryOp},
};

use crate::rust_mutants::{arithmetic_code, binary_replacements, value_site};

/// A probe site's own state (`settle` in the runtime): once more
/// evaluations cannot change its mutants' verdicts in this process, the
/// probe does nothing there.
/// `{runtime}` is the runtime's path: no path from the crate's root to
/// `core` works in every edition.
const STATE: &str = "{ static STATE: {runtime}::State = {runtime}::State::new(0); &STATE }";

/// An operator's probe, `{call}` given the site's state as `__supercov_site`:
/// its operands are read only while the site is open, so a decided site
/// costs one load.
fn guarded(runtime: &str, state: &str, call: &str) -> String {
    format!(
        "({{ let __supercov_site = {state}; if {runtime}::open(__supercov_site) {{ {call} }} }}, "
    )
}

/// Code a probe adds around a range, or at an offset.
pub(crate) enum ValueEdit {
    Wrap {
        range: TextRange,
        prefix: String,
        suffix: String,
        /// Around every other wrapper of the same range (a returned value),
        /// rather than inside them (an operator's operands).
        outer: bool,
    },
    At {
        offset: usize,
        text: String,
    },
}

/// The probes of a file: each edit, with the ids it records (each needs an
/// ordinal: `ordinal` gives it).
pub(crate) fn place(
    file: &str,
    source: &str,
    runtime: &str,
    skip: &dyn Fn(&SyntaxNode) -> bool,
    ordinal: &mut dyn FnMut(&str) -> u32,
) -> Vec<ValueEdit> {
    let Some((tree, _)) = crate::rust_instrumenter::parse_source(source) else {
        return Vec::new();
    };
    let state = STATE.replace("{runtime}", runtime);
    let mut edits = Vec::new();
    let mut ids = |site: &str, outcomes: &[&str]| {
        outcomes
            .iter()
            .map(|outcome| ordinal(&format!("{site}#{outcome}")).to_string())
            .collect::<Vec<_>>()
    };
    for node in tree.syntax().descendants().filter(|node| !skip(node)) {
        if let Some(binary) = ast::BinExpr::cast(node.clone()) {
            let (Some(left), Some(right), Some((_, operator))) =
                (binary.lhs(), binary.rhs(), binary.op_details())
            else {
                continue;
            };
            let (Some(left), Some(right)) = (operand(runtime, &left), operand(runtime, &right))
            else {
                continue;
            };
            let range = node.text_range();
            match operator {
                BinaryOp::CmpOp(CmpOp::Ord { strict, .. }) => {
                    let site = value_site(file, range, "compare");
                    let outcomes = ids(&site, &["lt", "eq", "gt", "unordered", "unknown"]);
                    edits.push(ValueEdit::Wrap {
                        range,
                        prefix: guarded(
                            runtime,
                            &state,
                            &format!(
                                "{runtime}::compare({left}, {right}, &[{}], {strict}, __supercov_site)",
                                outcomes.join(", ")
                            ),
                        ),
                        suffix: ").1".into(),
                        outer: false,
                    });
                }
                BinaryOp::ArithOp(op) | BinaryOp::Assignment { op: Some(op) } => {
                    let mutants = binary_replacements(operator)
                        .iter()
                        .map(|replacement| {
                            let site = value_site(file, range, replacement);
                            let outcome = ids(&site, &["same", "differs", "unknown"]);
                            format!(
                                "(b'{}', {}, {}, {})",
                                arithmetic_code(replacement),
                                outcome[0],
                                outcome[1],
                                outcome[2]
                            )
                        })
                        .collect::<Vec<_>>();
                    edits.push(ValueEdit::Wrap {
                        range,
                        prefix: guarded(
                            runtime,
                            &state,
                            &format!(
                                "{runtime}::arithmetic({left}, {right}, b'{}', &[{}], __supercov_site)",
                                op_code(op),
                                mutants.join(", ")
                            ),
                        ),
                        suffix: ").1".into(),
                        outer: false,
                    });
                }
                _ => {}
            }
            continue;
        }
        if let Some(prefix) = ast::PrefixExpr::cast(node.clone())
            && prefix.op_kind() == Some(UnaryOp::Neg)
            && let Some(read) = operand(runtime, &prefix.expr().expect("a file that parses"))
        {
            let range = node.text_range();
            let site = value_site(file, range, "negate");
            let outcomes = ids(&site, &["zero", "nonzero", "unknown"]);
            edits.push(ValueEdit::Wrap {
                range,
                prefix: guarded(
                    runtime,
                    &state,
                    &format!(
                        "{runtime}::negated({read}, &[{}], __supercov_site)",
                        outcomes.join(", ")
                    ),
                ),
                suffix: ").1".into(),
                outer: false,
            });
            continue;
        }
        if let Some(function) = ast::Fn::cast(node.clone()) {
            edits.extend(returns(file, runtime, &function, &mut ids));
        }
    }
    edits
}

/// The probe's `Num` for an operand that is pure, `None` for any other.
fn operand(runtime: &str, expression: &ast::Expr) -> Option<String> {
    match expression {
        ast::Expr::Literal(literal) => {
            let text = literal.syntax().text().to_string();
            // A suffix (`1u8`, `2f32`) names the literal's own type; a
            // literal without one takes the other operand's. A hex literal
            // that ends like a suffix (`0xf32`) is read as typed, and so as
            // `i32`: no more than `unknown` against another type.
            let suffixed = [
                "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128",
                "isize", "f32", "f64",
            ]
            .iter()
            .any(|suffix| text.ends_with(suffix));
            let token = literal
                .syntax()
                .first_token()
                .expect("a literal is a token");
            match token.kind() {
                SyntaxKind::INT_NUMBER if !suffixed => {
                    Some(format!("{runtime}::Num::LitInt({text})"))
                }
                SyntaxKind::FLOAT_NUMBER if !suffixed => {
                    Some(format!("{runtime}::Num::LitFloat({text})"))
                }
                SyntaxKind::INT_NUMBER | SyntaxKind::FLOAT_NUMBER | SyntaxKind::CHAR => {
                    Some(format!("{runtime}::num(&({text}))"))
                }
                _ => None,
            }
        }
        ast::Expr::PrefixExpr(prefix)
            if prefix.op_kind() == Some(UnaryOp::Neg)
                && matches!(prefix.expr(), Some(ast::Expr::Literal(_))) =>
        {
            let inner = operand(runtime, &prefix.expr().expect("a literal, as matched"))?;
            // `LitInt(5)` to `LitInt(-5)`; a typed literal stays typed.
            Some(
                inner
                    .replacen("LitInt(", "LitInt(-", 1)
                    .replacen("LitFloat(", "LitFloat(-", 1)
                    .replacen("num(&(", "num(&(-", 1),
            )
        }
        _ if pure(expression) => Some(format!("{runtime}::num(&({}))", expression.syntax().text())),
        _ => None,
    }
}

/// An expression that evaluating twice changes nothing.
fn pure(expression: &ast::Expr) -> bool {
    match expression {
        ast::Expr::PathExpr(path) => path.path().is_some_and(|path| {
            path.segments()
                .all(|segment| segment.generic_arg_list().is_none())
        }),
        ast::Expr::FieldExpr(field) => field.expr().is_some_and(|inner| pure(&inner)),
        ast::Expr::ParenExpr(paren) => paren.expr().is_some_and(|inner| pure(&inner)),
        ast::Expr::IndexExpr(index) => {
            index.base().is_some_and(|inner| pure(&inner))
                && index
                    .index()
                    .is_some_and(|inner| pure(&inner) || matches!(inner, ast::Expr::Literal(_)))
        }
        ast::Expr::PrefixExpr(prefix) => {
            prefix.op_kind() == Some(UnaryOp::Deref)
                && prefix.expr().is_some_and(|inner| pure(&inner))
        }
        ast::Expr::CastExpr(cast) => cast.expr().is_some_and(|inner| pure(&inner)),
        // Arithmetic of pure operands: evaluated again, it gives the same,
        // and panics, if at all, where the original would.
        ast::Expr::BinExpr(binary) => {
            matches!(binary.op_kind(), Some(BinaryOp::ArithOp(_)))
                && binary
                    .lhs()
                    .is_some_and(|inner| pure(&inner) || matches!(inner, ast::Expr::Literal(_)))
                && binary
                    .rhs()
                    .is_some_and(|inner| pure(&inner) || matches!(inner, ast::Expr::Literal(_)))
        }
        _ => false,
    }
}

fn op_code(op: ArithOp) -> char {
    match op {
        ArithOp::Add => '+',
        ArithOp::Sub => '-',
        ArithOp::Mul => '*',
        ArithOp::Div => '/',
        ArithOp::Rem => '%',
        ArithOp::Shl => '<',
        ArithOp::Shr => '>',
        ArithOp::BitXor => '^',
        ArithOp::BitOr => '|',
        ArithOp::BitAnd => '&',
    }
}

/// The return types a function's value is probed for.
const RETURNED: &[&str] = &[
    "bool", "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize",
    "f32", "f64",
];

/// A function's return probes: its tail and each `return` of its own pass
/// their value through `returned`, and a guard at its entry records an end
/// by a panic.
fn returns(
    file: &str,
    runtime: &str,
    function: &ast::Fn,
    ids: &mut dyn FnMut(&str, &[&str]) -> Vec<String>,
) -> Vec<ValueEdit> {
    let Some(ty) = function
        .ret_type()
        .and_then(|ret| ret.ty())
        .map(|ty| ty.syntax().text().to_string())
        .filter(|ty| RETURNED.contains(&ty.as_str()))
    else {
        return Vec::new();
    };
    if function.const_token().is_some() || function.async_token().is_some() {
        return Vec::new();
    }
    // A function with a body has a statement list.
    let Some(body) = function.body() else {
        return Vec::new();
    };
    let list = body.stmt_list().expect("a block has statements");
    let state = STATE.replace("{runtime}", runtime);
    let site = value_site(file, function.syntax().text_range(), "return");
    let outcomes = ids(&site, &["0", "1", "-1", "other", "unknown", "unwound"]);
    let table = format!("&[{}], {state}", outcomes[..5].join(", "));
    let mut edits = Vec::new();
    let entry = list
        .syntax()
        .children()
        .filter(|child| child.kind() == SyntaxKind::ATTR)
        .last()
        .map_or_else(
            || usize::from(list.syntax().text_range().start()) + 1,
            |attribute| usize::from(attribute.text_range().end()),
        );
    edits.push(ValueEdit::At {
        offset: entry,
        text: format!(
            " let __supercov_unwound = {runtime}::Unwound({});",
            outcomes[5]
        ),
    });
    let wrap = |expression: &ast::Expr| ValueEdit::Wrap {
        range: expression.syntax().text_range(),
        prefix: format!("{runtime}::returned::<{ty}>("),
        suffix: format!(", {table})"),
        outer: true,
    };
    if let Some(tail) = list.tail_expr() {
        edits.push(wrap(&tail));
    }
    let owner = function.syntax().clone();
    for value in body
        .syntax()
        .descendants()
        .filter_map(ast::ReturnExpr::cast)
        .filter(|ret| {
            ret.syntax()
                .ancestors()
                .find(|ancestor| {
                    matches!(ancestor.kind(), SyntaxKind::FN | SyntaxKind::CLOSURE_EXPR)
                        || ast::BlockExpr::cast(ancestor.clone())
                            .is_some_and(|block| block.async_token().is_some())
                })
                .is_some_and(|ancestor| ancestor == owner)
        })
        .filter_map(|ret| ret.expr())
    {
        edits.push(wrap(&value));
    }
    let _ = function.name();
    edits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placed(source: &str) -> Vec<String> {
        let mut next = 0;
        place("src/a.rs", source, "rt", &|_| false, &mut |_| {
            next += 1;
            next
        })
        .into_iter()
        .map(|edit| match edit {
            ValueEdit::Wrap { prefix, suffix, .. } => format!("{prefix}…{suffix}"),
            ValueEdit::At { text, .. } => text,
        })
        .collect()
    }

    #[test]
    fn pure_operands_and_number_returns_are_probed() {
        let edits = placed(
            "fn f(a: u8, b: u8, v: &[u8]) -> bool {\n    if a < 3 { return v[0] > b; }\n    a + b > g() || -a.x == 1\n}\nfn h(s: String) -> String { s + \"x\" }\nconst fn c(a: u8) -> u8 { a * 2 }\n",
        );
        let all = edits.join("\n");
        assert!(
            all.contains(
                "if rt::open(__supercov_site) { rt::compare(rt::num(&(a)), rt::Num::LitInt(3), &[7, 8, 9, 10, 11], true, __supercov_site) }"
            ),
            "{all}"
        );
        assert!(
            all.contains("rt::compare(rt::num(&(v[0])), rt::num(&(b))"),
            "{all}"
        );
        // `g()` is a call: evaluating it again could change something.
        assert!(!all.contains("g()"), "{all}");
        assert!(
            all.contains("let __supercov_unwound = rt::Unwound("),
            "{all}"
        );
        assert!(all.contains("rt::returned::<bool>("), "{all}");
        assert!(all.contains("rt::negated(rt::num(&(a.x))"), "{all}");
        // A string literal is no number: `s + "x"` has no probe.
        assert!(!all.contains("num(&(s))"), "{all}");
        // A `String` return is not probed, and neither is a `const fn` body
        // (the caller skips const contexts; here only its return).
        assert!(!all.contains("returned::<String>"), "{all}");
        assert!(!all.contains("returned::<u8>"), "{all}");
        assert!(
            all.contains("b'*', &[(b'+', 26, 27, 28), (b'/', 29, 30, 31)], __supercov_site)"),
            "{all}"
        );
        let negative = placed("fn n(a: i8) -> i8 { a * -2 }\n").join("\n");
        assert!(negative.contains("rt::Num::LitInt(-2)"), "{negative}");
        assert!(placed("fn broken( {").is_empty());
    }

    /// Operands that are not pure, and literals of every kind, beside pure
    /// ones: only the pure ones are read.
    #[test]
    fn only_pure_operands_are_read() {
        let all = placed(concat!(
            "fn a(v: &[u8], i: usize, y: u8, x: f64) {\n",
            "    let _ = (v[g()] < 1, g()[0] < 1, v[i] < 1, *g() < 1, (g() + y) < y, (y + g()) < y, (1 + y) < y, (y + 1) < y, y - 1, x < 1.5f64, -true < y);\n",
            "}\n",
        ))
        .join("\n");
        for absent in [
            "num(&(v[g()]))",
            "num(&(g()[0]))",
            "num(&(*g()))",
            "num(&((g() + y)))",
            "num(&((y + g())))",
            "-true",
        ] {
            assert!(!all.contains(absent), "{absent}: {all}");
        }
        for present in [
            "num(&(v[i]))",
            "num(&((1 + y)))",
            "num(&((y + 1)))",
            "b'-'",
            "num(&(1.5f64))",
        ] {
            assert!(all.contains(present), "{present}: {all}");
        }
    }

    /// Every operand and operator kind, and every function kind.
    #[test]
    fn each_kind_of_operand_operator_and_function() {
        let source = concat!(
            "fn a(x: f64, y: u8, z: &u8, w: (u8,)) -> u8 {\n",
            "    let _ = (x < 1.5, x < 2f32, 'a' < 'b', \"s\" < \"t\", y as u16 > 1, *z > (y), w.0 / 2, y % 2, y << 1, y >> 1, y ^ 1, y | 1, y & 1, y == 1, !true, -g(), -1u8 > y, -1.5 < x, -'a', x * 0x1F);\n",
            "    let c = || { return 1u8; };\n",
            "    return y + 1;\n",
            "}\n",
            "async fn b() -> u8 { 1 }\n",
            "const fn c() -> u8 { 1 }\n",
            "fn d() -> bool { return true; }\n",
            "fn e() -> u8;\n",
            "fn f() -> u8 { #![allow(unused)] 1 }\n",
            "fn g() -> u8 { async { return 2u8; }; 1 }\n",
        );
        let all = placed(source).join("\n");
        for expected in [
            "rt::Num::LitFloat(1.5)",
            "rt::num(&(2f32))",
            "rt::num(&('a'))",
            "rt::num(&(y as u16))",
            "rt::num(&(*z)), rt::num(&((y)))",
            "rt::num(&(w.0)), rt::Num::LitInt(2), b'/'",
            "b'%'",
            "b'<'",
            "b'>'",
            "b'^'",
            "b'|'",
            "b'&'",
            "rt::num(&(-1u8))",
            "rt::Num::LitFloat(-1.5)",
            "rt::Num::LitInt(0x1F)",
            "returned::<u8>(…",
            "returned::<bool>(…",
        ] {
            assert!(all.contains(expected), "{expected}: {all}");
        }
        // A string, a call, a `char` negated, `==`, `!`: no probe. Neither
        // an `async fn` nor a `const fn`, nor a closure's or an async
        // block's `return`.
        for absent in [
            "\"s\"",
            "g()",
            "-'a'",
            "== 1",
            "!true",
            "return 1u8",
            "return 2u8",
        ] {
            assert!(
                !all.contains(&format!("({absent}")) && !all.contains(&format!("{absent})")),
                "{absent}: {all}"
            );
        }
        assert_eq!(all.matches("__supercov_unwound").count(), 4, "{all}");
        // A node the caller skips has no probe.
        let skipped = place("src/a.rs", source, "rt", &|_| true, &mut |_| 0);
        assert!(skipped.is_empty());
    }
}
