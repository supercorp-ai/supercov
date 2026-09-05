//! Supercov-owned Rust parsing and obligation discovery.
//!
//! The first private frontend deliberately starts from a lossless concrete
//! syntax tree. LLVM/rustc coverage remains a development oracle; it is not a
//! product input. Probe insertion and Cargo execution build on this exact
//! source denominator.

use std::collections::BTreeSet;

use ra_ap_syntax::{
    AstNode, Edition, SourceFile, SyntaxKind, TextRange,
    ast::{self, BinaryOp, HasAttrs, HasName, LogicOp},
};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::{
    coverage_analysis::PointKind,
    coverage_report::{
        BranchAlternativeMeta, BranchMeta, CoverageManifest, DecisionMeta, PointMeta,
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RustInstrumenterError {
    SourceTooLarge,
    Parse(Vec<String>),
    InvalidRange,
    InvalidRuntimePath,
}

impl std::fmt::Display for RustInstrumenterError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SourceTooLarge => write!(formatter, "Rust source exceeds the parser range"),
            Self::Parse(errors) => write!(formatter, "Rust parse failed: {}", errors.join("; ")),
            Self::InvalidRange => write!(formatter, "Rust parser returned an invalid range"),
            Self::InvalidRuntimePath => write!(formatter, "invalid generated Rust runtime path"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RustInstrumentedSource {
    pub code: String,
    pub manifest: CoverageManifest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InsertionKind {
    End,
    Direct,
    Start,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Insertion {
    offset: usize,
    kind: InsertionKind,
    scope_len: usize,
    rank: usize,
    text: String,
}

fn valid_runtime_path(path: &str) -> bool {
    let mut parts = path.split("::");
    if !matches!(parts.next(), Some("crate")) {
        return false;
    }
    let parts = parts.collect::<Vec<_>>();
    !parts.is_empty()
        && parts.into_iter().all(|part| {
            !part.is_empty()
                && part.bytes().enumerate().all(|(index, byte)| {
                    byte == b'_'
                        || byte.is_ascii_alphabetic()
                        || (index > 0 && byte.is_ascii_digit())
                })
        })
}

/// Report whether rustc will evaluate this node at compile time.
///
/// Runtime probes cannot appear anywhere this is true: `condition`, `decision`
/// and `hit` are not `const fn`, so emitting a call here is not a bad
/// measurement but a build failure (E0015). bytes-1.12.1 hit exactly that with
/// `const ITERS: usize = if cfg!(miri) { 100 } else { 1_000 };`.
///
/// `ConstArg` is the shared node for enum discriminants, array lengths, const
/// generic arguments and const parameter defaults, so matching it covers all
/// four. The remaining case is an array repeat expression, `[value; count]`,
/// where only the count after the semicolon is const-evaluated.
/// Report whether the source's doc comments contain a fenced code block.
///
/// rustdoc turns fenced blocks in `///`, `//!` and `#[doc]` text into doctest
/// crates. The scan is line-based and deliberately coarse: a fence inside a
/// doc comment declares the limitation even when the fence is `ignore`d, which
/// over-declares the unmeasured surface rather than ever under-declaring it.
fn in_const_context(node: &ra_ap_syntax::SyntaxNode) -> bool {
    let start = node.text_range().start();
    node.ancestors().any(|ancestor| {
        ast::Fn::cast(ancestor.clone()).is_some_and(|function| function.const_token().is_some())
            || ast::BlockExpr::cast(ancestor.clone())
                .is_some_and(|block| block.const_token().is_some())
            || ast::Const::can_cast(ancestor.kind())
            || ast::Static::can_cast(ancestor.kind())
            || ast::ConstArg::can_cast(ancestor.kind())
            || ast::ArrayExpr::cast(ancestor).is_some_and(|array| {
                array
                    .semicolon_token()
                    .is_some_and(|semicolon| start >= semicolon.text_range().end())
            })
    })
}

/// Report whether this node sits inside a `GlobalAlloc` implementation.
///
/// The probe runtime allocates, so a probe inside `alloc` calls back into
/// `alloc`, which probes again, until the stack is gone. bytes-1.12.1's
/// tests/test_bytes_odd_alloc.rs installs a `#[global_allocator]`, and the
/// instrumented binary died with SIGSEGV before libtest could even list its
/// tests -- while the uninstrumented one listed them fine.
///
/// The general rule this enforces is that nothing the runtime itself calls can
/// carry a probe, and `#[global_allocator]` is the one way a user crate gets
/// onto that path. A `GlobalAlloc` impl is skipped whether or not it is the
/// registered allocator, because the registering `static` may live in another
/// file: declining a handful of allocator bodies costs almost no exactness,
/// while instrumenting the live one costs the whole run.
fn in_global_allocator(node: &ra_ap_syntax::SyntaxNode) -> bool {
    node.ancestors().any(|ancestor| {
        ast::Impl::cast(ancestor).is_some_and(|block| {
            block.trait_().is_some_and(|implemented| {
                implemented
                    .syntax()
                    .descendants_with_tokens()
                    .filter_map(|element| element.into_token())
                    .any(|token| token.kind() == SyntaxKind::IDENT && token.text() == "GlobalAlloc")
            })
        })
    })
}

/// Report whether a probe placed at this node could not run correctly.
fn cannot_carry_probe(node: &ra_ap_syntax::SyntaxNode) -> bool {
    in_const_context(node) || in_global_allocator(node)
}

fn range_offsets(range: TextRange) -> (usize, usize) {
    (usize::from(range.start()), usize::from(range.end()))
}

fn push_wrapper(
    insertions: &mut Vec<Insertion>,
    range: TextRange,
    scope: TextRange,
    rank: usize,
    prefix: String,
    suffix: String,
) {
    let (start, end) = range_offsets(range);
    let (scope_start, scope_end) = range_offsets(scope);
    let scope_len = scope_end - scope_start;
    insertions.push(Insertion {
        offset: start,
        kind: InsertionKind::Start,
        scope_len,
        rank,
        text: prefix,
    });
    insertions.push(Insertion {
        offset: end,
        kind: InsertionKind::End,
        scope_len,
        rank,
        text: suffix,
    });
}

fn push_direct(insertions: &mut Vec<Insertion>, offset: usize, text: String) {
    insertions.push(Insertion {
        offset,
        kind: InsertionKind::Direct,
        scope_len: 0,
        rank: 0,
        text,
    });
}

fn apply_insertions(
    source: &str,
    mut insertions: Vec<Insertion>,
) -> Result<String, RustInstrumenterError> {
    if insertions
        .iter()
        .any(|edit| edit.offset > source.len() || !source.is_char_boundary(edit.offset))
    {
        return Err(RustInstrumenterError::InvalidRange);
    }
    insertions.sort_by(|left, right| {
        left.offset.cmp(&right.offset).then_with(|| {
            let kind_order = |kind: InsertionKind| match kind {
                InsertionKind::End => 0,
                InsertionKind::Direct => 1,
                InsertionKind::Start => 2,
            };
            kind_order(left.kind)
                .cmp(&kind_order(right.kind))
                .then_with(|| match left.kind {
                    InsertionKind::End => left
                        .scope_len
                        .cmp(&right.scope_len)
                        .then_with(|| right.rank.cmp(&left.rank)),
                    InsertionKind::Direct => std::cmp::Ordering::Equal,
                    InsertionKind::Start => right
                        .scope_len
                        .cmp(&left.scope_len)
                        .then_with(|| left.rank.cmp(&right.rank)),
                })
        })
    });

    let mut output = source.to_owned();
    let mut index = insertions.len();
    while index > 0 {
        let offset = insertions[index - 1].offset;
        let start = insertions[..index].partition_point(|insertion| insertion.offset < offset);
        let text = insertions[start..index]
            .iter()
            .map(|insertion| insertion.text.as_str())
            .collect::<String>();
        output.insert_str(offset, &text);
        index = start;
    }
    Ok(output)
}

fn add_manifest_limitation(manifest: &mut CoverageManifest, file: &str, id: &str, reason: &str) {
    if manifest
        .limitations
        .iter()
        .any(|limitation| limitation.get("id").and_then(|value| value.as_str()) == Some(id))
    {
        return;
    }
    manifest.limitations.push(json!({
        "id": id,
        "kind": "rust-frontend-readiness",
        "file": file,
        "line": 1,
        "column": 0,
        "source": "",
        "reason": reason
    }));
}

fn allocate_frame_name(
    file: &str,
    condition: &ast::Expr,
    kind: &str,
    identifiers: &mut BTreeSet<String>,
) -> String {
    let id = stable_id(file, "decision", condition.syntax().text_range(), kind);
    let suffix = id.rsplit(':').next().unwrap_or("decision");
    let base = format!("__supercov_decision_{suffix}");
    let mut candidate = base.clone();
    let mut attempt = 0_usize;
    while !identifiers.insert(candidate.clone()) {
        attempt += 1;
        candidate = format!("{base}_{attempt}");
    }
    candidate
}

/// The `const` naming a match's alternative IDs. Upper case, so it raises no
/// naming lint in a crate that denies warnings.
fn allocate_table_name(
    file: &str,
    expression: &ast::MatchExpr,
    identifiers: &mut BTreeSet<String>,
) -> String {
    let id = stable_id(file, "match", expression.syntax().text_range(), "arms");
    let suffix = id
        .rsplit(':')
        .next()
        .unwrap_or("match")
        .to_ascii_uppercase();
    let base = format!("__SUPERCOV_ARMS_{suffix}");
    let mut candidate = base.clone();
    let mut attempt = 0_usize;
    while !identifiers.insert(candidate.clone()) {
        attempt += 1;
        candidate = format!("{base}_{attempt}");
    }
    candidate
}

impl std::error::Error for RustInstrumenterError {}

struct SourceLocations<'a> {
    source: &'a str,
    line_starts: Vec<usize>,
}

impl<'a> SourceLocations<'a> {
    fn new(source: &'a str) -> Self {
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
        }
    }

    fn range(&self, range: TextRange) -> Result<(usize, usize), RustInstrumenterError> {
        let start = usize::from(range.start());
        let end = usize::from(range.end());
        if start > end
            || end > self.source.len()
            || !self.source.is_char_boundary(start)
            || !self.source.is_char_boundary(end)
        {
            return Err(RustInstrumenterError::InvalidRange);
        }
        Ok((start, end))
    }

    fn line_column(&self, offset: usize) -> (usize, usize) {
        let line_index = self.line_starts.partition_point(|start| *start <= offset) - 1;
        (line_index + 1, offset - self.line_starts[line_index])
    }

    fn text(&self, range: TextRange) -> Result<String, RustInstrumenterError> {
        let (start, end) = self.range(range)?;
        Ok(self.source[start..end].trim().to_owned())
    }
}

fn stable_id(file: &str, kind: &str, range: TextRange, suffix: &str) -> String {
    let mut hash = Sha256::new();
    let start = usize::from(range.start()).to_string();
    let end = usize::from(range.end()).to_string();
    for value in [file, kind, &start, &end, suffix] {
        hash.update(value.as_bytes());
        hash.update([0]);
    }
    let digest = hash.finalize();
    let mut encoded = String::with_capacity(24);
    for byte in &digest[..12] {
        use std::fmt::Write as _;
        write!(&mut encoded, "{byte:02x}").expect("writing to a string cannot fail");
    }
    format!("rs:{kind}:{encoded}")
}

struct RustObligationCollector<'a> {
    file: &'a str,
    locations: SourceLocations<'a>,
    manifest: CoverageManifest,
    point_ids: BTreeSet<String>,
    decision_ids: BTreeSet<String>,
    branch_ids: BTreeSet<String>,
    limitation_ids: BTreeSet<&'static str>,
    error: Option<RustInstrumenterError>,
}

impl<'a> RustObligationCollector<'a> {
    fn new(file: &'a str, source: &'a str) -> Self {
        Self {
            file,
            locations: SourceLocations::new(source),
            manifest: CoverageManifest {
                unmeasured: Vec::new(),
                decisions: Vec::new(),
                points: Vec::new(),
                branches: Vec::new(),
                limitations: Vec::new(),
                scope: None,
            },
            point_ids: BTreeSet::new(),
            decision_ids: BTreeSet::new(),
            branch_ids: BTreeSet::new(),
            limitation_ids: BTreeSet::new(),
            error: None,
        }
    }

    fn location_source(&mut self, range: TextRange) -> Option<(usize, usize, String)> {
        let result = self.locations.range(range).map(|(start, _)| {
            let (line, column) = self.locations.line_column(start);
            (line, column, self.locations.text(range))
        });
        match result {
            Ok((line, column, Ok(source))) => Some((line, column, source)),
            Ok((_, _, Err(error))) | Err(error) => {
                self.error.get_or_insert(error);
                None
            }
        }
    }

    fn point(&mut self, range: TextRange, kind: PointKind, label: Option<String>) {
        let kind_name = match kind {
            PointKind::Statement => "statement",
            PointKind::Function => "function",
        };
        let id = stable_id(self.file, kind_name, range, label.as_deref().unwrap_or(""));
        if !self.point_ids.insert(id.clone()) {
            return;
        }
        let Some((line, column, source)) = self.location_source(range) else {
            return;
        };
        self.manifest.points.push(PointMeta {
            id,
            kind,
            file: self.file.into(),
            line,
            column,
            source,
            label,
        });
    }

    fn atomic_condition_ranges(expression: &ast::Expr, ranges: &mut Vec<TextRange>) {
        match expression {
            ast::Expr::ParenExpr(paren) => {
                if let Some(inner) = paren.expr() {
                    Self::atomic_condition_ranges(&inner, ranges);
                } else {
                    ranges.push(expression.syntax().text_range());
                }
            }
            ast::Expr::BinExpr(binary)
                if matches!(
                    binary.op_kind(),
                    Some(BinaryOp::LogicOp(LogicOp::And | LogicOp::Or))
                ) =>
            {
                if let Some(left) = binary.lhs() {
                    Self::atomic_condition_ranges(&left, ranges);
                }
                if let Some(right) = binary.rhs() {
                    Self::atomic_condition_ranges(&right, ranges);
                }
            }
            _ => ranges.push(expression.syntax().text_range()),
        }
    }

    fn decision(&mut self, test: &ast::Expr, kind: &str) {
        let range = test.syntax().text_range();
        let id = stable_id(self.file, "decision", range, kind);
        if !self.decision_ids.insert(id.clone()) {
            return;
        }
        let Some((line, column, source)) = self.location_source(range) else {
            return;
        };
        let mut condition_ranges = Vec::new();
        Self::atomic_condition_ranges(test, &mut condition_ranges);
        let mut conditions = Vec::with_capacity(condition_ranges.len());
        for condition in condition_ranges {
            match self.locations.text(condition) {
                Ok(source) => conditions.push(source),
                Err(error) => {
                    self.error.get_or_insert(error);
                    return;
                }
            }
        }
        self.manifest.decisions.push(DecisionMeta {
            id: id.clone(),
            file: self.file.into(),
            line,
            column,
            source: source.clone(),
            conditions,
            kind: kind.into(),
        });
        self.branch_with_id(
            format!("{id}:outcome"),
            range,
            kind,
            source,
            [("true", "true"), ("false", "false")],
        );
    }

    fn branch<const N: usize>(
        &mut self,
        range: TextRange,
        kind: &str,
        alternatives: [(&str, &str); N],
    ) {
        let id = stable_id(self.file, "branch", range, kind);
        let Some((_, _, source)) = self.location_source(range) else {
            return;
        };
        self.branch_with_id(id, range, kind, source, alternatives);
    }

    fn branch_with_id<const N: usize>(
        &mut self,
        id: String,
        range: TextRange,
        kind: &str,
        source: String,
        alternatives: [(&str, &str); N],
    ) {
        if !self.branch_ids.insert(id.clone()) {
            return;
        }
        let Some((line, column, _)) = self.location_source(range) else {
            return;
        };
        self.manifest.branches.push(BranchMeta {
            id: id.clone(),
            kind: kind.into(),
            file: self.file.into(),
            line,
            column,
            source,
            alternatives: alternatives
                .into_iter()
                .map(|(suffix, label)| BranchAlternativeMeta {
                    id: format!("{id}:{suffix}"),
                    label: label.into(),
                })
                .collect(),
        });
    }

    fn limitation(&mut self, id: &'static str, reason: &'static str) {
        if !self.limitation_ids.insert(id) {
            return;
        }
        self.manifest.limitations.push(json!({
            "id": id,
            "kind": "rust-frontend-readiness",
            "file": self.file,
            "line": 1,
            "column": 0,
            "source": "",
            "reason": reason
        }));
    }

    fn collect(mut self, file: &SourceFile) -> Result<CoverageManifest, RustInstrumenterError> {
        let root = file.syntax();

        for list in root.descendants().filter_map(ast::StmtList::cast) {
            for statement in list.statements() {
                match statement {
                    ast::Stmt::ExprStmt(statement) => {
                        self.point(statement.syntax().text_range(), PointKind::Statement, None);
                    }
                    ast::Stmt::LetStmt(statement) => {
                        self.point(statement.syntax().text_range(), PointKind::Statement, None);
                    }
                    ast::Stmt::Item(_) => {}
                }
            }
            if let Some(tail) = list.tail_expr() {
                self.point(tail.syntax().text_range(), PointKind::Statement, None);
            }
        }

        for function in root.descendants().filter_map(ast::Fn::cast) {
            if function.body().is_none() {
                continue;
            }
            if function.const_token().is_some() {
                self.limitation(
                    "rust-const-context-not-instrumented",
                    "Runtime probes cannot execute in const fn or compile-time evaluation",
                );
                continue;
            }
            let label = function.name().map(|name| name.text().to_string());
            self.point(function.syntax().text_range(), PointKind::Function, label);
        }

        for closure in root.descendants().filter_map(ast::ClosureExpr::cast) {
            self.point(
                closure.syntax().text_range(),
                PointKind::Function,
                Some("<closure>".into()),
            );
        }

        for expression in root.descendants().filter_map(ast::IfExpr::cast) {
            if let Some(condition) = expression.condition() {
                self.decision(&condition, "if");
            }
        }
        for expression in root.descendants().filter_map(ast::WhileExpr::cast) {
            if let Some(condition) = expression.condition() {
                self.decision(&condition, "while");
            }
            self.branch(
                expression.syntax().text_range(),
                "while-loop",
                [("zero", "zero iterations"), ("entered", "entered")],
            );
        }
        for guard in root.descendants().filter_map(ast::MatchGuard::cast) {
            if let Some(condition) = guard.condition() {
                self.decision(&condition, "match-guard");
            }
        }

        for binary in root.descendants().filter_map(ast::BinExpr::cast) {
            let kind = match binary.op_kind() {
                Some(BinaryOp::LogicOp(LogicOp::And)) => "logical-and",
                Some(BinaryOp::LogicOp(LogicOp::Or)) => "logical-or",
                _ => continue,
            };
            let range = binary.rhs().map_or_else(
                || binary.syntax().text_range(),
                |right| right.syntax().text_range(),
            );
            self.branch(
                range,
                kind,
                [
                    ("short-circuit", "short-circuited"),
                    ("evaluated", "right operand evaluated"),
                ],
            );
        }

        for expression in root.descendants().filter_map(ast::ForExpr::cast) {
            self.branch(
                expression.syntax().text_range(),
                "for-loop",
                [("zero", "zero iterations"), ("entered", "entered")],
            );
        }
        for expression in root.descendants().filter_map(ast::MatchExpr::cast) {
            let Some(list) = expression.match_arm_list() else {
                continue;
            };
            let arms = list.arms().collect::<Vec<_>>();
            let last = arms.len().saturating_sub(1);
            for (index, arm) in arms.iter().enumerate() {
                let range = arm.syntax().text_range();
                if index == last {
                    // A match is exhaustive, so once every earlier arm has
                    // been passed over the last one is selected: it can be
                    // reached but never skipped.
                    self.branch(range, "match-arm", [("selected", "selected")]);
                } else {
                    self.branch(
                        range,
                        "match-arm",
                        [("missed", "not selected"), ("selected", "selected")],
                    );
                }
            }
        }
        for expression in root.descendants().filter_map(ast::TryExpr::cast) {
            self.branch(
                expression.syntax().text_range(),
                "try-operator",
                [("continued", "continued"), ("returned", "early return")],
            );
        }

        if root.descendants().any(|node| {
            ast::MacroCall::can_cast(node.kind()) || ast::MacroExpr::can_cast(node.kind())
        }) {
            self.limitation(
                "rust-macro-expansion-not-instrumented",
                "Declarative and procedural macro expansions are not yet part of the owned source denominator",
            );
        }

        // An obligation the probes cannot reach stays in the denominator, but the
        // gap has to be declared rather than left to read as merely uncovered.
        // Only a context that actually holds an obligation counts:
        // `const MAX: usize = 10;` costs nothing and must not raise a limitation.
        let bears_obligation = |node: &ra_ap_syntax::SyntaxNode| {
            ast::StmtList::cast(node.clone()).is_some_and(|list| {
                list.statements().next().is_some() || list.tail_expr().is_some()
            }) || ast::IfExpr::can_cast(node.kind())
                || ast::WhileExpr::can_cast(node.kind())
                || ast::MatchGuard::can_cast(node.kind())
                || ast::ForExpr::can_cast(node.kind())
                || ast::MatchArm::can_cast(node.kind())
                || ast::TryExpr::can_cast(node.kind())
                || ast::ClosureExpr::can_cast(node.kind())
                || ast::BinExpr::cast(node.clone()).is_some_and(|binary| {
                    matches!(
                        binary.op_kind(),
                        Some(BinaryOp::LogicOp(LogicOp::And | LogicOp::Or))
                    )
                })
        };
        if root
            .descendants()
            .any(|node| bears_obligation(&node) && in_const_context(&node))
        {
            self.limitation(
                "rust-const-context-not-instrumented",
                "Runtime probes cannot execute in const fn or compile-time evaluation",
            );
        }
        if root
            .descendants()
            .any(|node| bears_obligation(&node) && in_global_allocator(&node))
        {
            self.limitation(
                "rust-global-allocator-not-instrumented",
                "Probing a GlobalAlloc implementation recurses into itself, because the runtime allocates",
            );
        }

        if let Some(error) = self.error {
            return Err(error);
        }
        self.manifest
            .decisions
            .sort_by(|left, right| left.id.cmp(&right.id));
        self.manifest
            .points
            .sort_by(|left, right| left.id.cmp(&right.id));
        self.manifest
            .branches
            .sort_by(|left, right| left.id.cmp(&right.id));
        self.manifest.limitations.sort_by(|left, right| {
            left.get("id")
                .and_then(|value| value.as_str())
                .cmp(&right.get("id").and_then(|value| value.as_str()))
        });
        Ok(self.manifest)
    }
}

pub fn build_rust_manifest(
    file: &str,
    source: &str,
) -> Result<CoverageManifest, RustInstrumenterError> {
    if source.len() > u32::MAX as usize {
        return Err(RustInstrumenterError::SourceTooLarge);
    }
    let parsed = SourceFile::parse(source, Edition::CURRENT);
    let errors = parsed
        .errors()
        .into_iter()
        .map(|error| error.to_string())
        .collect::<Vec<_>>();
    if !errors.is_empty() {
        return Err(RustInstrumenterError::Parse(errors));
    }
    RustObligationCollector::new(file, source).collect(&parsed.tree())
}

fn block_entry_offset(block: &ast::BlockExpr) -> Option<usize> {
    let list = block.stmt_list()?;
    list.attrs()
        .last()
        .map(|attribute| usize::from(attribute.syntax().text_range().end()))
        .or_else(|| {
            list.l_curly_token()
                .map(|token| usize::from(token.text_range().end()))
        })
}

/// Where an item for `node` can go: the entry of the nearest enclosing block.
/// An item declared there is visible to the whole block, so nothing about the
/// expression itself -- its value, its temporaries -- changes.
fn enclosing_block_entry(node: &ra_ap_syntax::SyntaxNode) -> Option<usize> {
    node.ancestors()
        .skip(1)
        .find_map(ast::BlockExpr::cast)
        .and_then(|block| block_entry_offset(&block))
}

/// A block written as a bare `{ ... }`: a probe placed just inside its brace
/// runs when the block is entered. Labeled, `unsafe`, `async` and `const`
/// blocks are wrapped instead, so an `async` body does not defer the probe.
fn plain_block(block: &ast::BlockExpr) -> bool {
    block
        .syntax()
        .first_token()
        .is_some_and(|token| token.kind() == SyntaxKind::L_CURLY)
}

fn instrument_decision(
    insertions: &mut Vec<Insertion>,
    runtime_path: &str,
    file: &str,
    condition: &ast::Expr,
    kind: &str,
    frame_name: &str,
) -> bool {
    if cannot_carry_probe(condition.syntax())
        || condition
            .syntax()
            .descendants()
            .any(|node| ast::LetExpr::can_cast(node.kind()))
    {
        return false;
    }
    let range = condition.syntax().text_range();
    let id = stable_id(file, "decision", range, kind);
    let mut condition_ranges = Vec::new();
    RustObligationCollector::atomic_condition_ranges(condition, &mut condition_ranges);
    push_wrapper(
        insertions,
        range,
        range,
        0,
        format!(
            "({{ let mut {frame_name} = {runtime_path}::DecisionFrame::new({id:?}, {}); {runtime_path}::decision((",
            condition_ranges.len()
        ),
        format!("), &mut {frame_name}) }})"),
    );
    for (index, atomic_range) in condition_ranges.into_iter().enumerate() {
        push_wrapper(
            insertions,
            atomic_range,
            range,
            1,
            format!("{runtime_path}::condition(("),
            format!("), &mut {frame_name}, {index})"),
        );
    }
    true
}

/// Produce a private Rust candidate using only Supercov-owned probe calls.
///
/// The caller supplies a collision-free generated crate-local runtime path.
/// This stage instruments the surfaces whose source transform already has
/// semantic tests. Remaining branch surfaces stay in the denominator and are
/// paired with a blocking manifest limitation.
pub fn instrument_rust_source(
    file: &str,
    source: &str,
    runtime_path: &str,
) -> Result<RustInstrumentedSource, RustInstrumenterError> {
    if !valid_runtime_path(runtime_path) {
        return Err(RustInstrumenterError::InvalidRuntimePath);
    }
    let mut manifest = build_rust_manifest(file, source)?;
    let parsed = SourceFile::parse(source, Edition::CURRENT);
    let tree = parsed.tree();
    let root = tree.syntax();
    let mut insertions = Vec::new();
    let mut identifiers = root
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .filter(|token| token.kind() == SyntaxKind::IDENT)
        .map(|token| token.text().to_string())
        .collect::<BTreeSet<_>>();

    let mut skipped_attributed_statement = false;
    // A probe must never be PREPENDED to a statement that carries outer
    // attributes. `#[cfg]` selects among adjacent statements, and a bare
    // `hit(...)` inserted between them survives the strip and changes which
    // expression is the block's tail: memchr's `is_available` returns bool
    // from one of two cfg-gated blocks, and the stray probe turned the kept
    // block into a statement and the probe itself into a `()` tail -- 32
    // E0308s across the crate. An attributed BLOCK takes the probe inside its
    // braces, where the same cfg governs both; any other attributed statement
    // is skipped and declared, mirroring the let-chain limitation.
    let attributed_probe = |insertions: &mut Vec<Insertion>,
                            skipped: &mut bool,
                            expression: Option<ast::Expr>,
                            has_attrs: bool,
                            range: TextRange,
                            id: String| {
        if !has_attrs {
            push_direct(
                insertions,
                usize::from(range.start()),
                format!("{runtime_path}::hit({id:?});"),
            );
            return;
        }
        if let Some(ast::Expr::BlockExpr(block)) = expression
            && let Some(offset) = block_entry_offset(&block)
        {
            push_direct(
                insertions,
                offset,
                format!("\n{runtime_path}::hit({id:?});"),
            );
            return;
        }
        *skipped = true;
    };
    for list in root.descendants().filter_map(ast::StmtList::cast) {
        for statement in list.statements() {
            let (range, expression, has_attrs) = match statement {
                ast::Stmt::ExprStmt(statement) if !cannot_carry_probe(statement.syntax()) => {
                    let expression = statement.expr();
                    // Outer attributes on an expression statement attach to
                    // the inner expression in this grammar.
                    let has_attrs = expression
                        .as_ref()
                        .is_some_and(|expression| expression.attrs().next().is_some());
                    (statement.syntax().text_range(), expression, has_attrs)
                }
                ast::Stmt::LetStmt(statement) if !cannot_carry_probe(statement.syntax()) => {
                    let has_attrs = statement.attrs().next().is_some();
                    (statement.syntax().text_range(), None, has_attrs)
                }
                _ => continue,
            };
            let id = stable_id(file, "statement", range, "");
            attributed_probe(
                &mut insertions,
                &mut skipped_attributed_statement,
                expression,
                has_attrs,
                range,
                id,
            );
        }
        if let Some(tail) = list
            .tail_expr()
            .filter(|tail| !cannot_carry_probe(tail.syntax()))
        {
            let range = tail.syntax().text_range();
            let id = stable_id(file, "statement", range, "");
            let has_attrs = tail.attrs().next().is_some();
            attributed_probe(
                &mut insertions,
                &mut skipped_attributed_statement,
                Some(tail),
                has_attrs,
                range,
                id,
            );
        }
    }

    for function in root.descendants().filter_map(ast::Fn::cast) {
        // `cannot_carry_probe` covers `const fn` itself, since a node's own
        // ancestors include the node.
        if cannot_carry_probe(function.syntax()) {
            continue;
        }
        let Some(body) = function.body() else {
            continue;
        };
        let label = function.name().map(|name| name.text().to_string());
        let id = stable_id(
            file,
            "function",
            function.syntax().text_range(),
            label.as_deref().unwrap_or(""),
        );
        if let Some(offset) = block_entry_offset(&body) {
            push_direct(
                &mut insertions,
                offset,
                format!("\n{runtime_path}::hit({id:?});"),
            );
        }
    }

    for closure in root.descendants().filter_map(ast::ClosureExpr::cast) {
        let Some(body) = closure.body() else {
            continue;
        };
        if cannot_carry_probe(body.syntax()) {
            continue;
        }
        let id = stable_id(file, "function", closure.syntax().text_range(), "<closure>");
        if let ast::Expr::BlockExpr(block) = &body {
            if let Some(offset) = block_entry_offset(block) {
                push_direct(
                    &mut insertions,
                    offset,
                    format!("\n{runtime_path}::hit({id:?});"),
                );
            }
        } else {
            let range = body.syntax().text_range();
            push_wrapper(
                &mut insertions,
                range,
                closure.syntax().text_range(),
                0,
                format!("{{ {runtime_path}::hit({id:?}); ("),
                ") }".into(),
            );
        }
    }

    // Match arms. One `const` per match, at the entry of the enclosing block,
    // names every arm's `not selected` and `selected` IDs in source order;
    // each arm then records itself as selected and every arm before it as
    // passed over, since a match tries its arms in order and stops at the
    // first that fits. The runtime dedupes by ID, so a hot match costs one
    // record per alternative.
    for expression in root.descendants().filter_map(ast::MatchExpr::cast) {
        if cannot_carry_probe(expression.syntax()) {
            continue;
        }
        let Some(list) = expression.match_arm_list() else {
            continue;
        };
        let arms = list.arms().collect::<Vec<_>>();
        if arms.is_empty() {
            continue;
        }
        let Some(table_offset) = enclosing_block_entry(expression.syntax()) else {
            continue;
        };
        let table = allocate_table_name(file, &expression, &mut identifiers);
        let entries = arms
            .iter()
            .map(|arm| {
                let id = stable_id(file, "branch", arm.syntax().text_range(), "match-arm");
                format!(
                    "{:?}, {:?}",
                    format!("{id}:missed"),
                    format!("{id}:selected")
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        push_direct(
            &mut insertions,
            table_offset,
            format!("\nconst {table}: &[&str] = &[{entries}];"),
        );
        for (index, arm) in arms.iter().enumerate() {
            let Some(body) = arm.expr() else {
                continue;
            };
            let call = format!("{runtime_path}::arms({table}, {index});");
            match &body {
                ast::Expr::BlockExpr(block) if plain_block(block) => {
                    if let Some(offset) = block_entry_offset(block) {
                        push_direct(&mut insertions, offset, format!("\n{call}"));
                    }
                }
                _ => push_wrapper(
                    &mut insertions,
                    body.syntax().text_range(),
                    arm.syntax().text_range(),
                    0,
                    format!("{{ {call} ("),
                    ") }".into(),
                ),
            }
        }
    }

    let mut skipped_let_condition = false;
    for expression in root.descendants().filter_map(ast::IfExpr::cast) {
        if let Some(condition) = expression.condition() {
            let frame_name = allocate_frame_name(file, &condition, "if", &mut identifiers);
            if !instrument_decision(
                &mut insertions,
                runtime_path,
                file,
                &condition,
                "if",
                &frame_name,
            ) && condition
                .syntax()
                .descendants()
                .any(|node| ast::LetExpr::can_cast(node.kind()))
            {
                skipped_let_condition = true;
            }
        }
    }
    for expression in root.descendants().filter_map(ast::WhileExpr::cast) {
        if let Some(condition) = expression.condition() {
            let frame_name = allocate_frame_name(file, &condition, "while", &mut identifiers);
            if !instrument_decision(
                &mut insertions,
                runtime_path,
                file,
                &condition,
                "while",
                &frame_name,
            ) && condition
                .syntax()
                .descendants()
                .any(|node| ast::LetExpr::can_cast(node.kind()))
            {
                skipped_let_condition = true;
            }
        }
    }
    for guard in root.descendants().filter_map(ast::MatchGuard::cast) {
        if let Some(condition) = guard.condition() {
            let frame_name = allocate_frame_name(file, &condition, "match-guard", &mut identifiers);
            instrument_decision(
                &mut insertions,
                runtime_path,
                file,
                &condition,
                "match-guard",
                &frame_name,
            );
        }
    }

    if skipped_attributed_statement {
        add_manifest_limitation(
            &mut manifest,
            file,
            "rust-attributed-statement-probes-not-injected",
            "Statements carrying outer attributes cannot take an adjacent probe without changing cfg selection",
        );
    }
    if skipped_let_condition {
        add_manifest_limitation(
            &mut manifest,
            file,
            "rust-let-chain-probes-not-injected",
            "Pattern conditions and let chains remain in the denominator but do not yet have semantics-proven owned condition probes",
        );
    }
    if manifest.branches.iter().any(|branch| {
        !branch.id.ends_with(":outcome")
            && matches!(
                branch.kind.as_str(),
                "logical-and" | "logical-or" | "for-loop" | "while-loop" | "try-operator"
            )
    }) {
        add_manifest_limitation(
            &mut manifest,
            file,
            "rust-structural-branch-probes-not-yet-injected",
            "Logical selection, loop and try-operator obligations remain visible but their owned observations are not yet injected",
        );
    }
    manifest.limitations.sort_by(|left, right| {
        left.get("id")
            .and_then(|value| value.as_str())
            .cmp(&right.get("id").and_then(|value| value.as_str()))
    });

    let code = apply_insertions(source, insertions)?;
    let transformed = SourceFile::parse(&code, Edition::CURRENT);
    let errors = transformed
        .errors()
        .into_iter()
        .map(|error| error.to_string())
        .collect::<Vec<_>>();
    if !errors.is_empty() {
        return Err(RustInstrumenterError::Parse(errors));
    }
    Ok(RustInstrumentedSource { code, manifest })
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        process::Command,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    const NOOP_RUNTIME: &str = r#"
#[doc(hidden)]
mod __supercov_runtime_v1 {
    pub struct DecisionFrame;
    impl DecisionFrame {
        pub fn new(_: &'static str, _: usize) -> Self { Self }
    }
    pub fn hit(_: &'static str) {}
    pub fn arms(_: &[&'static str], _: usize) {}
    pub fn condition(value: bool, _: &mut DecisionFrame, _: usize) -> bool { value }
    pub fn decision(value: bool, _: &mut DecisionFrame) -> bool { value }
}
"#;

    fn compile_and_run(source: &str, name: &str) -> std::process::Output {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "supercov-rust-transform-{}-{nonce}-{name}",
            std::process::id()
        ));
        fs::create_dir(&directory).unwrap();
        let input = directory.join("main.rs");
        let binary = directory.join("program");
        fs::write(&input, source).unwrap();
        let compile = Command::new("rustc")
            .arg("--edition=2024")
            .arg(&input)
            .arg("-o")
            .arg(&binary)
            .output()
            .unwrap();
        assert!(
            compile.status.success(),
            "rustc failed:\n{}\nsource:\n{source}",
            String::from_utf8_lossy(&compile.stderr)
        );
        let output = Command::new(&binary).output().unwrap();
        fs::remove_dir_all(directory).unwrap();
        output
    }

    #[test]
    fn discovers_rust_obligations_with_exact_ranges_and_stable_ids() {
        let source = r#"fn classify<T>(values: &[T], first: bool, second: bool, third: bool) -> Option<&T> {
    let picked = if first && (second || third) {
        values.first()?
    } else {
        None
    };
    for value in values {
        if first || second {
            return Some(value);
        }
    }
    match picked {
        Some(value) if second && third => Some(value),
        _ => None,
    }
}

fn closure(value: i32) -> bool {
    (|candidate| candidate > 0)(value)
}
"#;
        let first = build_rust_manifest("src/lib.rs", source).unwrap();
        let second = build_rust_manifest("src/lib.rs", source).unwrap();
        assert_eq!(first, second);
        assert!(first.points.iter().any(|point| {
            point.kind == PointKind::Function && point.label.as_deref() == Some("classify")
        }));
        assert!(first.points.iter().any(|point| {
            point.kind == PointKind::Function && point.label.as_deref() == Some("<closure>")
        }));
        let first_if = first
            .decisions
            .iter()
            .find(|decision| decision.line == 2)
            .unwrap();
        assert_eq!(first_if.conditions, ["first", "second", "third"]);
        assert_eq!(first_if.column, 20);
        assert!(
            first
                .branches
                .iter()
                .any(|branch| branch.kind == "for-loop")
        );
        let mut arms = first
            .branches
            .iter()
            .filter(|branch| branch.kind == "match-arm")
            .collect::<Vec<_>>();
        arms.sort_by_key(|branch| branch.line);
        assert_eq!(arms.len(), 2);
        assert_eq!(
            arms[0]
                .alternatives
                .iter()
                .map(|alternative| alternative.label.as_str())
                .collect::<Vec<_>>(),
            ["not selected", "selected"]
        );
        // The last arm of an exhaustive match is reached or not; it is never
        // considered and passed over.
        assert_eq!(
            arms[1]
                .alternatives
                .iter()
                .map(|alternative| alternative.label.as_str())
                .collect::<Vec<_>>(),
            ["selected"]
        );
        assert!(
            first
                .branches
                .iter()
                .any(|branch| branch.kind == "try-operator")
        );
        assert!(first.decisions.iter().all(|decision| {
            decision.id.starts_with("rs:decision:") && decision.conditions.len() >= 2
        }));
        assert!(first.limitations.is_empty());
    }

    #[test]
    fn declares_macro_and_const_boundaries_instead_of_hiding_them() {
        let source = r#"const fn doubled(value: usize) -> usize { value * 2 }

fn checked(value: bool) -> bool {
    assert!(value);
    const { doubled(2) == 4 }
}
"#;
        let manifest = build_rust_manifest("src/lib.rs", source).unwrap();
        let ids = manifest
            .limitations
            .iter()
            .filter_map(|limitation| limitation.get("id")?.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            ids,
            BTreeSet::from([
                "rust-const-context-not-instrumented",
                "rust-macro-expansion-not-instrumented"
            ])
        );
        assert!(!manifest.points.iter().any(|point| {
            point.kind == PointKind::Function && point.label.as_deref() == Some("doubled")
        }));
    }

    #[test]
    fn transforms_points_and_nested_decisions_without_changing_behavior() {
        let source = r#"use std::sync::atomic::{AtomicUsize, Ordering};

static CALLS: AtomicUsize = AtomicUsize::new(0);

fn observed(name: &str, value: bool) -> bool {
    let order = CALLS.fetch_add(1, Ordering::SeqCst);
    println!("{order}:{name}:{value}");
    value
}

fn classify(first: bool, second: bool, third: bool) -> i32 {
    if observed("a", first) && (observed("b", second) || observed("c", third)) {
        7
    } else {
        3
    }
}

fn main() {
    let closure = |value: i32| value + 1;
    println!("result={}", closure(classify(true, false, true)));
}
"#;
        let transformed =
            instrument_rust_source("src/main.rs", source, "crate::__supercov_runtime_v1").unwrap();
        assert!(transformed.code.contains("::condition("));
        assert!(transformed.code.contains("::decision("));
        assert!(transformed.code.contains("::hit("));
        let original = compile_and_run(source, "original");
        let instrumented = compile_and_run(
            &format!("{}\n{NOOP_RUNTIME}", transformed.code),
            "instrumented",
        );
        assert_eq!(instrumented.status, original.status);
        assert_eq!(instrumented.stdout, original.stdout);
        assert_eq!(instrumented.stderr, original.stderr);
    }

    #[test]
    fn skips_let_chains_and_const_contexts_with_explicit_limitations() {
        let source = r#"const fn enabled(value: bool) -> bool {
    if value { true } else { false }
}

fn classify(value: Option<bool>, fallback: bool) -> bool {
    if let Some(inner) = value && inner && fallback { true } else { false }
}
"#;
        let transformed =
            instrument_rust_source("src/lib.rs", source, "crate::__supercov_runtime_v1").unwrap();
        let ids = transformed
            .manifest
            .limitations
            .iter()
            .filter_map(|limitation| limitation.get("id")?.as_str())
            .collect::<BTreeSet<_>>();
        assert!(ids.contains("rust-const-context-not-instrumented"));
        assert!(ids.contains("rust-let-chain-probes-not-injected"));
        assert!(!transformed.code.contains("condition("));
    }

    #[test]
    fn instrumented_const_and_static_initialisers_still_compile() {
        // Every one of these positions is const-evaluated, so none of them can
        // hold a call to the runtime -- `condition`, `decision` and `hit` are
        // not `const fn`. Found on bytes-1.12.1, whose test target has
        // `const ITERS: usize = if cfg!(miri) { 100 } else { 1_000 };` and
        // failed to build with E0015.
        let source = r#"const DIRECT: usize = if cfg!(unix) { 100 } else { 1_000 };
static WIDTH: usize = if cfg!(unix) { 2 } else { 4 };

enum Mode {
    Narrow = if cfg!(unix) { 1 } else { 2 },
}

struct Buffer([u8; if cfg!(unix) { 4 } else { 8 }]);

impl Buffer {
    const SPAN: usize = if cfg!(unix) { 5 } else { 9 };
}

fn scaled(flag: bool) -> usize {
    const LOCAL: usize = if cfg!(unix) { 3 } else { 6 };
    if flag { LOCAL + Buffer::SPAN } else { DIRECT + WIDTH }
}

fn main() {
    let buffer = Buffer([0; if cfg!(unix) { 4 } else { 8 }]);
    println!(
        "{} {} {} {}",
        scaled(true),
        scaled(false),
        Mode::Narrow as usize,
        buffer.0.len()
    );
}
"#;
        let transformed =
            instrument_rust_source("src/main.rs", source, "crate::__supercov_runtime_v1").unwrap();
        // The runtime `if` in `scaled` is still instrumented -- declining a
        // const initialiser must not decline the whole file.
        assert!(transformed.code.contains("::decision("));
        let ids = transformed
            .manifest
            .limitations
            .iter()
            .filter_map(|limitation| limitation.get("id")?.as_str())
            .collect::<BTreeSet<_>>();
        assert!(ids.contains("rust-const-context-not-instrumented"));

        let original = compile_and_run(source, "const-original");
        let instrumented = compile_and_run(
            &format!("{}\n{NOOP_RUNTIME}", transformed.code),
            "const-instrumented",
        );
        assert_eq!(instrumented.status, original.status);
        assert_eq!(instrumented.stdout, original.stdout);
        assert_eq!(instrumented.stderr, original.stderr);
    }

    #[test]
    fn a_probed_global_allocator_would_recurse_into_itself() {
        // The runtime allocates, so a probe inside `alloc` re-enters `alloc` and
        // recurses until the stack is gone. bytes-1.12.1's
        // tests/test_bytes_odd_alloc.rs installs one of these, and the
        // instrumented binary died with SIGSEGV before libtest could list a
        // single test, while the uninstrumented binary listed them fine.
        let source = r#"use std::alloc::{GlobalAlloc, Layout, System};

struct Odd;

unsafe impl GlobalAlloc for Odd {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if layout.align() == 1 && layout.size() > 0 {
            System.alloc(layout)
        } else {
            System.alloc(layout)
        }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        System.dealloc(pointer, layout);
    }
}

#[global_allocator]
static ODD: Odd = Odd;

fn classify(flag: bool) -> usize {
    if flag { 1 } else { 2 }
}

fn main() {
    let held = std::vec![7u8; 32];
    println!("{} {}", classify(!held.is_empty()), held.len());
}
"#;
        let transformed =
            instrument_rust_source("src/main.rs", source, "crate::__supercov_runtime_v1").unwrap();
        // Nothing inside the allocator may carry a probe...
        let allocator = transformed
            .code
            .split("unsafe impl GlobalAlloc for Odd")
            .nth(1)
            .and_then(|rest| rest.split("#[global_allocator]").next())
            .expect("the instrumented source still contains the allocator impl");
        assert!(
            !allocator.contains("__supercov_runtime_v1"),
            "probe injected into a GlobalAlloc impl:\n{allocator}"
        );
        // ...while `classify`, right next to it, is still measured.
        assert!(transformed.code.contains("::decision("));
        let ids = transformed
            .manifest
            .limitations
            .iter()
            .filter_map(|limitation| limitation.get("id")?.as_str())
            .collect::<BTreeSet<_>>();
        assert!(ids.contains("rust-global-allocator-not-instrumented"));

        let original = compile_and_run(source, "alloc-original");
        let instrumented = compile_and_run(
            &format!("{}\n{NOOP_RUNTIME}", transformed.code),
            "alloc-instrumented",
        );
        assert_eq!(instrumented.status, original.status);
        assert_eq!(instrumented.stdout, original.stdout);
        assert_eq!(instrumented.stderr, original.stderr);
    }

    #[test]
    fn match_arms_record_selection_without_changing_behavior() {
        let source = r#"#[derive(Debug)]
enum Shape { Dot, Line(i32), Box { w: i32, h: i32 } }

fn area(shape: &Shape) -> i32 {
    match shape {
        Shape::Dot => 0,
        Shape::Line(length) if *length < 0 => -length,
        Shape::Line(length) => *length,
        Shape::Box { w, h } => {
            let area = w * h;
            area
        }
    }
}

fn describe(value: i32) -> &'static str {
    let inner = |v: i32| match v { 0 => "none", 1 => "one", _ => "many" };
    match value {
        0 => inner(value),
        n if n < 0 => unsafe { std::hint::unreachable_unchecked() },
        n => match n % 2 {
            0 => "even",
            _ => inner(n),
        },
    }
}

fn main() {
    for shape in [Shape::Dot, Shape::Line(-3), Shape::Line(4), Shape::Box { w: 2, h: 5 }] {
        println!("{shape:?}={}", area(&shape));
    }
    for value in [0, 1, 3, 8] {
        println!("{value}:{}", describe(value));
    }
}
"#;
        let transformed =
            instrument_rust_source("src/main.rs", source, "crate::__supercov_runtime_v1").unwrap();
        assert!(transformed.code.contains("::arms(__SUPERCOV_ARMS_"));
        assert_eq!(
            transformed.code.matches("const __SUPERCOV_ARMS_").count(),
            4
        );
        let arms = transformed
            .manifest
            .branches
            .iter()
            .filter(|branch| branch.kind == "match-arm")
            .count();
        assert_eq!(arms, 4 + 3 + 3 + 2);
        // Every arm's alternatives appear in a table, and the source keeps
        // its meaning.
        for branch in transformed
            .manifest
            .branches
            .iter()
            .filter(|branch| branch.kind == "match-arm")
        {
            for alternative in &branch.alternatives {
                assert!(
                    transformed.code.contains(&format!("{:?}", alternative.id)),
                    "{} is not in any table",
                    alternative.id
                );
            }
        }
        let original = compile_and_run(source, "original-arms");
        let instrumented = compile_and_run(
            &format!("{}\n{NOOP_RUNTIME}", transformed.code),
            "instrumented-arms",
        );
        assert_eq!(instrumented.status, original.status);
        assert_eq!(instrumented.stdout, original.stdout);
        assert_eq!(instrumented.stderr, original.stderr);
    }

    #[test]
    fn cfg_gated_sibling_blocks_keep_their_tail_position() {
        // memchr's is_available returns bool from one of two cfg-gated blocks.
        // A probe PREPENDED to the second block sits between the siblings,
        // survives the cfg strip, and becomes the new `()` tail -- 32 E0308s
        // across the crate. Attributed blocks take the probe inside their
        // braces instead, where the same cfg governs both.
        let source = r#"pub fn is_available() -> bool {
    #[cfg(target_endian = "little")]
    {
        true
    }
    #[cfg(not(target_endian = "little"))]
    {
        false
    }
}

fn main() {
    println!("{}", is_available());
}
"#;
        let transformed =
            instrument_rust_source("src/main.rs", source, "crate::__supercov_runtime_v1").unwrap();
        let original = compile_and_run(source, "cfg-original");
        let instrumented = compile_and_run(
            &format!("{}\n{NOOP_RUNTIME}", transformed.code),
            "cfg-instrumented",
        );
        assert_eq!(instrumented.status, original.status);
        assert_eq!(instrumented.stdout, original.stdout);
        // The kept block is still probed -- inside its braces.
        assert!(
            transformed
                .code
                .contains("{\n\ncrate::__supercov_runtime_v1::hit(")
                || transformed
                    .code
                    .contains("{\ncrate::__supercov_runtime_v1::hit(")
        );

        // An attributed non-block statement is skipped and declared.
        let attributed_let = r#"fn main() {
    #[cfg(target_endian = "little")]
    let value = 1;
    #[cfg(not(target_endian = "little"))]
    let value = 2;
    println!("{value}");
}
"#;
        let transformed = instrument_rust_source(
            "src/main.rs",
            attributed_let,
            "crate::__supercov_runtime_v1",
        )
        .unwrap();
        let ids = transformed
            .manifest
            .limitations
            .iter()
            .filter_map(|limitation| limitation.get("id")?.as_str())
            .collect::<BTreeSet<_>>();
        assert!(ids.contains("rust-attributed-statement-probes-not-injected"));
        let original = compile_and_run(attributed_let, "cfg-let-original");
        let instrumented = compile_and_run(
            &format!("{}\n{NOOP_RUNTIME}", transformed.code),
            "cfg-let-instrumented",
        );
        assert_eq!(instrumented.stdout, original.stdout);
    }

    #[test]
    fn rejects_non_crate_local_runtime_paths() {
        assert_eq!(
            instrument_rust_source("src/lib.rs", "fn okay() {}", "supercov::runtime"),
            Err(RustInstrumenterError::InvalidRuntimePath)
        );
    }

    #[test]
    fn rejects_invalid_rust_without_partial_obligations() {
        assert!(matches!(
            build_rust_manifest("src/lib.rs", "fn broken( {\n"),
            Err(RustInstrumenterError::Parse(_))
        ));
    }
}
