//! Supercov-owned Python obligation discovery.
//!
//! Ruff's Rust parser supplies syntax and exact byte ranges. Supercov owns the
//! denominator: every statement, function, decision and branch obligation is
//! decided here, ahead of the run, from source alone. Alongside the shared
//! [`CoverageManifest`] this module emits a *probe plan*: the source spans,
//! `not` polarity, and/or trees and trigger lines the stdlib-only Python
//! runtime needs to map `sys.monitoring` events back onto those obligations.
//! The runtime never decides what counts; it only reports what it observed.

use std::collections::{BTreeMap, BTreeSet};

use ruff_python_ast::{
    BoolOp, CmpOp, Comprehension, Expr, Stmt, UnaryOp,
    helpers::is_docstring_stmt,
    visitor::{Visitor, walk_comprehension, walk_expr, walk_stmt},
};
use ruff_python_parser::parse_module;
use ruff_text_size::{Ranged, TextRange};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    coverage_analysis::PointKind,
    coverage_report::{
        BranchAlternativeMeta, BranchMeta, CoverageManifest, DecisionMeta, PointMeta,
    },
};

pub const PYTHON_PROBE_PLAN_VERSION: u32 = 1;

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

/// A source span in one-based lines and zero-based UTF-8 byte columns, the
/// same units CPython reports through `co_positions()`. Serialized as
/// `[[line, column], [line, column]]` so the runtime unpacks it directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "[[usize; 2]; 2]", into = "[[usize; 2]; 2]")]
pub struct PlanSpan {
    pub start: [usize; 2],
    pub end: [usize; 2],
}

impl From<[[usize; 2]; 2]> for PlanSpan {
    fn from(value: [[usize; 2]; 2]) -> Self {
        Self {
            start: value[0],
            end: value[1],
        }
    }
}

impl From<PlanSpan> for [[usize; 2]; 2] {
    fn from(value: PlanSpan) -> Self {
        [value.start, value.end]
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StatementPlan {
    pub id: String,
    /// Inclusive line range in which a `LINE` event proves the statement ran.
    pub lines: [usize; 2],
    /// Start and end positions of the statement in line and byte column.
    pub start: [usize; 2],
    pub end: [usize; 2],
    /// True when an earlier statement already owns the first line, so only an
    /// `INSTRUCTION` event at the statement's first instruction can prove it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub exact: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FunctionPlan {
    pub id: String,
    /// `co_firstlineno` of the code object: the first decorator line.
    pub line: usize,
    pub name: String,
    /// Whole definition span; disambiguates several lambdas on one line.
    pub span: PlanSpan,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HandlerPlan {
    pub id: String,
    /// From `except` up to the handler body: the type-match instructions live
    /// here. Empty-span bare handlers have no test.
    pub header: PlanSpan,
    pub body_lines: [usize; 2],
    pub bare: bool,
    pub missed: String,
    pub selected: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TryPlan {
    pub id: String,
    pub body: PlanSpan,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orelse: Option<PlanSpan>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finalbody: Option<PlanSpan>,
    pub handlers: Vec<HandlerPlan>,
    pub success: String,
    pub raised: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConditionPlan {
    pub span: PlanSpan,
    /// Number of `not` operators wrapping the tested operand; odd depth inverts
    /// the truthiness the conditional jump observes.
    pub not: usize,
    /// For CPython's specialized `POP_JUMP_IF_(NOT_)NONE` instructions: true
    /// when the un-negated source condition is `value is None`, false for
    /// `value is not None`, absent for every other expression.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub none_when_true: Option<bool>,
}

/// Short-circuit structure of a decision. Leaves are condition indexes. A
/// negated node models `not (a and b)`: CPython still emits one jump per
/// operand, so the operands stay separate conditions and the negation applies
/// to the node's result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ConditionTree {
    Leaf(usize),
    Node {
        op: String,
        items: Vec<ConditionTree>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        negate: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DecisionPlan {
    pub id: String,
    pub kind: String,
    pub span: PlanSpan,
    pub conditions: Vec<ConditionPlan>,
    pub tree: ConditionTree,
    /// Present for comprehension filters: CPython 3.13+ stamps their jumps
    /// with the element expression's position, so the runtime falls back to
    /// offset order inside this span.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comprehension: Option<PlanSpan>,
    pub outcome_true: String,
    pub outcome_false: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LoopPlan {
    pub id: String,
    pub iter: PlanSpan,
    pub zero: String,
    pub entered: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LogicalPlan {
    pub id: String,
    pub boolop: PlanSpan,
    /// Index of the right operand this branch describes (1-based within the
    /// BoolOp's operand list).
    pub operand: usize,
    /// When the BoolOp is part of a decision's and/or tree, the runtime derives
    /// this branch from the decision vector using these leaf index groups:
    /// leaves of the previous operand and leaves of this operand.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_leaves: Option<Vec<usize>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operand_leaves: Option<Vec<usize>>,
    pub short_circuit: String,
    pub evaluated: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MatchCasePlan {
    pub id: String,
    pub span: PlanSpan,
    /// Pattern plus guard: conditional jumps positioned here decide the case.
    pub test: PlanSpan,
    pub irrefutable: bool,
    pub body_lines: [usize; 2],
    pub missed: String,
    pub selected: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MatchNoCasePlan {
    pub id: String,
    pub matched: String,
    pub unmatched: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MatchPlan {
    pub span: PlanSpan,
    pub cases: Vec<MatchCasePlan>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_case: Option<MatchNoCasePlan>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PythonFilePlan {
    pub statements: Vec<StatementPlan>,
    pub functions: Vec<FunctionPlan>,
    pub decisions: Vec<DecisionPlan>,
    pub loops: Vec<LoopPlan>,
    pub logical: Vec<LogicalPlan>,
    pub matches: Vec<MatchPlan>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tries: Vec<TryPlan>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PythonProbePlan {
    pub version: u32,
    pub root: String,
    pub files: BTreeMap<String, PythonFilePlan>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PythonFileObligations {
    pub manifest: CoverageManifest,
    pub plan: PythonFilePlan,
}

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

    fn span(&self, range: TextRange) -> Result<PlanSpan, PythonInstrumenterError> {
        let (start, end) = self.range(range)?;
        let (start_line, start_column) = self.line_column(start);
        let (end_line, end_column) = self.line_column(end);
        Ok(PlanSpan {
            start: [start_line, start_column],
            end: [end_line, end_column],
        })
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

/// Statements whose execution CPython never reports: docstrings are folded
/// into `__doc__` without an instruction and scope declarations compile to
/// nothing.
fn is_unobservable_statement(statement: &Stmt, first_in_body: bool) -> bool {
    matches!(statement, Stmt::Global(_) | Stmt::Nonlocal(_))
        || (first_in_body && is_docstring_stmt(statement))
}

fn first_body_statement(statement: &Stmt) -> Option<&Stmt> {
    match statement {
        Stmt::FunctionDef(inner) => inner.body.first(),
        Stmt::ClassDef(inner) => inner.body.first(),
        Stmt::If(inner) => inner.body.first(),
        Stmt::While(inner) => inner.body.first(),
        Stmt::For(inner) => inner.body.first(),
        Stmt::With(inner) => inner.body.first(),
        Stmt::Try(inner) => inner.body.first(),
        Stmt::Match(inner) => inner.cases.first().and_then(|case| case.body.first()),
        _ => None,
    }
}

/// A BoolOp that belongs to a decision's tree: the decision ID and the leaf
/// indexes contributed by each operand.
type DecisionBoolOp = (String, Vec<Vec<usize>>);

struct PythonObligationCollector<'a> {
    file: &'a str,
    locations: SourceLocations<'a>,
    manifest: CoverageManifest,
    plan: PythonFilePlan,
    point_ids: BTreeSet<String>,
    decision_ids: BTreeSet<String>,
    branch_ids: BTreeSet<String>,
    /// Lines already claimed by a statement trigger; later statements on the
    /// same line are proven by an `INSTRUCTION` event at their first
    /// instruction instead of a `LINE` event.
    claimed_lines: BTreeSet<usize>,
    /// BoolOp ranges that form a decision's and/or tree, with the decision ID
    /// and the leaf indexes of each operand.
    decision_boolops: BTreeMap<(usize, usize), DecisionBoolOp>,
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
            plan: PythonFilePlan::default(),
            point_ids: BTreeSet::new(),
            decision_ids: BTreeSet::new(),
            branch_ids: BTreeSet::new(),
            claimed_lines: BTreeSet::new(),
            decision_boolops: BTreeMap::new(),
            error: None,
        }
    }

    fn fail<T>(&mut self, error: PythonInstrumenterError) -> Option<T> {
        self.error.get_or_insert(error);
        None
    }

    fn location_source(&mut self, range: TextRange) -> Option<(usize, usize, String)> {
        let result = self.locations.range(range).map(|(start, _)| {
            let (line, column) = self.locations.line_column(start);
            (line, column, self.locations.text(range))
        });
        match result {
            Ok((line, column, Ok(source))) => Some((line, column, source)),
            Ok((_, _, Err(error))) | Err(error) => self.fail(error),
        }
    }

    fn span(&mut self, range: TextRange) -> Option<PlanSpan> {
        match self.locations.span(range) {
            Ok(span) => Some(span),
            Err(error) => self.fail(error),
        }
    }

    fn push_point(&mut self, id: &str, range: TextRange, kind: PointKind, label: Option<String>) {
        let Some((line, column, source)) = self.location_source(range) else {
            return;
        };
        self.manifest.points.push(PointMeta {
            id: id.into(),
            kind,
            file: self.file.into(),
            line,
            column,
            source,
            label,
        });
    }

    fn statement(&mut self, statement: &Stmt) {
        let range = statement.range();
        let id = stable_id(self.file, "statement", range, "");
        if !self.point_ids.insert(id.clone()) {
            return;
        }
        self.push_point(&id, range, PointKind::Statement, None);
        let Some(span) = self.span(range) else {
            return;
        };
        let start_line = span.start[0];
        // A compound statement is proven by its header expressions. CPython
        // stamps the header's instructions with the lines those expressions
        // occupy, never the keyword line alone, so claim every header line up
        // to the first body statement.
        let end_line = match first_body_statement(statement) {
            Some(body) => match self.span(body.range()) {
                Some(body_span) => body_span.start[0].saturating_sub(1).max(start_line),
                None => return,
            },
            None => span.end[0],
        };
        let exact = self.claimed_lines.contains(&start_line);
        if !exact {
            for line in start_line..=end_line {
                self.claimed_lines.insert(line);
            }
        }
        self.plan.statements.push(StatementPlan {
            id,
            lines: [start_line, end_line],
            start: span.start,
            end: span.end,
            exact,
        });
    }

    fn function(&mut self, range: TextRange, name: &str) {
        let id = stable_id(self.file, "function", range, name);
        if !self.point_ids.insert(id.clone()) {
            return;
        }
        self.push_point(&id, range, PointKind::Function, Some(name.to_owned()));
        let Some(span) = self.span(range) else {
            return;
        };
        self.plan.functions.push(FunctionPlan {
            id,
            line: span.start[0],
            name: name.to_owned(),
            span,
        });
    }

    fn strip_not(expr: &Expr) -> (&Expr, usize) {
        let mut current = expr;
        let mut depth = 0;
        while let Expr::UnaryOp(unary) = current {
            if unary.op != UnaryOp::Not {
                break;
            }
            depth += 1;
            current = &unary.operand;
        }
        (current, depth)
    }

    fn none_when_true(expr: &Expr) -> Option<bool> {
        let Expr::Compare(comparison) = expr else {
            return None;
        };
        let [operator] = comparison.ops.as_ref() else {
            return None;
        };
        let [right] = comparison.comparators.as_ref() else {
            return None;
        };
        if !matches!(comparison.left.as_ref(), Expr::NoneLiteral(_))
            && !matches!(right, Expr::NoneLiteral(_))
        {
            return None;
        }
        match operator {
            CmpOp::Is => Some(true),
            CmpOp::IsNot => Some(false),
            _ => None,
        }
    }

    /// Flatten a test expression into leaves and a short-circuit tree. A
    /// BoolOp is part of the tree only when it is the test itself or the
    /// direct operand of another tree BoolOp; `not (a and b)` stays one leaf
    /// because CPython inverts the jump senses inside it rather than exposing
    /// the operands as separate decision conditions.
    fn tree(
        &mut self,
        expr: &Expr,
        leaves: &mut Vec<ConditionPlan>,
        boolops: &mut Vec<(TextRange, Vec<Vec<usize>>)>,
    ) -> Option<ConditionTree> {
        let (operand, not) = Self::strip_not(expr);
        if let Expr::BoolOp(boolean) = operand {
            let mut items = Vec::with_capacity(boolean.values.len());
            let mut groups = Vec::with_capacity(boolean.values.len());
            for value in &boolean.values {
                let first = leaves.len();
                items.push(self.tree(value, leaves, boolops)?);
                groups.push((first..leaves.len()).collect());
            }
            boolops.push((boolean.range, groups));
            return Some(ConditionTree::Node {
                op: match boolean.op {
                    BoolOp::And => "and".into(),
                    BoolOp::Or => "or".into(),
                },
                items,
                negate: not % 2 == 1,
            });
        }
        let span = self.span(operand.range())?;
        leaves.push(ConditionPlan {
            span,
            not,
            none_when_true: Self::none_when_true(operand),
        });
        Some(ConditionTree::Leaf(leaves.len() - 1))
    }

    fn decision(&mut self, test: &Expr, kind: &str, comprehension: Option<TextRange>) {
        let range = test.range();
        let id = stable_id(self.file, "decision", range, kind);
        if !self.decision_ids.insert(id.clone()) {
            return;
        }
        let Some((line, column, source)) = self.location_source(range) else {
            return;
        };
        let mut leaves = Vec::new();
        let mut boolops = Vec::new();
        let Some(tree) = self.tree(test, &mut leaves, &mut boolops) else {
            return;
        };
        let mut conditions = Vec::with_capacity(leaves.len());
        for leaf in &leaves {
            let leaf_range = TextRange::new(
                self.locations.line_starts[leaf.span.start[0] - 1]
                    .saturating_add(leaf.span.start[1])
                    .try_into()
                    .unwrap_or_default(),
                self.locations.line_starts[leaf.span.end[0] - 1]
                    .saturating_add(leaf.span.end[1])
                    .try_into()
                    .unwrap_or_default(),
            );
            match self.locations.text(leaf_range) {
                Ok(text) => conditions.push(if leaf.not % 2 == 1 {
                    format!("not {text}")
                } else {
                    text
                }),
                Err(error) => {
                    self.fail::<()>(error);
                    return;
                }
            }
        }
        for (boolop_range, groups) in boolops {
            let (start, end) = match self.locations.range(boolop_range) {
                Ok(bounds) => bounds,
                Err(error) => {
                    self.fail::<()>(error);
                    return;
                }
            };
            self.decision_boolops
                .insert((start, end), (id.clone(), groups));
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
        let outcome_id = format!("{id}:outcome");
        self.branch_with_id(
            outcome_id.clone(),
            range,
            kind,
            source,
            [("true", "true"), ("false", "false")],
        );
        let Some(span) = self.span(range) else {
            return;
        };
        let comprehension = match comprehension {
            Some(range) => match self.span(range) {
                Some(span) => Some(span),
                None => return,
            },
            None => None,
        };
        self.plan.decisions.push(DecisionPlan {
            id,
            kind: kind.into(),
            span,
            conditions: leaves,
            tree,
            comprehension,
            outcome_true: format!("{outcome_id}:true"),
            outcome_false: format!("{outcome_id}:false"),
        });
    }

    fn branch<const N: usize>(
        &mut self,
        range: TextRange,
        kind: &str,
        alternatives: [(&str, &str); N],
    ) -> Option<String> {
        let id = stable_id(self.file, "branch", range, kind);
        let (_, _, source) = self.location_source(range)?;
        self.branch_with_id(id, range, kind, source, alternatives)
    }

    fn branch_with_id<const N: usize>(
        &mut self,
        id: String,
        range: TextRange,
        kind: &str,
        source: String,
        alternatives: [(&str, &str); N],
    ) -> Option<String> {
        if !self.branch_ids.insert(id.clone()) {
            return None;
        }
        let (line, column, _) = self.location_source(range)?;
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
        Some(id)
    }

    fn loop_branch(&mut self, range: TextRange, iter: &Expr, kind: &str) {
        let Some(id) = self.branch(
            range,
            kind,
            [("zero", "zero iterations"), ("entered", "entered")],
        ) else {
            return;
        };
        let Some(iter_span) = self.span(iter.range()) else {
            return;
        };
        self.plan.loops.push(LoopPlan {
            zero: format!("{id}:zero"),
            entered: format!("{id}:entered"),
            id,
            iter: iter_span,
        });
    }

    fn try_statement(&mut self, statement: &ruff_python_ast::StmtTry) {
        let Some(id) = self.branch(
            statement.range,
            if statement.is_star { "try-star" } else { "try" },
            [("success", "try completed"), ("raised", "handler entered")],
        ) else {
            return;
        };
        let body_range = match (statement.body.first(), statement.body.last()) {
            (Some(first), Some(last)) => TextRange::new(first.start(), last.end()),
            _ => return,
        };
        let Some(body) = self.span(body_range) else {
            return;
        };
        let block_span = |collector: &mut Self, block: &[Stmt]| -> Option<Option<PlanSpan>> {
            match (block.first(), block.last()) {
                (Some(first), Some(last)) => collector
                    .span(TextRange::new(first.start(), last.end()))
                    .map(Some),
                _ => Some(None),
            }
        };
        let Some(orelse) = block_span(self, &statement.orelse) else {
            return;
        };
        let Some(finalbody) = block_span(self, &statement.finalbody) else {
            return;
        };
        let mut handlers = Vec::with_capacity(statement.handlers.len());
        for (index, handler) in statement.handlers.iter().enumerate() {
            let Some(handler_id) = self.branch(
                handler.range(),
                &format!("except-{index}"),
                [("missed", "not selected"), ("selected", "selected")],
            ) else {
                return;
            };
            let ruff_python_ast::ExceptHandler::ExceptHandler(clause) = handler;
            let (Some(first), Some(last)) = (clause.body.first(), clause.body.last()) else {
                return;
            };
            let header_range = TextRange::new(clause.range.start(), first.start());
            let (Some(header), Some(first_span), Some(last_span)) = (
                self.span(header_range),
                self.span(first.range()),
                self.span(last.range()),
            ) else {
                return;
            };
            handlers.push(HandlerPlan {
                missed: format!("{handler_id}:missed"),
                selected: format!("{handler_id}:selected"),
                id: handler_id,
                header,
                body_lines: [first_span.start[0], last_span.end[0]],
                bare: clause.type_.is_none(),
            });
        }
        self.plan.tries.push(TryPlan {
            success: format!("{id}:success"),
            raised: format!("{id}:raised"),
            id,
            body,
            orelse,
            finalbody,
            handlers,
        });
    }

    fn logical(&mut self, boolean: &ruff_python_ast::ExprBoolOp) {
        let Some(boolop_span) = self.span(boolean.range) else {
            return;
        };
        let bounds = match self.locations.range(boolean.range) {
            Ok(bounds) => bounds,
            Err(error) => {
                self.fail::<()>(error);
                return;
            }
        };
        let decision = self.decision_boolops.get(&bounds).cloned();
        let op = match boolean.op {
            BoolOp::And => "and",
            BoolOp::Or => "or",
        };
        for (index, value) in boolean.values.iter().enumerate().skip(1) {
            let Some(id) = self.branch(
                value.range(),
                &format!("logical-{op}-{index}"),
                [
                    ("short-circuit", "short-circuited"),
                    ("evaluated", "right operand evaluated"),
                ],
            ) else {
                continue;
            };
            let (decision_id, previous_leaves, operand_leaves) = match &decision {
                Some((decision_id, groups)) => (
                    Some(decision_id.clone()),
                    Some(groups[index - 1].clone()),
                    Some(groups[index].clone()),
                ),
                None => (None, None, None),
            };
            self.plan.logical.push(LogicalPlan {
                short_circuit: format!("{id}:short-circuit"),
                evaluated: format!("{id}:evaluated"),
                id,
                boolop: boolop_span,
                operand: index,
                decision: decision_id,
                previous_leaves,
                operand_leaves,
            });
        }
    }

    fn match_statement(&mut self, statement: &ruff_python_ast::StmtMatch) {
        let Some(span) = self.span(statement.range) else {
            return;
        };
        let mut cases = Vec::with_capacity(statement.cases.len());
        for (index, case) in statement.cases.iter().enumerate() {
            let kind = format!("match-case-{index}");
            let Some(id) = self.branch(
                case.range,
                &kind,
                [("missed", "not selected"), ("selected", "selected")],
            ) else {
                return;
            };
            if let Some(guard) = &case.guard {
                self.decision(guard, "match-guard", None);
            }
            let test_range = match &case.guard {
                Some(guard) => TextRange::new(case.pattern.start(), guard.end()),
                None => case.pattern.range(),
            };
            let (Some(case_span), Some(test)) = (self.span(case.range), self.span(test_range))
            else {
                return;
            };
            let body_lines = match (case.body.first(), case.body.last()) {
                (Some(first), Some(last)) => {
                    match (self.span(first.range()), self.span(last.range())) {
                        (Some(first), Some(last)) => [first.start[0], last.end[0]],
                        _ => return,
                    }
                }
                _ => [case_span.start[0], case_span.end[0]],
            };
            cases.push(MatchCasePlan {
                missed: format!("{id}:missed"),
                selected: format!("{id}:selected"),
                id,
                span: case_span,
                test,
                irrefutable: case.guard.is_none() && case.pattern.is_irrefutable(),
                body_lines,
            });
        }
        let no_case = if statement
            .cases
            .iter()
            .any(|case| case.guard.is_none() && case.pattern.is_irrefutable())
        {
            None
        } else {
            self.branch(
                statement.subject.range(),
                "match-no-case",
                [
                    ("matched", "some case matched"),
                    ("unmatched", "no case matched"),
                ],
            )
            .map(|id| MatchNoCasePlan {
                matched: format!("{id}:matched"),
                unmatched: format!("{id}:unmatched"),
                id,
            })
        };
        self.plan.matches.push(MatchPlan {
            span,
            cases,
            no_case,
        });
    }

    fn visit_body_statements(&mut self, body: &'a [Stmt]) {
        for (index, statement) in body.iter().enumerate() {
            if is_unobservable_statement(statement, index == 0) {
                // Its expressions still may not contain obligations worth
                // walking (docstrings, names), so skip entirely.
                continue;
            }
            self.visit_stmt(statement);
        }
    }
}

impl<'a> Visitor<'a> for PythonObligationCollector<'a> {
    fn visit_body(&mut self, body: &'a [Stmt]) {
        self.visit_body_statements(body);
    }

    fn visit_stmt(&mut self, statement: &'a Stmt) {
        self.statement(statement);
        match statement {
            Stmt::FunctionDef(function) => self.function(function.range, &function.name),
            Stmt::If(statement) => {
                self.decision(&statement.test, "if", None);
                for clause in &statement.elif_else_clauses {
                    if let Some(test) = &clause.test {
                        self.decision(test, "elif", None);
                    }
                }
            }
            Stmt::While(statement) => {
                self.decision(&statement.test, "while", None);
            }
            Stmt::For(statement) => self.loop_branch(
                statement.range,
                &statement.iter,
                if statement.is_async {
                    "async-for"
                } else {
                    "for"
                },
            ),
            Stmt::Match(statement) => self.match_statement(statement),
            Stmt::Try(statement) => self.try_statement(statement),
            Stmt::Assert(statement) => {
                self.decision(&statement.test, "assert", None);
            }
            _ => {}
        }
        walk_stmt(self, statement);
    }

    fn visit_expr(&mut self, expression: &'a Expr) {
        match expression {
            Expr::Lambda(lambda) => self.function(lambda.range, "<lambda>"),
            Expr::If(expression) => {
                self.decision(&expression.test, "ternary", None);
            }
            Expr::BoolOp(expression) => self.logical(expression),
            _ => {}
        }
        walk_expr(self, expression);
    }

    fn visit_comprehension(&mut self, comprehension: &'a Comprehension) {
        self.loop_branch(
            comprehension.range,
            &comprehension.iter,
            if comprehension.is_async {
                "async-comprehension"
            } else {
                "comprehension"
            },
        );
        for condition in &comprehension.ifs {
            self.decision(condition, "comprehension-if", Some(comprehension.range));
        }
        walk_comprehension(self, comprehension);
    }
}

/// Build the complete obligation manifest and runtime probe plan for one
/// Python source file. Limitations are per file; the run-level frontend
/// deduplicates their IDs across files.
pub fn build_python_obligations(
    file: &str,
    source: &str,
) -> Result<PythonFileObligations, PythonInstrumenterError> {
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
    collector.manifest.unmeasured.sort();
    collector.manifest.unmeasured.dedup();
    Ok(PythonFileObligations {
        manifest: collector.manifest,
        plan: collector.plan,
    })
}

/// Manifest-only view kept for callers that predate the probe plan.
pub fn build_python_manifest(
    file: &str,
    source: &str,
) -> Result<CoverageManifest, PythonInstrumenterError> {
    build_python_obligations(file, source).map(|obligations| obligations.manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = r#"async def classify[T](items, flag=True):
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

    #[test]
    fn discovers_current_python_obligations_with_exact_ranges_and_stable_ids() {
        let first = build_python_obligations("src/app.py", SOURCE).unwrap();
        let second = build_python_obligations("src/app.py", SOURCE).unwrap();
        assert_eq!(first, second);
        let manifest = &first.manifest;
        assert!(manifest.points.iter().any(|point| {
            point.kind == PointKind::Function && point.label.as_deref() == Some("classify")
        }));
        assert!(manifest.points.iter().any(|point| {
            point.kind == PointKind::Function && point.label.as_deref() == Some("<lambda>")
        }));
        let if_decision = manifest
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
        for kind in ["ternary", "match-guard", "comprehension-if", "elif"] {
            assert!(
                manifest
                    .decisions
                    .iter()
                    .any(|decision| decision.kind == kind),
                "missing {kind}"
            );
        }
        assert!(
            manifest
                .branches
                .iter()
                .any(|branch| branch.kind == "async-comprehension")
        );
        assert!(
            manifest
                .branches
                .iter()
                .any(|branch| branch.kind == "try-star")
        );
        assert!(
            manifest
                .branches
                .iter()
                .any(|branch| branch.kind.starts_with("logical-and"))
        );
        assert!(
            manifest
                .decisions
                .iter()
                .all(|decision| decision.id.starts_with("py:decision:"))
        );
        assert!(manifest.unmeasured.is_empty());
        assert!(manifest.limitations.is_empty());
    }

    #[test]
    fn plan_carries_spans_polarity_trees_and_trigger_lines() {
        let plan = build_python_obligations("src/app.py", SOURCE).unwrap().plan;
        let if_plan = plan
            .decisions
            .iter()
            .find(|decision| decision.kind == "if")
            .unwrap();
        assert_eq!(if_plan.conditions.len(), 3);
        assert_eq!(if_plan.conditions[0].span.start, [4, 11]);
        assert_eq!(if_plan.conditions[0].span.end, [4, 24]);
        assert_eq!(if_plan.conditions[1].span.start, [4, 30]);
        assert_eq!(
            if_plan.tree,
            ConditionTree::Node {
                op: "and".into(),
                items: vec![
                    ConditionTree::Leaf(0),
                    ConditionTree::Node {
                        op: "or".into(),
                        items: vec![ConditionTree::Leaf(1), ConditionTree::Leaf(2)],
                        negate: false,
                    },
                ],
                negate: false,
            }
        );
        // The decision-tree BoolOps produce logical branches derived from the
        // vector rather than from value-context jumps.
        let logical_or = plan
            .logical
            .iter()
            .find(|logical| logical.decision.as_deref() == Some(if_plan.id.as_str()))
            .unwrap();
        assert!(logical_or.previous_leaves.is_some());
        // The lambda's `value or flag` is value context.
        assert!(
            plan.logical
                .iter()
                .any(|logical| logical.decision.is_none())
        );
        let comprehension = plan
            .decisions
            .iter()
            .find(|decision| decision.kind == "comprehension-if")
            .unwrap();
        assert!(comprehension.comprehension.is_some());
        // `async def classify` header spans line 1 only; its body starts on 2.
        let function_statement = plan
            .statements
            .iter()
            .find(|statement| statement.lines[0] == 1)
            .unwrap();
        assert_eq!(function_statement.lines, [1, 1]);
        assert!(
            plan.functions
                .iter()
                .any(|function| function.name == "classify" && function.line == 1)
        );
        assert_eq!(plan.loops.len(), 2);
        assert_eq!(plan.matches.len(), 1);
        assert_eq!(plan.matches[0].cases.len(), 2);
        assert!(plan.matches[0].no_case.is_some());
        assert_eq!(plan.tries.len(), 1);
        let try_plan = &plan.tries[0];
        assert_eq!(try_plan.body.start, [9, 8]);
        assert_eq!(try_plan.handlers.len(), 1);
        assert_eq!(try_plan.handlers[0].body_lines, [15, 15]);
        assert!(!try_plan.handlers[0].bare);
        assert!(try_plan.finalbody.is_none());
    }

    #[test]
    fn not_polarity_and_same_line_statements_are_modelled() {
        let source = "def f(a, b):\n    if not (a and b): return 1\n    x = 1; y = 2\n    g = lambda: 1; h = lambda: 2\n    return x + y\n";
        let obligations = build_python_obligations("m.py", source).unwrap();
        let decision = &obligations.plan.decisions[0];
        // `not (a and b)` keeps `a` and `b` as separate conditions and negates
        // the node, matching the one-jump-per-operand bytecode.
        assert_eq!(decision.conditions.len(), 2);
        assert_eq!(decision.conditions[0].not, 0);
        assert_eq!(
            decision.tree,
            ConditionTree::Node {
                op: "and".into(),
                items: vec![ConditionTree::Leaf(0), ConditionTree::Leaf(1)],
                negate: true,
            }
        );
        assert_eq!(obligations.manifest.decisions[0].conditions, ["a", "b"]);
        let negated_leaf = build_python_obligations(
            "n.py",
            "def g(a):
    if not a:
        return 1
",
        )
        .unwrap();
        assert_eq!(negated_leaf.plan.decisions[0].conditions[0].not, 1);
        assert_eq!(negated_leaf.manifest.decisions[0].conditions, ["not a"]);
        let none_comparisons = build_python_obligations(
            "none.py",
            "def g(a, b):\n    if a is None or b is not None:\n        return 1\n",
        )
        .unwrap();
        let conditions = &none_comparisons.plan.decisions[0].conditions;
        assert_eq!(conditions[0].none_when_true, Some(true));
        assert_eq!(conditions[1].none_when_true, Some(false));
        // `return 1` shares line 2 with the `if`, `y = 2` shares line 3 with
        // `x = 1`, and `h = ...` shares line 4 with `g = ...`: those three are
        // proven by INSTRUCTION events at their exact start, nothing is
        // unmeasured, and both lambdas keep their spans.
        let exact = obligations
            .plan
            .statements
            .iter()
            .filter(|statement| statement.exact)
            .map(|statement| statement.start)
            .collect::<Vec<_>>();
        assert_eq!(exact, [[2, 22], [3, 11], [4, 19]]);
        assert!(obligations.manifest.unmeasured.is_empty());
        assert!(obligations.manifest.limitations.is_empty());
        let lambdas = obligations
            .plan
            .functions
            .iter()
            .filter(|function| function.name == "<lambda>")
            .collect::<Vec<_>>();
        assert_eq!(lambdas.len(), 2);
        assert_ne!(lambdas[0].span, lambdas[1].span);
    }

    #[test]
    fn try_statements_carry_bodies_handlers_and_finally_spans() {
        let source = "def g(x):\n    try:\n        y = int(x)\n    except ValueError:\n        y = -1\n    except:\n        y = -2\n    else:\n        y += 1\n    finally:\n        x = None\n    return y\n";
        let plan = build_python_obligations("t.py", source).unwrap().plan;
        let try_plan = &plan.tries[0];
        assert_eq!(
            try_plan.body,
            PlanSpan {
                start: [3, 8],
                end: [3, 18]
            }
        );
        assert_eq!(
            try_plan.orelse,
            Some(PlanSpan {
                start: [9, 8],
                end: [9, 14]
            })
        );
        assert_eq!(
            try_plan.finalbody,
            Some(PlanSpan {
                start: [11, 8],
                end: [11, 16]
            })
        );
        assert_eq!(try_plan.handlers.len(), 2);
        assert_eq!(
            try_plan.handlers[0].header,
            PlanSpan {
                start: [4, 4],
                end: [5, 8]
            }
        );
        assert!(!try_plan.handlers[0].bare);
        assert!(try_plan.handlers[1].bare);
        assert_eq!(try_plan.handlers[1].body_lines, [7, 7]);
    }

    #[test]
    fn docstrings_and_scope_declarations_are_not_statements() {
        let source = "\"\"\"module doc\"\"\"\nX = 1\ndef f():\n    \"\"\"doc\"\"\"\n    global X\n    pass\n";
        let obligations = build_python_obligations("m.py", source).unwrap();
        let statements = obligations
            .manifest
            .points
            .iter()
            .filter(|point| point.kind == PointKind::Statement)
            .map(|point| point.line)
            .collect::<Vec<_>>();
        assert_eq!(statements, [2, 3, 6]);
    }

    #[test]
    fn rejects_invalid_python_without_partial_obligations() {
        assert!(matches!(
            build_python_manifest("src/broken.py", "if :\n    pass\n"),
            Err(PythonInstrumenterError::Parse(_))
        ));
    }
}
