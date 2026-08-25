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

fn in_const_context(node: &ra_ap_syntax::SyntaxNode) -> bool {
    node.ancestors().any(|ancestor| {
        ast::Fn::cast(ancestor.clone()).is_some_and(|function| function.const_token().is_some())
            || ast::BlockExpr::cast(ancestor).is_some_and(|block| block.const_token().is_some())
    })
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
        for arm in root.descendants().filter_map(ast::MatchArm::cast) {
            self.branch(
                arm.syntax().text_range(),
                "match-arm",
                [("missed", "not selected"), ("selected", "selected")],
            );
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
        if root
            .descendants()
            .filter_map(ast::BlockExpr::cast)
            .any(|block| block.const_token().is_some())
        {
            self.limitation(
                "rust-const-context-not-instrumented",
                "Runtime probes cannot execute in const fn or compile-time evaluation",
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

fn instrument_decision(
    insertions: &mut Vec<Insertion>,
    runtime_path: &str,
    file: &str,
    condition: &ast::Expr,
    kind: &str,
    frame_name: &str,
) -> bool {
    if in_const_context(condition.syntax())
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

    for list in root.descendants().filter_map(ast::StmtList::cast) {
        for statement in list.statements() {
            let range = match statement {
                ast::Stmt::ExprStmt(statement) if !in_const_context(statement.syntax()) => {
                    statement.syntax().text_range()
                }
                ast::Stmt::LetStmt(statement) if !in_const_context(statement.syntax()) => {
                    statement.syntax().text_range()
                }
                _ => continue,
            };
            let id = stable_id(file, "statement", range, "");
            push_direct(
                &mut insertions,
                usize::from(range.start()),
                format!("{runtime_path}::hit({id:?});"),
            );
        }
        if let Some(tail) = list
            .tail_expr()
            .filter(|tail| !in_const_context(tail.syntax()))
        {
            let range = tail.syntax().text_range();
            let id = stable_id(file, "statement", range, "");
            push_direct(
                &mut insertions,
                usize::from(range.start()),
                format!("{runtime_path}::hit({id:?});"),
            );
        }
    }

    for function in root.descendants().filter_map(ast::Fn::cast) {
        if function.const_token().is_some() {
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
        if in_const_context(body.syntax()) {
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
                "logical-and"
                    | "logical-or"
                    | "for-loop"
                    | "while-loop"
                    | "match-arm"
                    | "try-operator"
            )
    }) {
        add_manifest_limitation(
            &mut manifest,
            file,
            "rust-structural-branch-probes-not-yet-injected",
            "Logical selection, loop, match-arm and try-operator obligations remain visible but their owned observations are not yet injected",
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
        assert!(
            first
                .branches
                .iter()
                .any(|branch| branch.kind == "match-arm")
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
