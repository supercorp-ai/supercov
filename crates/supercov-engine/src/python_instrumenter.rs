//! Supercov-owned Python parsing and obligation discovery.
//!
//! This module is deliberately private product groundwork. External coverage
//! engines do not participate: Ruff's Rust parser supplies syntax and exact
//! ranges, while Supercov owns the denominator and will own every probe.

use std::collections::BTreeSet;

use ruff_python_ast::{
    Comprehension, Expr, Stmt,
    visitor::{Visitor, walk_comprehension, walk_expr, walk_stmt},
};
use ruff_python_parser::parse_module;
use ruff_text_size::{Ranged, TextRange};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::{
    coverage_analysis::PointKind,
    coverage_report::{
        BranchAlternativeMeta, BranchMeta, CoverageManifest, DecisionMeta, PointMeta,
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PythonInstrumenterError {
    SourceTooLarge,
    Parse(String),
    InvalidRange,
}

impl std::fmt::Display for PythonInstrumenterError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SourceTooLarge => write!(formatter, "Python source exceeds the parser range"),
            Self::Parse(error) => write!(formatter, "Python parse failed: {error}"),
            Self::InvalidRange => write!(formatter, "Python parser returned an invalid range"),
        }
    }
}

impl std::error::Error for PythonInstrumenterError {}

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

    fn range(&self, range: TextRange) -> Result<(usize, usize), PythonInstrumenterError> {
        let start = range.start().to_usize();
        let end = range.end().to_usize();
        if start > end
            || end > self.source.len()
            || !self.source.is_char_boundary(start)
            || !self.source.is_char_boundary(end)
        {
            return Err(PythonInstrumenterError::InvalidRange);
        }
        Ok((start, end))
    }

    fn line_column(&self, offset: usize) -> (usize, usize) {
        let line_index = self.line_starts.partition_point(|start| *start <= offset) - 1;
        (line_index + 1, offset - self.line_starts[line_index])
    }

    fn text(&self, range: TextRange) -> Result<String, PythonInstrumenterError> {
        let (start, end) = self.range(range)?;
        Ok(self.source[start..end].trim().to_owned())
    }
}

fn stable_id(file: &str, kind: &str, range: TextRange, suffix: &str) -> String {
    let mut hash = Sha256::new();
    let start = range.start().to_usize().to_string();
    let end = range.end().to_usize().to_string();
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
    format!("py:{kind}:{encoded}")
}

struct PythonObligationCollector<'a> {
    file: &'a str,
    locations: SourceLocations<'a>,
    manifest: CoverageManifest,
    point_ids: BTreeSet<String>,
    decision_ids: BTreeSet<String>,
    branch_ids: BTreeSet<String>,
    error: Option<PythonInstrumenterError>,
}

impl<'a> PythonObligationCollector<'a> {
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

    fn atomic_conditions(expr: &Expr, conditions: &mut Vec<TextRange>) {
        if let Expr::BoolOp(boolean) = expr {
            for value in &boolean.values {
                Self::atomic_conditions(value, conditions);
            }
        } else {
            conditions.push(expr.range());
        }
    }

    fn decision(&mut self, range: TextRange, test: &Expr, kind: &str) {
        let id = stable_id(self.file, "decision", range, kind);
        if !self.decision_ids.insert(id.clone()) {
            return;
        }
        let Some((line, column, source)) = self.location_source(range) else {
            return;
        };
        let mut condition_ranges = Vec::new();
        Self::atomic_conditions(test, &mut condition_ranges);
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
}

impl<'a> Visitor<'a> for PythonObligationCollector<'_> {
    fn visit_stmt(&mut self, statement: &'a Stmt) {
        self.point(statement.range(), PointKind::Statement, None);
        match statement {
            Stmt::FunctionDef(function) => self.point(
                function.range,
                PointKind::Function,
                Some(function.name.to_string()),
            ),
            Stmt::If(statement) => {
                self.decision(statement.test.range(), &statement.test, "if");
                for clause in &statement.elif_else_clauses {
                    if let Some(test) = &clause.test {
                        self.decision(test.range(), test, "elif");
                    }
                }
            }
            Stmt::While(statement) => {
                self.decision(statement.test.range(), &statement.test, "while");
            }
            Stmt::For(statement) => self.branch(
                statement.range,
                if statement.is_async {
                    "async-for"
                } else {
                    "for"
                },
                [("zero", "zero iterations"), ("entered", "entered")],
            ),
            Stmt::Match(statement) => {
                for (index, case) in statement.cases.iter().enumerate() {
                    let kind = format!("match-case-{index}");
                    self.branch(
                        case.range,
                        &kind,
                        [("missed", "not selected"), ("selected", "selected")],
                    );
                    if let Some(guard) = &case.guard {
                        self.decision(guard.range(), guard, "match-guard");
                    }
                }
                if !statement
                    .cases
                    .iter()
                    .any(|case| case.pattern.is_irrefutable())
                {
                    self.branch(
                        statement.subject.range(),
                        "match-no-case",
                        [
                            ("matched", "some case matched"),
                            ("unmatched", "no case matched"),
                        ],
                    );
                }
            }
            Stmt::Try(statement) => {
                self.branch(
                    statement.range,
                    if statement.is_star { "try-star" } else { "try" },
                    [("success", "try completed"), ("raised", "handler entered")],
                );
                for (index, handler) in statement.handlers.iter().enumerate() {
                    self.branch(
                        handler.range(),
                        &format!("except-{index}"),
                        [("missed", "not selected"), ("selected", "selected")],
                    );
                }
            }
            Stmt::Assert(statement) => {
                self.decision(statement.test.range(), &statement.test, "assert");
            }
            _ => {}
        }
        walk_stmt(self, statement);
    }

    fn visit_expr(&mut self, expression: &'a Expr) {
        match expression {
            Expr::Lambda(lambda) => {
                self.point(lambda.range, PointKind::Function, Some("<lambda>".into()));
            }
            Expr::If(expression) => {
                self.decision(expression.test.range(), &expression.test, "ternary");
            }
            Expr::BoolOp(expression) => {
                for (index, value) in expression.values.iter().enumerate().skip(1) {
                    let op = match expression.op {
                        ruff_python_ast::BoolOp::And => "and",
                        ruff_python_ast::BoolOp::Or => "or",
                    };
                    self.branch(
                        value.range(),
                        &format!("logical-{op}-{index}"),
                        [
                            ("short-circuit", "short-circuited"),
                            ("evaluated", "right operand evaluated"),
                        ],
                    );
                }
            }
            _ => {}
        }
        walk_expr(self, expression);
    }

    fn visit_comprehension(&mut self, comprehension: &'a Comprehension) {
        self.branch(
            comprehension.range,
            if comprehension.is_async {
                "async-comprehension"
            } else {
                "comprehension"
            },
            [("zero", "zero iterations"), ("entered", "entered")],
        );
        for condition in &comprehension.ifs {
            self.decision(condition.range(), condition, "comprehension-if");
        }
        walk_comprehension(self, comprehension);
    }
}

pub fn build_python_manifest(
    file: &str,
    source: &str,
) -> Result<CoverageManifest, PythonInstrumenterError> {
    if source.len() > u32::MAX as usize {
        return Err(PythonInstrumenterError::SourceTooLarge);
    }
    let parsed =
        parse_module(source).map_err(|error| PythonInstrumenterError::Parse(error.to_string()))?;
    let mut collector = PythonObligationCollector::new(file, source);
    collector.visit_body(parsed.suite());
    if let Some(error) = collector.error {
        return Err(error);
    }
    collector.manifest.limitations.push(json!({
        "id": "python-owned-probes-not-yet-injected",
        "kind": "frontend-readiness",
        "file": file,
        "line": 1,
        "column": 0,
        "source": "",
        "reason": "The Rust-owned Python denominator is private until owned probes and semantic-equivalence gates are complete"
    }));
    Ok(collector.manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_current_python_obligations_with_exact_ranges_and_stable_ids() {
        let source = r#"async def classify[T](items, flag=True):
    values = [item async for item in items if item.ready and flag]
    for value in values:
        if value.primary and (value.safe or flag):
            return value if flag else None
        elif value.fallback:
            break
    try:
        match values:
            case [first, *_] if first.ready:
                return first
            case []:
                return None
    except* ValueError:
        return None
    return (lambda value: value or flag)(None)
"#;
        let first = build_python_manifest("src/app.py", source).unwrap();
        let second = build_python_manifest("src/app.py", source).unwrap();
        assert_eq!(first, second);
        assert!(first.points.iter().any(|point| {
            point.kind == PointKind::Function && point.label.as_deref() == Some("classify")
        }));
        assert!(first.points.iter().any(|point| {
            point.kind == PointKind::Function && point.label.as_deref() == Some("<lambda>")
        }));
        let if_decision = first
            .decisions
            .iter()
            .find(|decision| decision.kind == "if")
            .unwrap();
        assert_eq!(
            if_decision.conditions,
            ["value.primary", "value.safe", "flag"]
        );
        assert_eq!(if_decision.line, 4);
        assert_eq!(if_decision.column, 11);
        assert!(
            first
                .decisions
                .iter()
                .any(|decision| decision.kind == "ternary")
        );
        assert!(
            first
                .decisions
                .iter()
                .any(|decision| decision.kind == "match-guard")
        );
        assert!(
            first
                .branches
                .iter()
                .any(|branch| branch.kind == "async-comprehension")
        );
        assert!(
            first
                .branches
                .iter()
                .any(|branch| branch.kind == "try-star")
        );
        assert!(
            first
                .branches
                .iter()
                .any(|branch| branch.kind.starts_with("logical-and"))
        );
        assert!(
            first
                .decisions
                .iter()
                .all(|decision| decision.id.starts_with("py:decision:"))
        );
        assert_eq!(first.limitations.len(), 1);
    }

    #[test]
    fn rejects_invalid_python_without_partial_obligations() {
        assert!(matches!(
            build_python_manifest("src/broken.py", "if :\n    pass\n"),
            Err(PythonInstrumenterError::Parse(_))
        ));
    }
}
