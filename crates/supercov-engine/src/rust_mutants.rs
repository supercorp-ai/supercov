//! The mutants of a Rust file, as cargo-mutants makes them.
//!
//! `enumerate` walks the program as written and lists, in cargo-mutants'
//! order and with its spans, every mutant of its operator set
//! (`MUTANT_SET`): a function body replaced by values of its return type
//! (`FnValue`), binary operators swapped (`BinaryOperator`), `!` and `-`
//! deleted (`UnaryOperator`), match arms deleted where a `_` arm catches
//! what they matched (`MatchArm`), match guards made `true` and `false`
//! (`MatchArmGuard`), and fields of a struct literal with a `..base` deleted
//! (`StructField`). cargo-mutants reads the program with `syn`; this reads
//! it with the parser the instrumenter uses, and the parity goldens in
//! `tests/fixtures/cargo-mutants-parity` (written by
//! `scripts/cargo-mutants-parity.mjs`) hold the two to the same mutants,
//! order, spans and replacement code.
//!
//! What cargo-mutants never looks inside is left alone here too: macro
//! arguments (they are tokens until expanded), `#[cfg(test)]` and test
//! functions, `#[mutants::skip]`, `unsafe fn`, a method named `new`, and an
//! `impl Default`. Calls of `with_capacity` are skipped whole, as
//! cargo-mutants skips them by default.
//!
//! A mutant's id is `file:startLine:startCol-endLine:endCol:Genre:descriptor`
//! with cargo-mutants' 1-based lines and 1-based char columns. The
//! descriptor is the replacement code, or `removed` where the mutant deletes
//! (an operator, an arm, a field).

use ra_ap_syntax::{
    AstNode, NodeOrToken, SyntaxKind, SyntaxNode, SyntaxToken, TextRange,
    ast::{self, ArithOp, AstToken, BinaryOp, CmpOp, HasGenericArgs, HasName, LogicOp, UnaryOp},
};

/// The operator set, as `mutants.json` names it.
pub(crate) const MUTANT_SET: &str = "cargo-mutants@27.1.0";

/// The descriptor of a mutant whose replacement is nothing.
pub(crate) const REMOVED: &str = "removed";

/// One cargo-mutants mutant of a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RustMutant {
    /// `FnValue`, `BinaryOperator`, `UnaryOperator`, `MatchArm`,
    /// `MatchArmGuard` or `StructField`.
    pub genre: &'static str,
    /// The bytes the replacement takes the place of.
    pub range: TextRange,
    /// 1-based line and 1-based char column, as cargo-mutants reports them;
    /// `end` is exclusive.
    pub start: (u32, u32),
    pub end: (u32, u32),
    /// The code put in the place of `range`, as cargo-mutants prints it;
    /// empty where the mutant deletes.
    pub replacement: String,
    /// The function the mutant is in: its whole item (attributes and doc
    /// comments too).
    pub function: Option<TextRange>,
    /// What the instrumenter needs to place a switch: see `Site`.
    pub site: Site,
}

/// Where a mutant is in the program, for its switch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Site {
    /// A function body: the statement list between its braces, and whether
    /// the function returns an `impl Trait` (whose value a switch beside the
    /// body cannot take).
    Body { list: TextRange, opaque: bool },
    /// The whole binary or prefix expression whose operator is replaced.
    Operator { expression: TextRange },
    /// A match arm deleted: the arm, and its pattern.
    Arm { arm: TextRange, pattern: TextRange },
    /// A match guard's condition.
    Guard,
    /// A field of a struct literal deleted: the whole literal.
    Field { record: TextRange },
}

impl RustMutant {
    /// The descriptor in the mutant's id.
    pub(crate) fn descriptor(&self) -> &str {
        if self.replacement.is_empty() {
            REMOVED
        } else {
            &self.replacement
        }
    }
}

/// The mutants of `source` in cargo-mutants' order, or `None` where it does
/// not parse (cargo-mutants lists none of a file `syn` refuses).
pub(crate) fn enumerate(source: &str) -> Option<Vec<RustMutant>> {
    let (tree, _) = crate::rust_instrumenter::parse_source(source)?;
    let mut walker = Walker {
        lines: Lines::new(source),
        mutants: Vec::new(),
        functions: Vec::new(),
    };
    let root = tree.syntax();
    if !excluded(root) {
        walker.children(root);
    }
    Some(walker.mutants)
}

/// A file's mutants as `mutants.json` lists them, each linked to the
/// evidence the run recorded of its code: the innermost statement and
/// function around it, the arm a `MatchArm` mutant deletes, and the guard a
/// `MatchArmGuard` mutant forces. `recorded` is every id the run measured in
/// the file (its points, decisions and branch alternatives); a link is made
/// only to one of them, so a mutant in code the run could not measure (a
/// `const fn`) has none, and is reached for all the run knows. `None` where
/// the file does not parse, or the run measured a point its source no longer
/// has: it changed since.
pub(crate) fn list(
    file: &str,
    source: &str,
    recorded: &std::collections::BTreeSet<String>,
) -> Option<Vec<crate::js_mutant_links::CandidateMutant>> {
    use crate::rust_instrumenter::stable_id;
    let (tree, _) = crate::rust_instrumenter::parse_source(source)?;
    let mutants = enumerate(source).expect("a file that parses is enumerated");
    let root = tree.syntax();
    // Every statement and function the instrumenter can name, with its
    // range; the run's own points must be among them.
    let mut statements = Vec::new();
    let mut functions = Vec::new();
    for list in root.descendants().filter_map(ast::StmtList::cast) {
        for statement in list.statements() {
            if matches!(statement, ast::Stmt::ExprStmt(_) | ast::Stmt::LetStmt(_)) {
                let range = statement.syntax().text_range();
                statements.push((stable_id(file, "statement", range, ""), range));
            }
        }
        if let Some(tail) = list.tail_expr() {
            let range = tail.syntax().text_range();
            statements.push((stable_id(file, "statement", range, ""), range));
        }
    }
    for function in root.descendants().filter_map(ast::Fn::cast) {
        let range = function.syntax().text_range();
        let label = function
            .name()
            .map(|name| name.text().to_string())
            .unwrap_or_default();
        functions.push((stable_id(file, "function", range, &label), range));
    }
    for closure in root.descendants().filter_map(ast::ClosureExpr::cast) {
        let range = closure.syntax().text_range();
        functions.push((stable_id(file, "function", range, "<closure>"), range));
    }
    // The instrumenter reads the arguments of macros it knows as code, so
    // its points are its manifest's, not only those above.
    let manifest = crate::rust_instrumenter::build_rust_manifest(file, source)
        .expect("a file that parses has a manifest");
    let known = manifest
        .points
        .iter()
        .chain(&manifest.assertion_sites)
        .map(|point| point.id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let points_changed = recorded.iter().any(|id| {
        (id.starts_with("rs:statement:") || id.starts_with("rs:function:"))
            && !known.contains(id.as_str())
    });
    if points_changed {
        return None;
    }
    // The innermost recorded one of `candidates` whose range holds `range`.
    let innermost = |candidates: &[(String, TextRange)], range: TextRange| {
        candidates
            .iter()
            .filter(|(id, around)| around.contains_range(range) && recorded.contains(id))
            .min_by_key(|(_, around)| around.len())
            .map(|(id, _)| id.clone())
    };
    let linked = mutants
        .into_iter()
        .map(|mutant| {
            let mut candidate = crate::js_mutant_links::CandidateMutant {
                log_only: false,
                id: String::new(),
                mutator: mutant.genre.to_owned(),
                replacement: mutant.descriptor().to_owned(),
                start: mutant.start,
                end: mutant.end,
                original: source[mutant.range].to_owned(),
                operator: None,
                value_used: false,
                effects: false,
                statement: None,
                function: None,
                decision: None,
                branch: None,
                chain: None,
                compare: None,
                probe: None,
                arrow: None,
                equivalent: None,
                untestable: None,
                reads: None,
                is_static: false,
                unplaceable: None,
            };
            // Code no probe can be in (a `const fn`'s, an allocator's) has no
            // record of its own, though the run may list its points: the
            // mutant is linked to nothing, and reached for all the run knows.
            let node = match root.covering_element(mutant.range) {
                ra_ap_syntax::NodeOrToken::Node(node) => node,
                ra_ap_syntax::NodeOrToken::Token(token) => {
                    token.parent().expect("a token is in a node")
                }
            };
            if crate::rust_instrumenter::cannot_carry_probe(&node) {
                return candidate;
            }
            // The value probe at the mutated code, where the run placed one.
            let probed = |range: TextRange, kind: &str| {
                let site = value_site(file, range, kind);
                recorded.contains(&site).then_some(site)
            };
            match &mutant.site {
                Site::Body { .. } => {
                    candidate.probe = mutant.function.and_then(|range| probed(range, "return"));
                    // A body is reached when its function runs: the
                    // function's own point, never one around it.
                    candidate.function = mutant.function.and_then(|range| {
                        functions
                            .iter()
                            .find(|(id, around)| *around == range && recorded.contains(id))
                            .map(|(id, _)| id.clone())
                    });
                }
                Site::Operator { expression } => {
                    let start = usize::from(mutant.range.start() - expression.start());
                    candidate.original = source[*expression].to_owned();
                    candidate.operator = Some(crate::js_mutant_links::MutantOperator {
                        text: source[mutant.range].to_owned(),
                        at: start,
                    });
                    candidate.statement = innermost(&statements, *expression);
                    candidate.function = innermost(&functions, *expression);
                    let operator = &source[mutant.range];
                    if mutant.genre == "UnaryOperator" {
                        if operator == "-" {
                            candidate.probe = probed(*expression, "negate");
                        }
                    } else if matches!(operator, "<" | "<=" | ">" | ">=") {
                        candidate.compare = probed(*expression, "compare");
                    } else if !matches!(operator, "==" | "!=" | "&&" | "||") {
                        candidate.probe = probed(*expression, &mutant.replacement);
                    }
                }
                Site::Arm { arm, .. } => {
                    let selected =
                        format!("{}:selected", stable_id(file, "branch", *arm, "match-arm"));
                    candidate.statement = recorded
                        .contains(&selected)
                        .then_some(selected)
                        .or_else(|| innermost(&statements, *arm));
                    candidate.function = innermost(&functions, *arm);
                }
                Site::Guard => {
                    let decision = stable_id(file, "decision", mutant.range, "match-guard");
                    if recorded.contains(&decision) {
                        candidate.decision = Some(crate::js_mutant_links::DecisionLink {
                            id: decision,
                            condition: None,
                            prefix: None,
                        });
                    }
                    candidate.statement = innermost(&statements, mutant.range);
                    candidate.function = innermost(&functions, mutant.range);
                }
                Site::Field { .. } => {
                    candidate.statement = innermost(&statements, mutant.range);
                    candidate.function = innermost(&functions, mutant.range);
                }
            }
            candidate
        })
        .collect();
    Some(linked)
}

struct Walker<'s> {
    lines: Lines<'s>,
    mutants: Vec<RustMutant>,
    /// The functions around the node being walked, innermost last.
    functions: Vec<TextRange>,
}

impl Walker<'_> {
    fn children(&mut self, node: &SyntaxNode) {
        for child in node.children() {
            self.node(&child);
        }
    }

    fn push(&mut self, genre: &'static str, range: TextRange, replacement: String, site: Site) {
        self.mutants.push(RustMutant {
            genre,
            range,
            start: self.lines.position(usize::from(range.start())),
            end: self.lines.position(usize::from(range.end())),
            replacement,
            function: self.functions.last().copied(),
            site,
        });
    }

    fn node(&mut self, node: &SyntaxNode) {
        match node.kind() {
            SyntaxKind::FN => self.function(node),
            SyntaxKind::IMPL => {
                if excluded(node) {
                    return;
                }
                let implements_default = ast::Impl::cast(node.clone())
                    .and_then(|item| item.trait_())
                    .is_some_and(|trait_| {
                        matches!(trait_, ast::Type::PathType(ref path) if path.path().is_some_and(|path| last_name(&path).as_deref() == Some("Default")))
                    });
                if !implements_default {
                    self.children(node);
                }
            }
            SyntaxKind::TRAIT | SyntaxKind::MODULE => {
                if !excluded(node) {
                    self.children(node);
                }
            }
            SyntaxKind::BIN_EXPR => self.binary(node),
            SyntaxKind::PREFIX_EXPR => self.prefix(node),
            SyntaxKind::MATCH_EXPR => self.matched(node),
            SyntaxKind::RECORD_EXPR => self.record(node),
            SyntaxKind::CALL_EXPR => {
                if excluded(node) {
                    return;
                }
                let skipped = ast::CallExpr::cast(node.clone())
                    .and_then(|call| call.expr())
                    .and_then(|callee| match callee {
                        ast::Expr::PathExpr(path) => path.path(),
                        _ => None,
                    })
                    .is_some_and(|path| last_name(&path).as_deref() == Some(SKIPPED_CALL));
                if !skipped {
                    self.children(node);
                }
            }
            SyntaxKind::METHOD_CALL_EXPR => {
                if excluded(node) {
                    return;
                }
                let skipped = ast::MethodCallExpr::cast(node.clone())
                    .and_then(|call| call.name_ref())
                    .is_some_and(|name| name.text() == SKIPPED_CALL);
                if !skipped {
                    self.children(node);
                }
            }
            _ => self.children(node),
        }
    }

    fn function(&mut self, node: &SyntaxNode) {
        let function = ast::Fn::cast(node.clone()).expect("a function node");
        // A function with no body (in a trait or an `extern` block) has
        // nothing to mutate, and `syn` visits nothing inside it.
        let Some(body) = function.body() else {
            return;
        };
        let in_impl = node
            .parent()
            .and_then(|list| list.parent())
            .is_some_and(|owner| matches!(owner.kind(), SyntaxKind::IMPL | SyntaxKind::TRAIT));
        let named_new = function.name().is_some_and(|name| name.text() == "new");
        if function.unsafe_token().is_some() || excluded(node) || (in_impl && named_new) {
            return;
        }
        let list = body.stmt_list().expect("a block has statements");
        let statements = statements(&list);
        if statements.is_empty() {
            return;
        }
        self.functions.push(node.text_range());
        let first = visible_start(&statements[0]);
        let last = statements.last().expect("not empty").text_range().end();
        let range = TextRange::new(first, last);
        let return_type = function.ret_type().and_then(|ret| ret.ty());
        // An `impl Trait` anywhere in it (`Result<impl Iterator, E>`): a
        // second value is a second type.
        let opaque = return_type.as_ref().is_some_and(|ty| {
            ty.syntax()
                .descendants()
                .any(|node| node.kind() == SyntaxKind::IMPL_TRAIT_TYPE)
        });
        let replacements = match &return_type {
            None => vec!["()".to_owned()],
            Some(ty) => type_replacements(ty),
        };
        let original = block_tokens(&list);
        for replacement in replacements {
            // A replacement that is the body as written changes nothing.
            if tokens_of_code(&replacement) == original {
                continue;
            }
            self.push(
                "FnValue",
                range,
                replacement,
                Site::Body {
                    list: list.syntax().text_range(),
                    opaque,
                },
            );
        }
        self.children(node);
        self.functions.pop();
    }

    fn binary(&mut self, node: &SyntaxNode) {
        // No attribute skips an operator: `syn` gives `#[a] x + y`'s
        // attribute to `x`, and cargo-mutants mutates the `+`.
        // A file that parses has every operator's operands and token.
        let (token, operator) = ast::BinExpr::cast(node.clone())
            .and_then(|binary| binary.op_details())
            .expect("a binary expression has its operator");
        for replacement in binary_replacements(operator) {
            self.push(
                "BinaryOperator",
                token.text_range(),
                (*replacement).to_owned(),
                Site::Operator {
                    expression: regrouped(node, token.text_range(), replacement)
                        .unwrap_or_else(|| node.text_range()),
                },
            );
        }
        self.children(node);
    }

    fn prefix(&mut self, node: &SyntaxNode) {
        if excluded(node) {
            return;
        }
        let prefix = ast::PrefixExpr::cast(node.clone()).expect("a prefix expression");
        if matches!(prefix.op_kind(), Some(UnaryOp::Not | UnaryOp::Neg)) {
            self.push(
                "UnaryOperator",
                prefix.op_token().expect("an operator").text_range(),
                String::new(),
                Site::Operator {
                    expression: node.text_range(),
                },
            );
        }
        self.children(node);
    }

    fn matched(&mut self, node: &SyntaxNode) {
        if excluded(node) {
            return;
        }
        let arms = ast::MatchExpr::cast(node.clone())
            .and_then(|expression| expression.match_arm_list())
            .map(|list| list.arms().collect::<Vec<_>>())
            .unwrap_or_default();
        let wildcard = |arm: &ast::MatchArm| matches!(arm.pat(), Some(ast::Pat::WildcardPat(_)));
        if arms.iter().any(wildcard) {
            for (arm, pattern) in arms
                .iter()
                .filter(|arm| !wildcard(arm) && arm.guard().is_none())
                .map(|arm| (arm, arm.pat().expect("an arm that parsed has its pattern")))
            {
                let range =
                    TextRange::new(visible_start(arm.syntax()), arm.syntax().text_range().end());
                self.push(
                    "MatchArm",
                    range,
                    String::new(),
                    Site::Arm {
                        arm: arm.syntax().text_range(),
                        pattern: pattern.syntax().text_range(),
                    },
                );
            }
        }
        for arm in &arms {
            if let Some(condition) = arm.guard().and_then(|guard| guard.condition()) {
                for replacement in ["true", "false"] {
                    self.push(
                        "MatchArmGuard",
                        condition.syntax().text_range(),
                        replacement.to_owned(),
                        Site::Guard,
                    );
                }
            }
        }
        self.children(node);
    }

    fn record(&mut self, node: &SyntaxNode) {
        if excluded(node) {
            return;
        }
        let list = ast::RecordExpr::cast(node.clone())
            .and_then(|record| record.record_expr_field_list())
            .expect("a struct literal has its fields");
        if list.spread().is_some() {
            for field in list.fields() {
                // `0: x` names a tuple struct's field by position, which
                // cargo-mutants leaves alone; `a` alone is named.
                let named = field
                    .name_ref()
                    .is_none_or(|name| !name.text().starts_with(|c: char| c.is_ascii_digit()));
                if !named {
                    continue;
                }
                let start = visible_start(field.syntax());
                let end = next_comma(field.syntax())
                    .map_or(field.syntax().text_range().end(), |comma| {
                        comma.text_range().end()
                    });
                self.push(
                    "StructField",
                    TextRange::new(start, end),
                    String::new(),
                    Site::Field {
                        record: node.text_range(),
                    },
                );
            }
        }
        self.children(node);
    }
}

/// The id of a value probe's site (`rust_value_probes`): the mutated
/// expression or function and what is probed of it (`compare`, `negate`,
/// `return`, or an arithmetic mutant's replacement).
pub(crate) fn value_site(file: &str, range: TextRange, kind: &str) -> String {
    crate::rust_instrumenter::stable_id(file, "value", range, kind)
}

/// The runtime's code for an operator replacement: its first character, or
/// `<` and `>` for the shifts.
pub(crate) fn arithmetic_code(replacement: &str) -> char {
    replacement.chars().next().unwrap_or(' ')
}

/// A call cargo-mutants skips whole by default.
const SKIPPED_CALL: &str = "with_capacity";

/// cargo-mutants' replacements of a binary operator.
/// cargo-mutants replaces an operator's text in the source, so the
/// compiler reads its mutant afresh: where the new operator binds more or
/// less tightly than the old, the expression around it groups otherwise.
/// `a - b * c` with `*` made `+` is `a - b + c`, `(a - b) + c`, not
/// `a - (b + c)`; `p && q && r` with its first `&&` made `||` is
/// `p || (q && r)`. The expression of operators the mutant sits in (its
/// operators, casts and ranges, up to what bounds them: parentheses, a call,
/// a statement), where its text with the replacement groups otherwise than
/// the expression as written with only the operator changed; `None` where
/// it groups alike, and the operator's own expression holds the mutant.
fn regrouped(node: &SyntaxNode, operator: TextRange, replacement: &str) -> Option<TextRange> {
    let chained = |node: &SyntaxNode| {
        matches!(
            node.kind(),
            SyntaxKind::BIN_EXPR
                | SyntaxKind::PREFIX_EXPR
                | SyntaxKind::CAST_EXPR
                | SyntaxKind::RANGE_EXPR
        )
    };
    // The node is an operator's: the chain holds it at least.
    let outer = node
        .ancestors()
        .take_while(chained)
        .last()
        .unwrap_or(node.clone());
    if outer == *node {
        return None;
    }
    let start = outer.text_range().start();
    let text = outer.text().to_string();
    let at = usize::from(operator.start() - start)..usize::from(operator.end() - start);
    let mutated = format!("{}{replacement}{}", &text[..at.start], &text[at.end..]);
    let parsed = ra_ap_syntax::SourceFile::parse(
        &format!("fn f() {{ let _ = ({mutated}); }}"),
        ra_ap_syntax::Edition::Edition2024,
    );
    let reparsed = parsed
        .tree()
        .syntax()
        .descendants()
        .find(|node| node.kind() == SyntaxKind::PAREN_EXPR)
        .and_then(|paren| paren.children().next());
    let alike = reparsed.is_some_and(|reparsed| {
        shape(&reparsed, None) == shape(&outer, Some((operator, replacement)))
    });
    (!alike).then(|| outer.text_range())
}

/// How `node` groups: each node in parentheses, its tokens as written but
/// for `replaced`, the token at a range given its new text.
fn shape(node: &SyntaxNode, replaced: Option<(TextRange, &str)>) -> String {
    let parts = node
        .children_with_tokens()
        .filter_map(|element| match element {
            ra_ap_syntax::NodeOrToken::Node(child) => Some(shape(&child, replaced)),
            ra_ap_syntax::NodeOrToken::Token(token) if token.kind().is_trivia() => None,
            ra_ap_syntax::NodeOrToken::Token(token) => Some(
                replaced
                    .filter(|(range, _)| *range == token.text_range())
                    .map_or_else(|| token.text().to_owned(), |(_, text)| text.to_owned()),
            ),
        })
        .collect::<Vec<_>>();
    format!("({})", parts.join(" "))
}

pub(crate) fn binary_replacements(operator: BinaryOp) -> &'static [&'static str] {
    use ra_ap_syntax::ast::Ordering::{Greater, Less};
    match operator {
        BinaryOp::CmpOp(CmpOp::Eq { negated: false }) => &["!="],
        BinaryOp::CmpOp(CmpOp::Eq { negated: true }) => &["=="],
        BinaryOp::LogicOp(LogicOp::And) => &["||"],
        BinaryOp::LogicOp(LogicOp::Or) => &["&&"],
        BinaryOp::CmpOp(CmpOp::Ord {
            ordering: Less,
            strict: true,
        }) => &["==", ">", "<="],
        BinaryOp::CmpOp(CmpOp::Ord {
            ordering: Greater,
            strict: true,
        }) => &["==", "<", ">="],
        BinaryOp::CmpOp(CmpOp::Ord {
            ordering: Less,
            strict: false,
        }) => &[">"],
        BinaryOp::CmpOp(CmpOp::Ord {
            ordering: Greater,
            strict: false,
        }) => &["<"],
        BinaryOp::ArithOp(op) => match op {
            ArithOp::Add => &["-", "*"],
            ArithOp::Sub | ArithOp::Mul => &["+", "/"],
            ArithOp::Div => &["%", "*"],
            ArithOp::Rem => &["/", "+"],
            ArithOp::Shl => &[">>"],
            ArithOp::Shr => &["<<"],
            ArithOp::BitAnd => &["|", "^"],
            ArithOp::BitOr => &["&", "^"],
            ArithOp::BitXor => &["|", "&"],
        },
        BinaryOp::Assignment { op: Some(op) } => match op {
            ArithOp::Add => &["-=", "*="],
            ArithOp::Sub | ArithOp::Mul => &["+=", "/="],
            ArithOp::Div => &["%=", "*="],
            ArithOp::Rem => &["/=", "+="],
            ArithOp::Shl => &[">>="],
            ArithOp::Shr => &["<<="],
            ArithOp::BitAnd => &["|="],
            ArithOp::BitOr => &["&="],
            ArithOp::BitXor => &["|=", "&="],
        },
        BinaryOp::Assignment { op: None } => &[],
    }
}

/// A block's statements as `syn` lists them: its inner attributes are not
/// among them.
fn statements(list: &ast::StmtList) -> Vec<SyntaxNode> {
    list.syntax()
        .children()
        .filter(|child| child.kind() != SyntaxKind::ATTR)
        .collect()
}

/// Where `syn` says a node starts: at its first attribute or doc comment,
/// never at a plain comment the parser attached to it.
fn visible_start(node: &SyntaxNode) -> ra_ap_syntax::TextSize {
    node.children_with_tokens()
        .find(|element| match element {
            NodeOrToken::Token(token) => !trivia(token),
            NodeOrToken::Node(_) => true,
        })
        .map_or(node.text_range().start(), |element| {
            element.text_range().start()
        })
}

/// A comment token that documents: `///`, `//!`, `/** */`, `/*! */`.
fn doc_comment(token: &SyntaxToken) -> bool {
    ast::Comment::cast(token.clone()).is_some_and(|comment| comment.kind().doc.is_some())
}

/// Whitespace and comments that are not documentation: what `syn` drops.
fn trivia(token: &SyntaxToken) -> bool {
    token.kind() == SyntaxKind::WHITESPACE
        || (token.kind() == SyntaxKind::COMMENT && !doc_comment(token))
}

/// The `,` after a node, past whitespace and comments. A field before a
/// `..base` always has one.
fn next_comma(node: &SyntaxNode) -> Option<SyntaxToken> {
    std::iter::successors(node.next_sibling_or_token(), |element| {
        element.next_sibling_or_token()
    })
    .find(|element| !element.kind().is_trivia())
    .and_then(NodeOrToken::into_token)
    .filter(|token| token.kind() == SyntaxKind::COMMA)
}

fn last_name(path: &ast::Path) -> Option<String> {
    path.segment()
        .and_then(|segment| segment.name_ref())
        .map(|name| name.text().to_string())
}

/// Whether cargo-mutants skips a node and everything in it: `#[cfg(test)]`,
/// an attribute whose path ends in `test` (`#[test]`, `#[tokio::test]`), or
/// `#[mutants::skip]`, alone or in a `cfg_attr`.
fn excluded(node: &SyntaxNode) -> bool {
    node.children()
        .filter(|child| child.kind() == SyntaxKind::ATTR)
        .any(|attribute| attribute_excludes(&attribute))
}

fn attribute_excludes(attribute: &SyntaxNode) -> bool {
    let tokens = attribute
        .descendants_with_tokens()
        .filter_map(NodeOrToken::into_token)
        .filter(|token| !token.kind().is_trivia())
        .map(|token| token.text().to_owned())
        .collect::<Vec<_>>();
    // `#`, then `!` for an inner attribute, then `[ ... ]`.
    let start = if tokens.get(1).map(String::as_str) == Some("!") {
        3
    } else {
        2
    };
    let inner = tokens
        .get(start..tokens.len().saturating_sub(1))
        .unwrap_or_default();
    let (path, rest) = read_path(inner);
    if path.last().map(String::as_str) == Some("test") || path == ["mutants", "skip"] {
        return true;
    }
    let Some(arguments) = rest
        .strip_prefix(&["(".to_owned()])
        .and_then(|rest| rest.strip_suffix(&[")".to_owned()]))
        .and_then(bare_paths)
    else {
        return false;
    };
    match path.as_slice() {
        [cfg] if cfg == "cfg" => arguments.iter().any(|argument| argument == &["test"]),
        [cfg_attr] if cfg_attr == "cfg_attr" => arguments
            .iter()
            .any(|argument| argument == &["mutants", "skip"]),
        _ => false,
    }
}

fn is_name(token: &str) -> bool {
    token
        .chars()
        .next()
        .is_some_and(|first| first == '_' || first.is_alphabetic())
}

/// The path at the start of an attribute's tokens, and what follows it.
fn read_path(tokens: &[String]) -> (Vec<String>, &[String]) {
    let mut path = Vec::new();
    let mut at = 0;
    // Names joined by `::`, up to whatever is not a name.
    while let Some(token) = tokens.get(at).filter(|token| is_name(token)) {
        path.push(token.clone());
        at += 1;
        if tokens.get(at).map(String::as_str) == Some("::") {
            at += 1;
        } else {
            break;
        }
    }
    (path, &tokens[at..])
}

/// An attribute's arguments as `syn::Attribute::parse_nested_meta` reads
/// them with a callback that consumes nothing: a comma-separated list of
/// bare paths. Anything else (`feature = "x"`, `all(..)`) is an error there,
/// and so `None` here.
fn bare_paths(mut tokens: &[String]) -> Option<Vec<Vec<String>>> {
    let mut paths = Vec::new();
    while !tokens.is_empty() {
        let (path, rest) = read_path(tokens);
        if path.is_empty() {
            return None;
        }
        paths.push(path);
        tokens = match rest.split_first() {
            None => rest,
            Some((comma, rest)) if comma == "," => rest,
            Some(_) => return None,
        };
    }
    Some(paths)
}

/// 1-based lines and 1-based char columns of byte offsets.
struct Lines<'s> {
    source: &'s str,
    starts: Vec<usize>,
}

impl<'s> Lines<'s> {
    fn new(source: &'s str) -> Self {
        let mut starts = vec![0];
        starts.extend(
            source
                .bytes()
                .enumerate()
                .filter_map(|(index, byte)| (byte == b'\n').then_some(index + 1)),
        );
        Self { source, starts }
    }

    fn position(&self, offset: usize) -> (u32, u32) {
        let line = self.starts.partition_point(|start| *start <= offset) - 1;
        let column = self.source[self.starts[line]..offset].chars().count();
        (line as u32 + 1, column as u32 + 1)
    }
}

// Tokens, as proc_macro2 sees them.

/// One token of a proc-macro token stream: an identifier, a literal, one
/// punctuation character, or a delimiter.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Ident(String),
    Literal(String),
    Punct(char),
    Open(char),
    Close(char),
}

fn push_token(token: &SyntaxToken, tokens: &mut Vec<Token>) {
    let text = token.text();
    let kind = token.kind();
    if kind.is_trivia() {
        return;
    }
    match kind {
        SyntaxKind::L_PAREN | SyntaxKind::L_BRACK | SyntaxKind::L_CURLY => {
            tokens.push(Token::Open(text.chars().next().expect("a delimiter")));
        }
        SyntaxKind::R_PAREN | SyntaxKind::R_BRACK | SyntaxKind::R_CURLY => {
            tokens.push(Token::Close(text.chars().next().expect("a delimiter")));
        }
        SyntaxKind::LIFETIME_IDENT => {
            tokens.push(Token::Punct('\''));
            tokens.push(Token::Ident(text.trim_start_matches('\'').to_owned()));
        }
        SyntaxKind::INT_NUMBER
        | SyntaxKind::FLOAT_NUMBER
        | SyntaxKind::STRING
        | SyntaxKind::BYTE_STRING
        | SyntaxKind::C_STRING
        | SyntaxKind::CHAR
        | SyntaxKind::BYTE => tokens.push(Token::Literal(text.to_owned())),
        _ if text
            .chars()
            .next()
            .is_some_and(|first| first == '_' || first.is_alphanumeric()) =>
        {
            tokens.push(Token::Ident(text.to_owned()));
        }
        _ => tokens.extend(text.chars().map(Token::Punct)),
    }
}

fn node_tokens(node: &SyntaxNode, tokens: &mut Vec<Token>) {
    node.descendants_with_tokens()
        .filter_map(NodeOrToken::into_token)
        .for_each(|token| push_token(&token, tokens));
}

/// A function body's tokens as `syn` prints its block: braces around its
/// statements, without its inner attributes.
fn block_tokens(list: &ast::StmtList) -> Vec<Token> {
    let mut tokens = vec![Token::Open('{')];
    statements(list)
        .iter()
        .for_each(|statement| node_tokens(statement, &mut tokens));
    tokens.push(Token::Close('}'));
    tokens
}

/// The tokens of `{ code }`, parsed as a function body: a replacement this
/// module writes, which always parses.
fn tokens_of_code(code: &str) -> Vec<Token> {
    let source = format!("fn f() {{ {code} }}");
    let (tree, _) = crate::rust_instrumenter::parse_source(&source).expect("a replacement parses");
    let list = tree
        .syntax()
        .descendants()
        .find_map(ast::StmtList::cast)
        .expect("a function body");
    block_tokens(&list)
}

/// A token stream printed as cargo-mutants' `ToPrettyString` prints it.
fn pretty(tokens: &[Token]) -> String {
    fn group(tokens: &[Token], at: &mut usize, out: &mut String) {
        // A group ends at its `}` or `)`, and the whole at the stream's end.
        loop {
            let Some(token) = tokens.get(*at).cloned() else {
                return;
            };
            *at += 1;
            let next = tokens.get(*at);
            let more = next.is_some_and(|next| !matches!(next, Token::Close(_)));
            match token {
                Token::Punct(character) => {
                    if character == '|'
                        && !out.is_empty()
                        && !out.ends_with(' ')
                        && !out.ends_with('|')
                    {
                        out.push(' ');
                    }
                    out.push(character);
                    if more && (out.ends_with("->") || character == ',' || character == ';') {
                        out.push(' ');
                    }
                }
                Token::Ident(text) | Token::Literal(text) => {
                    if out.ends_with('=') || out.ends_with("=>") || out.ends_with('|') {
                        out.push(' ');
                    }
                    out.push_str(&text);
                    match next {
                        Some(Token::Ident(_) | Token::Literal(_)) => out.push(' '),
                        Some(Token::Punct(',' | ';' | '<' | '>' | ':' | '.' | '!')) => {}
                        Some(Token::Punct(_)) => out.push(' '),
                        _ => {}
                    }
                }
                Token::Open(open) => {
                    out.push(open);
                    let mut inner = String::new();
                    group(tokens, at, &mut inner);
                    out.push_str(&inner);
                }
                Token::Close(close) => {
                    out.push(close);
                    return;
                }
            }
        }
    }
    let mut out = String::new();
    let mut at = 0;
    group(tokens, &mut at, &mut out);
    out
}

/// Code as cargo-mutants prints it, from its source text.
fn pretty_code(node: &SyntaxNode) -> String {
    let mut tokens = Vec::new();
    node_tokens(node, &mut tokens);
    pretty(&tokens)
}

// Values of a return type, after cargo-mutants' `fnvalue.rs`.

fn path_of(ty: &ast::Type) -> Option<ast::Path> {
    match ty {
        ast::Type::PathType(path) => path.path(),
        _ => None,
    }
}

/// A path that is one bare name: `syn::Path::is_ident`.
/// With no qualifier, leading `::`, arguments or anchor, a path's text is
/// its name.
fn is_ident(path: &ast::Path, name: &str) -> bool {
    path.syntax().text() == name
}

fn last_ident_is(path: &ast::Path, names: &[&str]) -> bool {
    last_name(path).is_some_and(|name| names.contains(&name.as_str()))
}

fn generic_arguments(path: &ast::Path) -> Option<Vec<ast::GenericArg>> {
    path.segment()
        .and_then(|segment| segment.generic_arg_list())
        .map(|list| list.generic_args().collect())
}

/// The first type argument of a path ending in `name`, past lifetimes.
fn first_type_argument(path: &ast::Path, name: &str) -> Option<ast::Type> {
    if last_name(path).as_deref() != Some(name) {
        return None;
    }
    for argument in generic_arguments(path)? {
        match argument {
            ast::GenericArg::TypeArg(argument) => return argument.ty(),
            ast::GenericArg::LifetimeArg(_) => {}
            _ => return None,
        }
    }
    None
}

/// `Box<T>` and the like: one argument, a type.
fn only_type_argument(path: &ast::Path, names: &[&str]) -> Option<(String, ast::Type)> {
    let name = last_name(path).filter(|name| names.contains(&name.as_str()))?;
    match generic_arguments(path)?.as_slice() {
        [ast::GenericArg::TypeArg(argument)] => argument.ty().map(|ty| (name, ty)),
        _ => None,
    }
}

fn known_map(path: &ast::Path) -> Option<(String, ast::Type, ast::Type)> {
    let name = last_name(path).filter(|name| ["BTreeMap", "HashMap"].contains(&name.as_str()))?;
    match generic_arguments(path)?.as_slice() {
        [
            ast::GenericArg::TypeArg(key),
            ast::GenericArg::TypeArg(value),
        ] => key
            .ty()
            .zip(value.ty())
            .map(|(key, value)| (name, key, value)),
        _ => None,
    }
}

/// A type with one type argument, which might be built from it.
fn maybe_collection_or_container(path: &ast::Path) -> Option<(String, ast::Type)> {
    let arguments = generic_arguments(path)?;
    let types = arguments
        .iter()
        .filter_map(|argument| match argument {
            ast::GenericArg::TypeArg(argument) => Some(argument),
            _ => None,
        })
        .collect::<Vec<_>>();
    match types.as_slice() {
        [only] => last_name(path).zip(only.ty()),
        _ => None,
    }
}

fn impl_iterator_item(ty: &ast::ImplTraitType) -> Option<ast::Type> {
    // The first `Iterator<Item = T>` among its bounds (a lifetime bound has
    // no type).
    ty.type_bound_list()
        .into_iter()
        .flat_map(|list| list.bounds())
        .filter_map(|bound| path_of(&bound.ty()?))
        .filter(|path| last_name(path).as_deref() == Some("Iterator"))
        .find_map(|path| {
            match generic_arguments(&path).and_then(|arguments| arguments.into_iter().next()) {
                Some(ast::GenericArg::AssocTypeArg(item))
                    if item.name_ref().is_some_and(|name| name.text() == "Item") =>
                {
                    item.ty()
                }
                _ => None,
            }
        })
}

const UNSIGNED: &[&str] = &["u8", "u16", "u32", "u64", "u128", "usize"];
const SIGNED: &[&str] = &["i8", "i16", "i32", "i64", "i128", "isize"];
const NONZERO_SIGNED: &[&str] = &[
    "NonZeroIsize",
    "NonZeroI8",
    "NonZeroI16",
    "NonZeroI32",
    "NonZeroI64",
    "NonZeroI128",
];
const NONZERO_UNSIGNED: &[&str] = &[
    "NonZeroUsize",
    "NonZeroU8",
    "NonZeroU16",
    "NonZeroU32",
    "NonZeroU64",
    "NonZeroU128",
];

fn is_one_of(path: &ast::Path, names: &[&str]) -> bool {
    names.iter().any(|name| is_ident(path, name))
}

fn map_all(values: Vec<String>, wrap: impl Fn(&str) -> String) -> Vec<String> {
    values.iter().map(|value| wrap(value)).collect()
}

fn slice_values(element: &ast::Type) -> Vec<String> {
    std::iter::once("Vec::leak(Vec::new())".to_owned())
        .chain(map_all(type_replacements(element), |value| {
            format!("Vec::leak(vec![{value}])")
        }))
        .collect()
}

/// Values cargo-mutants tries for a function returning `ty`.
fn type_replacements(ty: &ast::Type) -> Vec<String> {
    match ty {
        ast::Type::PathType(_) => {
            path_replacements(&path_of(ty).expect("a path type has its path"))
        }
        ast::Type::ArrayType(array) => {
            let element = array.ty().expect("an array type has its element type");
            let length = array
                .const_arg()
                .map(|length| pretty_code(length.syntax()))
                .unwrap_or_default();
            map_all(type_replacements(&element), |value| {
                format!("[{value}; {length}]")
            })
        }
        ast::Type::SliceType(slice) => slice
            .ty()
            .map_or_else(Vec::new, |element| slice_values(&element)),
        ast::Type::RefType(reference) => {
            let inner = reference.ty().expect("a reference type has its type");
            match &inner {
                ast::Type::PathType(_)
                    if reference.mut_token().is_none()
                        && path_of(&inner).is_some_and(|path| is_ident(&path, "str")) =>
                {
                    vec!["\"\"".to_owned(), "\"xyzzy\"".to_owned()]
                }
                ast::Type::SliceType(slice) => slice
                    .ty()
                    .map_or_else(Vec::new, |element| slice_values(&element)),
                _ => map_all(type_replacements(&inner), |value| {
                    format!("Box::leak(Box::new({value}))")
                }),
            }
        }
        ast::Type::TupleType(tuple) => {
            let mut products = vec![Vec::new()];
            for field in tuple.fields() {
                let values = type_replacements(&field);
                products = products
                    .into_iter()
                    .flat_map(|prefix: Vec<String>| {
                        values.iter().map(move |value| {
                            let mut next = prefix.clone();
                            next.push(value.clone());
                            next
                        })
                    })
                    .collect();
            }
            products
                .into_iter()
                .map(|values| format!("({})", values.join(", ")))
                .collect()
        }
        ast::Type::ImplTraitType(implemented) => match impl_iterator_item(implemented) {
            Some(item) => std::iter::once("::std::iter::empty()".to_owned())
                .chain(map_all(type_replacements(&item), |value| {
                    format!("::std::iter::once({value})")
                }))
                .collect(),
            None => Vec::new(),
        },
        ast::Type::NeverType(_) => Vec::new(),
        _ => vec!["Default::default()".to_owned()],
    }
}

fn path_replacements(path: &ast::Path) -> Vec<String> {
    let strings = |values: &[&str]| values.iter().map(|value| (*value).to_owned()).collect();
    if is_ident(path, "bool") {
        return strings(&["true", "false"]);
    }
    if is_ident(path, "String") {
        return strings(&["String::new()", "\"xyzzy\".into()"]);
    }
    if is_ident(path, "str") {
        return strings(&["\"\"", "\"xyzzy\""]);
    }
    if is_one_of(path, UNSIGNED) {
        return strings(&["0", "1"]);
    }
    if is_one_of(path, SIGNED) {
        return strings(&["0", "1", "-1"]);
    }
    if last_ident_is(path, NONZERO_SIGNED) {
        return strings(&["1.try_into().unwrap()", "(-1).try_into().unwrap()"]);
    }
    if last_ident_is(path, NONZERO_UNSIGNED) {
        return strings(&["1.try_into().unwrap()"]);
    }
    if let Some(inner) = first_type_argument(path, "NonZero") {
        return if path_of(&inner).is_some_and(|inner| is_one_of(&inner, UNSIGNED)) {
            strings(&["1.try_into().unwrap()"])
        } else {
            strings(&["1.try_into().unwrap()", "(-1).try_into().unwrap()"])
        };
    }
    if is_one_of(path, &["f32", "f64"]) {
        return strings(&["0.0", "1.0", "-1.0"]);
    }
    if last_name(path).as_deref() == Some("Result") {
        return match first_type_argument(path, "Result") {
            Some(ok) => map_all(type_replacements(&ok), |value| format!("Ok({value})")),
            None => strings(&["Ok(Default::default())"]),
        };
    }
    if last_name(path).as_deref() == Some("HttpResponse") {
        return strings(&["HttpResponse::Ok().finish()"]);
    }
    if let Some(some) = first_type_argument(path, "Option") {
        return std::iter::once("None".to_owned())
            .chain(map_all(type_replacements(&some), |value| {
                format!("Some({value})")
            }))
            .collect();
    }
    if let Some(element) = first_type_argument(path, "Vec") {
        return std::iter::once("vec![]".to_owned())
            .chain(map_all(type_replacements(&element), |value| {
                format!("vec![{value}]")
            }))
            .collect();
    }
    if let Some(borrowed) = first_type_argument(path, "Cow") {
        return type_replacements(&borrowed)
            .into_iter()
            .flat_map(|value| {
                [
                    format!("Cow::Borrowed({value})"),
                    format!("Cow::Owned({value}.to_owned())"),
                ]
            })
            .collect();
    }
    if let Some((container, inner)) =
        only_type_argument(path, &["Box", "Cell", "RefCell", "Arc", "Rc", "Mutex"])
    {
        return map_all(type_replacements(&inner), |value| {
            format!("{container}::new({value})")
        });
    }
    if let Some((collection, inner)) = only_type_argument(
        path,
        &[
            "BinaryHeap",
            "BTreeSet",
            "HashSet",
            "LinkedList",
            "VecDeque",
        ],
    ) {
        return std::iter::once(format!("{collection}::new()"))
            .chain(map_all(type_replacements(&inner), |value| {
                format!("{collection}::from_iter([{value}])")
            }))
            .collect();
    }
    if let Some((map, key, value)) = known_map(path) {
        let keys = type_replacements(&key);
        let values = type_replacements(&value);
        return std::iter::once(format!("{map}::new()"))
            .chain(keys.iter().flat_map(|key| {
                let map = &map;
                values
                    .iter()
                    .map(move |value| format!("{map}::from_iter([({key}, {value})])"))
            }))
            .collect();
    }
    if let Some((collection, inner)) = maybe_collection_or_container(path) {
        return std::iter::once(format!("{collection}::new()"))
            .chain(type_replacements(&inner).into_iter().flat_map(|value| {
                [
                    format!("{collection}::from_iter([{value}])"),
                    format!("{collection}::new({value})"),
                    format!("{collection}::from({value})"),
                ]
            }))
            .collect();
    }
    strings(&["Default::default()"])
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use serde_json::Value;

    use super::*;

    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/cargo-mutants-parity"
    );

    fn render(genre: &str, start: (u32, u32), end: (u32, u32), replacement: &str) -> String {
        format!(
            "{}:{}-{}:{} {genre} {replacement}",
            start.0, start.1, end.0, end.1
        )
    }

    fn pair(value: &Value) -> (u32, u32) {
        let pair = value.as_array().expect("a [line, column] pair");
        (
            pair[0].as_u64().expect("a line") as u32,
            pair[1].as_u64().expect("a column") as u32,
        )
    }

    fn sources(directory: &Path, found: &mut Vec<std::path::PathBuf>) {
        for entry in fs::read_dir(directory).expect("a fixture directory") {
            let path = entry.expect("an entry").path();
            if path.is_dir() {
                sources(&path, found);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                found.push(path);
            }
        }
    }

    /// A line diff of two lists: `-` for cargo-mutants' entries Supercov
    /// lacks, `+` for Supercov's entries cargo-mutants lacks.
    fn diff(expected: &[String], actual: &[String]) -> String {
        let (n, m) = (expected.len(), actual.len());
        let mut lengths = vec![vec![0usize; m + 1]; n + 1];
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                lengths[i][j] = if expected[i] == actual[j] {
                    lengths[i + 1][j + 1] + 1
                } else {
                    lengths[i + 1][j].max(lengths[i][j + 1])
                };
            }
        }
        let mut output = String::new();
        let (mut i, mut j) = (0, 0);
        while i < n || j < m {
            if i < n && j < m && expected[i] == actual[j] {
                i += 1;
                j += 1;
            } else if j < m && (i == n || lengths[i][j + 1] >= lengths[i + 1][j]) {
                output.push_str(&format!("  + {}\n", actual[j]));
                j += 1;
            } else {
                output.push_str(&format!("  - {}\n", expected[i]));
                i += 1;
            }
        }
        output
    }

    #[test]
    fn rust_mutants_match_cargo_mutants_on_every_fixture() {
        let mut files = Vec::new();
        sources(&Path::new(FIXTURE).join("src"), &mut files);
        files.sort();
        assert!(files.len() >= 3, "fixtures: {files:?}");
        let mut failures = Vec::new();
        for path in &files {
            let name = path.strip_prefix(FIXTURE).unwrap().display().to_string();
            let source = fs::read_to_string(path).expect("a readable fixture");
            let golden_path = path.with_file_name(format!(
                "{}.cargo-mutants.json",
                path.file_name().unwrap().to_string_lossy()
            ));
            let golden: Value = serde_json::from_str(
                &fs::read_to_string(&golden_path).expect("a golden beside each fixture"),
            )
            .expect("a JSON golden");
            assert_eq!(golden["mutantSet"], MUTANT_SET, "{name}");
            let expected = golden["mutants"]
                .as_array()
                .expect("mutants")
                .iter()
                .map(|entry| {
                    render(
                        entry["genre"].as_str().unwrap(),
                        pair(&entry["start"]),
                        pair(&entry["end"]),
                        entry["replacement"].as_str().unwrap(),
                    )
                })
                .collect::<Vec<_>>();
            let actual = enumerate(&source)
                .expect("the fixture parses")
                .iter()
                .map(|mutant| render(mutant.genre, mutant.start, mutant.end, &mutant.replacement))
                .collect::<Vec<_>>();
            if expected != actual {
                failures.push(format!(
                    "{name}: cargo-mutants {} mutants, Supercov {} (- cargo-mutants only, + Supercov only)\n{}",
                    expected.len(),
                    actual.len(),
                    diff(&expected, &actual)
                ));
            }
        }
        assert!(failures.is_empty(), "\n{}", failures.join("\n"));
    }

    #[test]
    fn pretty_prints_as_cargo_mutants_does() {
        let printed = |code: &str| pretty(&tokens_of_code(code));
        assert_eq!(printed("match x { a => b }"), "{match x{a => b}}");
        assert_eq!(printed("fair || foul"), "{fair || foul}");
        assert_eq!(
            printed("Vec::<'a, T>::new(x, y)"),
            "{Vec::<'a, T>::new(x, y)}"
        );
        assert_eq!(
            printed("let x: [u8; 4] = [0; 4];"),
            "{let x:[u8; 4]=[0; 4];}"
        );
    }

    #[test]
    fn a_descriptor_names_a_deletion() {
        let mutants = enumerate("fn f(a: bool) -> bool { !a }\n").unwrap();
        let unary = mutants
            .iter()
            .find(|mutant| mutant.genre == "UnaryOperator")
            .unwrap();
        assert_eq!(unary.descriptor(), REMOVED);
        assert_eq!(mutants[0].descriptor(), "true");
        assert!(enumerate("fn f( {").is_none());
    }

    /// Each mutant is linked to the points the run measured, and its weak
    /// verdict read from them: a body by its function, an arm by its own
    /// selection, a guard by its outcomes.
    #[test]
    fn listed_mutants_are_linked_and_judged_by_what_the_run_recorded() {
        use crate::js_weak_mutation::{Observations, Status, verdict};
        let file = "src/lib.rs";
        let source = "pub fn f(n: i32, b: bool) -> i32 {\n    match n {\n        0 => 1,\n        x if x > 2 => 2,\n        _ => if !b { -n } else { n },\n    }\n}\n";
        let manifest = crate::rust_instrumenter::build_rust_manifest(file, source).unwrap();
        let mut recorded = manifest
            .points
            .iter()
            .map(|point| point.id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        recorded.extend(
            manifest
                .decisions
                .iter()
                .map(|decision| decision.id.clone()),
        );
        recorded.extend(manifest.branches.iter().flat_map(|branch| {
            branch
                .alternatives
                .iter()
                .map(|alternative| alternative.id.clone())
        }));
        let listed = list(file, source, &recorded).unwrap();
        let find = |mutator: &str, replacement: &str| {
            listed
                .iter()
                .find(|mutant| mutant.mutator == mutator && mutant.replacement == replacement)
                .unwrap_or_else(|| panic!("no {mutator} {replacement}: {listed:?}"))
        };
        let body = find("FnValue", "0");
        assert!(body.function.is_some() && body.statement.is_none());
        let arm = find("MatchArm", REMOVED);
        assert!(
            arm.statement
                .as_deref()
                .is_some_and(|id| id.ends_with(":selected"))
        );
        let guard = find("MatchArmGuard", "true");
        assert!(guard.decision.is_some());
        let mut seen = Observations::default();
        assert_eq!(verdict(body, &seen).0, Status::NotReached);
        seen.hits.insert(body.function.clone().unwrap());
        assert_eq!(verdict(body, &seen).0, Status::Unknown);
        assert_eq!(verdict(find("FnValue", "1"), &seen).0, Status::Unknown);
        seen.hits.extend(
            recorded
                .iter()
                .filter(|id| id.starts_with("rs:statement:"))
                .cloned(),
        );
        assert_eq!(verdict(arm, &seen).0, Status::NotReached);
        seen.hits.insert(arm.statement.clone().unwrap());
        assert_eq!(verdict(arm, &seen).0, Status::Infected { proxy: true });
        let decision = guard.decision.as_ref().unwrap().id.clone();
        assert_eq!(verdict(guard, &seen).0, Status::NotReached);
        seen.decisions
            .insert(decision.clone(), vec![(vec![Some(true)], true)]);
        assert_eq!(verdict(guard, &seen).0, Status::NotInfected);
        assert_eq!(
            verdict(find("MatchArmGuard", "false"), &seen).0,
            Status::Infected { proxy: false }
        );
        let not = listed
            .iter()
            .find(|mutant| mutant.mutator == "UnaryOperator" && mutant.original.starts_with('!'))
            .unwrap();
        assert_eq!(verdict(not, &seen).0, Status::Infected { proxy: false });
        let negation = listed
            .iter()
            .find(|mutant| mutant.mutator == "UnaryOperator" && mutant.original.starts_with('-'))
            .unwrap();
        assert_eq!(verdict(negation, &seen).0, Status::Unknown);
        assert_eq!(
            verdict(find("BinaryOperator", "=="), &seen).0,
            Status::Unknown
        );
        // A run that measured a point the source no longer has: changed.
        recorded.insert("rs:statement:gone".into());
        assert!(list(file, source, &recorded).is_none());
    }

    /// What cargo-mutants skips is skipped wherever it is, and each kind of
    /// node is walked through.
    #[test]
    fn skipped_code_and_every_kind_of_node_are_walked_as_cargo_mutants_walks_them() {
        assert_eq!(
            enumerate("#![cfg(test)]\nfn f() -> u8 { 1 + 1 }\n").unwrap(),
            vec![]
        );
        assert_eq!(enumerate("struct S;\n").unwrap(), vec![]);
        let source = concat!(
            "#[cfg(test)] impl S { fn a() -> u8 { 1 + 1 } }\n",
            "#[mutants::skip] trait T { fn b() -> u8 { 1 + 1 } }\n",
            "#[cfg_attr(feature = \"x\", mutants::skip)] mod m { fn c() -> u8 { 1 + 1 } }\n",
            "#[cfg(all(test, unix))] fn d() -> u8 { 1 + 1 }\n",
            "#[cfg_attr(test, inline)] fn d2() -> u8 { 1 + 1 }\n",
            "#[cfg_attr(test, ::x, y::z)] fn d3() -> u8 { 1 + 1 }\n",
            "#[path = \"x\"] mod n {}\n",
            "#[allow(::bad)] fn d4() {}\n",
            "fn e(a: u8, s: S) -> u8 {\n",
            "    let mut x = 0;\n",
            "    x = 1;\n",
            "    fn nested() {}\n",
            "    #[cfg(test)] f(a + 1);\n",
            "    #[cfg(test)] s.g(a + 1);\n",
            "    (|v: u8| v + 1)(a);\n",
            "    S::with_capacity(a + 1);\n",
            "    s.with_capacity(a + 1);\n",
            "    (s.f)(a + 1);\n",
            "    #[cfg(test)] { a + 1 };\n",
            "    #[cfg(test)] -a;\n",
            "    *&a;\n",
            "    #[cfg(test)] match a { _ => 1 };\n",
            "    match a { 1 => 2, _ if a > 3 => 4, (_) => 5 };\n",
            "    #[cfg(test)] S { a, ..s };\n",
            "    S { a: 1, ..s };\n",
            "    S { /* first */ a // trailing\n        , 0: 1, b: 2 /* last */, ..s };\n",
            "    S { a: 1 };\n",
            "    x\n",
            "}\n",
            "unsafe fn u() -> u8 { 1 + 1 }\n",
            "trait U { fn new() -> u8 { 1 } fn empty() {} fn declared(); fn given() -> bool { true && false } }\n",
            "impl S { fn new() -> u8 { 1 + 1 } fn empty() {} }\n",
            "extern \"C\" { fn x(); }\n",
        );
        let mutants = enumerate(source).unwrap();
        let named = mutants
            .iter()
            .map(|mutant| {
                format!(
                    "{}:{} {} {}",
                    mutant.start.0,
                    mutant.start.1,
                    mutant.genre,
                    mutant.descriptor()
                )
            })
            .collect::<Vec<_>>();
        // Skipped: the `cfg(test)` impl, the skipped trait and module, the
        // attributed call, method call, block, negation, match and literal,
        // the `with_capacity` calls, `unsafe fn`, `new` and empty bodies.
        for line in [1, 2, 13, 14, 16, 17, 20, 22, 24, 30, 31, 33] {
            assert!(
                !named
                    .iter()
                    .any(|entry| entry.starts_with(&format!("{line}:"))),
                "line {line}: {named:?}"
            );
        }
        // Not skipped: a `cfg` with more than names, `cfg_attr` of no skip.
        for line in [3, 4, 5, 6, 19] {
            assert!(
                named
                    .iter()
                    .any(|entry| entry.starts_with(&format!("{line}:"))),
                "line {line}: {named:?}"
            );
        }
        assert!(
            named.contains(&"15:16 BinaryOperator -".to_owned()),
            "{named:?}"
        );
        assert!(
            named.contains(&"18:13 BinaryOperator -".to_owned()),
            "{named:?}"
        );
        assert!(
            named.contains(&"23:15 MatchArm removed".to_owned()),
            "{named:?}"
        );
        assert!(
            named.contains(&"23:28 MatchArmGuard true".to_owned()),
            "{named:?}"
        );
        assert!(
            named.contains(&"25:9 StructField removed".to_owned()),
            "{named:?}"
        );
        // A field's span runs past comments to its comma; `0: 1` is not a
        // named field; the last field without a comma ends at itself.
        let fields = mutants
            .iter()
            .filter(|mutant| mutant.genre == "StructField" && mutant.start.0 >= 26)
            .map(|mutant| &source[mutant.range])
            .collect::<Vec<_>>();
        assert_eq!(
            fields,
            ["a // trailing\n        ,", "b: 2 /* last */,"],
            "{named:?}"
        );
        assert!(
            named.contains(&"32:87 BinaryOperator ||".to_owned()),
            "{named:?}"
        );
        assert!(
            named.contains(&"23:40 MatchArm removed".to_owned()),
            "{named:?}"
        );
        assert_eq!(
            named.len(),
            37,
            "cargo-mutants 27.1.0 lists 37 of this source: {named:?}"
        );
    }

    /// The pretty printer spaces what cargo-mutants' printer spaces.
    #[test]
    fn every_spacing_rule_of_the_printer() {
        let printed = |code: &str| pretty(&tokens_of_code(code));
        assert_eq!(printed("a | b; c"), "{a | b; c}");
        assert_eq!(printed("f::<u8>(a, 1) - -1"), "{f::<u8>(a, 1)--1}");
        assert_eq!(printed("b!(); a.c"), "{b!(); a.c}");
        assert_eq!(printed("fn g() -> T {}"), "{fn g()-> T{}}");
        // A `|` after a group is spaced; `||` and a leading `|` are not
        // spaced before (what follows a `|` is).
        assert_eq!(printed("f() | g()"), "{f() | g()}");
        assert_eq!(printed("a || b"), "{a || b}");
        assert_eq!(printed("|x| x"), "{| x | x}");
        assert_eq!(printed("x = y"), "{x = y}");
    }

    /// Links are made only to what the run recorded, and every kind of site
    /// is linked.
    #[test]
    fn a_mutant_is_linked_only_to_what_the_run_recorded() {
        let file = "src/lib.rs";
        let source = "pub fn f(a: u8, b: bool) -> u8 {\n    let c = |x: u8| x == a;\n    match a { 0 => 1, x if b => 2, _ => S { y: -1, ..S::d() }.y }\n}\n";
        assert!(list(file, "fn broken( {", &Default::default()).is_none());
        // No function or block at all, and blocks of items and statements
        // with no tail.
        assert!(
            list(file, "struct S;\n", &Default::default()).is_some_and(|listed| listed.is_empty())
        );
        assert!(
            list(
                file,
                "fn g(a: u8) {\n    fn inner() {}\n    let b = a + 1;\n}\n",
                &Default::default()
            )
            .is_some_and(|listed| !listed.is_empty())
        );
        // Nothing recorded: no link at all.
        let bare = list(file, source, &Default::default()).unwrap();
        assert!(bare.iter().all(|mutant| mutant.statement.is_none()
            && mutant.function.is_none()
            && mutant.decision.is_none()
            && mutant.probe.is_none()
            && mutant.compare.is_none()));
        // A decision the run recorded is not a point: no change is read
        // from it.
        let decisions = crate::rust_instrumenter::build_rust_manifest(file, source)
            .unwrap()
            .decisions
            .iter()
            .map(|decision| decision.id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let linked = list(file, source, &decisions).unwrap();
        assert!(linked.iter().any(|mutant| mutant.decision.is_some()));
        let mut gone = decisions.clone();
        gone.insert("rs:function:gone".into());
        assert!(list(file, source, &gone).is_none());
        // Value sites recorded link the probes.
        let mutants = enumerate(source).unwrap();
        let mut sites = std::collections::BTreeSet::new();
        for mutant in &mutants {
            if let Site::Operator { expression } = &mutant.site {
                sites.insert(value_site(file, *expression, "negate"));
                sites.insert(value_site(file, *expression, "compare"));
                sites.insert(value_site(file, *expression, &mutant.replacement));
            }
            if let (Site::Body { .. }, Some(function)) = (&mutant.site, mutant.function) {
                sites.insert(value_site(file, function, "return"));
            }
        }
        let probed = list(file, source, &sites).unwrap();
        let negation = probed
            .iter()
            .find(|m| m.mutator == "UnaryOperator")
            .unwrap();
        assert!(negation.probe.is_some());
        let equality = probed.iter().find(|m| m.replacement == "!=").unwrap();
        assert!(equality.probe.is_none() && equality.compare.is_none());
        assert!(
            probed
                .iter()
                .any(|m| m.mutator == "FnValue" && m.probe.is_some())
        );
        assert!(probed.iter().any(|m| m.mutator == "StructField"));
        assert_eq!(arithmetic_code(""), ' ');
    }

    /// A mutant whose operator binds otherwise than the one it replaces
    /// regroups the expression around it, as cargo-mutants' text does
    /// (`a - b * c` made `a - b + c` is `(a - b) + c`): its switch holds that
    /// whole expression. One that groups alike stays at its own operator.
    #[test]
    fn a_mutant_that_regroups_its_expression_is_switched_around_it() {
        let source = "pub fn f(a: i32, b: i32, c: i32, p: bool, q: bool, r: bool) -> i32 {\n    if p && q && r { a - b * c } else { a + b - c }\n}\n";
        let mutants = enumerate(source).unwrap();
        let site = |at: &str, replacement: &str| {
            let mutant = mutants
                .iter()
                .find(|mutant| {
                    &source[mutant.range] == at.split('@').next().unwrap()
                        && mutant.start.1 == at.split('@').nth(1).unwrap().parse::<u32>().unwrap()
                        && mutant.replacement == replacement
                })
                .unwrap_or_else(|| panic!("no {at} {replacement}"));
            match mutant.site {
                Site::Operator { expression } => source[expression].to_owned(),
                _ => panic!("{mutant:?}"),
            }
        };
        assert_eq!(site("*@28", "+"), "a - b * c");
        assert_eq!(site("*@28", "/"), "b * c");
        assert_eq!(site("&&@10", "||"), "p && q && r");
        assert_eq!(site("&&@15", "||"), "p && q && r");
        assert_eq!(site("-@24", "+"), "a - b * c");
        assert_eq!(site("+@43", "-"), "a + b");
    }

    /// A `const fn`'s body is declared and never probed: the run records
    /// its points, never hit, and a mutant there is linked to none of them,
    /// so it is reached for all the run knows, not `not-reached`.
    #[test]
    fn a_mutant_in_a_const_fn_is_linked_to_nothing() {
        let file = "src/lib.rs";
        let source = "pub const fn named(s: &str) -> bool {\n    !s.is_empty()\n}\npub fn plain(s: &str) -> bool {\n    !s.is_empty()\n}\npub const fn look(n: u8) -> u8 {\n    match n {\n        1 => 2,\n        _ => 3,\n    }\n}\n";
        let manifest = crate::rust_instrumenter::build_rust_manifest(file, source).unwrap();
        let recorded = manifest
            .points
            .iter()
            .map(|point| point.id.clone())
            .chain(manifest.unmeasured.iter().cloned())
            .collect::<std::collections::BTreeSet<_>>();
        let listed = list(file, source, &recorded).unwrap();
        let negations = listed
            .iter()
            .filter(|mutant| mutant.mutator == "UnaryOperator")
            .collect::<Vec<_>>();
        assert_eq!(negations.len(), 2, "{listed:?}");
        assert!(
            negations[0].statement.is_none() && negations[0].function.is_none(),
            "{:?}",
            negations[0]
        );
        assert!(negations[1].statement.is_some(), "{:?}", negations[1]);
        // A deleted arm of a `const fn`'s `match`: its branch is in the
        // run's record, never taken; the mutant is linked to none of it.
        let mut with_branches = recorded.clone();
        with_branches.extend(manifest.branches.iter().flat_map(|branch| {
            branch
                .alternatives
                .iter()
                .map(|alternative| alternative.id.clone())
        }));
        let arm = list(file, source, &with_branches)
            .unwrap()
            .into_iter()
            .find(|mutant| mutant.mutator == "MatchArm")
            .unwrap();
        assert!(arm.statement.is_none() && arm.function.is_none(), "{arm:?}");
    }
}
