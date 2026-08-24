//! First oxc-backed vertical slice of the Rust JavaScript instrumenter.
//!
//! This candidate reports and instruments statements, functions, and the
//! frozen control-decision surface, including semantic-safety boundaries and
//! exact wide-decision fallback.
//! It is not exposed by the CLI and cannot claim a complete denominator until
//! the remaining reference transformations are ported.

use std::{
    collections::{HashMap, HashSet},
    fmt::Write,
    path::Path,
};

use oxc_allocator::{Allocator, TakeIn};
use oxc_ast::{
    AstBuilder, NONE,
    ast::{
        Argument, ArrayExpressionElement, ArrowFunctionExpression, AssignmentTarget,
        CallExpression, ConditionalExpression, Declaration, DoWhileStatement, Expression,
        ForInStatement, ForOfStatement, ForStatement, Function, FunctionBody, IfStatement,
        NewExpression, ObjectPropertyKind, Program, PropertyKey, PropertyKind, Statement,
        VariableDeclarationKind, WhileStatement, WithStatement,
    },
};
use oxc_ast_visit::{Visit, VisitMut, walk, walk_mut};
use oxc_codegen::Codegen;
use oxc_parser::Parser;
use oxc_semantic::SemanticBuilder;
use oxc_span::{GetSpan, SourceType, Span};
use oxc_syntax::{
    number::NumberBase,
    operator::{AssignmentOperator, BinaryOperator, LogicalOperator},
    scope::ScopeFlags,
};
use oxc_traverse::{Ancestor, Traverse, TraverseCtx, traverse_mut};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateDecision {
    pub id: String,
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub source: String,
    pub conditions: Vec<String>,
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidatePoint {
    pub id: String,
    pub kind: String,
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateOutput {
    pub engine: String,
    pub complete: bool,
    pub supported_surface: String,
    pub code: String,
    pub decisions: Vec<CandidateDecision>,
    pub points: Vec<CandidatePoint>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<CandidateRuntime>,
    pub coverage_limitations: Vec<CandidateLimitation>,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateRuntime {
    pub coverage_hit: String,
    pub mcdc_begin: String,
    pub mcdc_condition: String,
    pub mcdc_end: String,
    pub mcdc_end_v2: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateLimitation {
    pub id: String,
    pub kind: String,
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub source: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandidateError {
    UnknownSourceType(String),
    Parse(Vec<String>),
}

type SpanKey = (u32, u32);

#[derive(Default)]
struct SafetyAnalysis {
    source_sensitive_functions: HashSet<SpanKey>,
    with_statements: HashSet<SpanKey>,
    limitations: Vec<CandidateLimitation>,
}

struct SafetyScanner<'s> {
    source: &'s str,
    file: &'s str,
    source_sensitive_functions: HashSet<SpanKey>,
    with_statements: HashSet<SpanKey>,
    function_limitations: Vec<CandidateLimitation>,
    with_limitations: Vec<CandidateLimitation>,
    dynamic_limitations: Vec<CandidateLimitation>,
    unsafe_function_depth: usize,
    with_depth: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PointPass {
    Statements,
    Functions,
}

struct PointCollector<'s> {
    source: &'s str,
    file: &'s str,
    pass: PointPass,
    points: Vec<CandidatePoint>,
    statement_targets: HashMap<SpanKey, Vec<String>>,
    function_targets: HashMap<SpanKey, String>,
    source_sensitive_functions: &'s HashSet<SpanKey>,
    unsafe_function_depth: usize,
    with_depth: usize,
}

#[derive(Default)]
struct PointAnalysis {
    points: Vec<CandidatePoint>,
    statement_targets: HashMap<SpanKey, Vec<String>>,
    function_targets: HashMap<SpanKey, String>,
}

impl PointCollector<'_> {
    fn point(&self, span: Span, kind: &str, label: Option<String>) -> CandidatePoint {
        let (line, column) = line_and_utf16_column(self.source, span.start as usize);
        CandidatePoint {
            id: stable_id(self.source, self.file, kind, span, ""),
            kind: kind.to_string(),
            file: self.file.to_string(),
            line,
            column,
            source: source_slice(self.source, span).to_string(),
            label,
        }
    }

    fn unsafe_context(&self) -> bool {
        self.unsafe_function_depth > 0 || self.with_depth > 0
    }

    fn exit_function(&mut self, span: Span) {
        if self.source_sensitive_functions.contains(&span_key(span)) {
            self.unsafe_function_depth -= 1;
        }
    }
}

fn function_label<State>(
    own_name: Option<&str>,
    context: &TraverseCtx<'_, State>,
) -> Option<String> {
    if let Some(name) = own_name {
        return Some(name.to_string());
    }
    match context.ancestors().next()? {
        Ancestor::ObjectPropertyValue(parent) => property_label(parent.key()),
        Ancestor::MethodDefinitionValue(parent) => property_label(parent.key()),
        Ancestor::VariableDeclaratorInit(parent) => parent
            .id()
            .get_binding_identifier()
            .map(|identifier| identifier.name.to_string()),
        _ => None,
    }
}

fn property_label(key: &PropertyKey<'_>) -> Option<String> {
    key.static_name()
        .map(|name| name.into_owned())
        .or_else(|| match key {
            PropertyKey::Identifier(identifier) => Some(identifier.name.to_string()),
            _ => None,
        })
}

fn function_point_span<State>(span: Span, context: &TraverseCtx<'_, State>) -> Span {
    match context.ancestors().next() {
        Some(Ancestor::ObjectPropertyValue(parent))
            if *parent.method() || *parent.kind() != PropertyKind::Init =>
        {
            *parent.span()
        }
        Some(Ancestor::MethodDefinitionValue(parent)) => *parent.span(),
        _ => span,
    }
}

fn executable_statement(statement: &Statement<'_>) -> bool {
    !matches!(
        statement,
        Statement::BlockStatement(_)
            | Statement::EmptyStatement(_)
            | Statement::FunctionDeclaration(_)
    ) && !statement.is_typescript_syntax()
}

impl<'a> Traverse<'a, ()> for PointCollector<'_> {
    fn enter_statement(&mut self, node: &mut Statement<'a>, context: &mut TraverseCtx<'a, ()>) {
        let mut ancestors = context.ancestors();
        let parent = ancestors.next();
        let expression_arrow_body = matches!(parent, Some(Ancestor::FunctionBodyStatements(_)))
            && matches!(ancestors.next(), Some(Ancestor::ArrowFunctionExpressionBody(arrow)) if *arrow.expression());
        if self.pass != PointPass::Statements
            || self.unsafe_context()
            || !executable_statement(node)
            || matches!(parent, Some(Ancestor::LabeledStatementBody(_)))
            || expression_arrow_body
        {
            return;
        }
        let point = self.point(node.span(), "statement", None);
        self.statement_targets
            .entry(span_key(node.span()))
            .or_default()
            .push(point.id.clone());
        self.points.push(point);
    }

    fn enter_declaration(&mut self, node: &mut Declaration<'a>, context: &mut TraverseCtx<'a, ()>) {
        if self.pass != PointPass::Statements
            || self.unsafe_context()
            || node.is_typescript_syntax()
            || matches!(node, Declaration::FunctionDeclaration(_))
            || !matches!(
                context.ancestors().next(),
                Some(Ancestor::ExportNamedDeclarationDeclaration(_))
                    | Some(Ancestor::ExportDefaultDeclarationDeclaration(_))
            )
        {
            return;
        }
        let point = self.point(node.span(), "statement", None);
        self.statement_targets
            .entry(span_key(node.span()))
            .or_default()
            .push(point.id.clone());
        self.points.push(point);
    }

    fn enter_function(&mut self, node: &mut Function<'a>, context: &mut TraverseCtx<'a, ()>) {
        let point_span = function_point_span(node.span, context);
        let label = if point_span == node.span {
            function_label(node.id.as_ref().map(|id| id.name.as_str()), context)
        } else {
            None
        };
        if self
            .source_sensitive_functions
            .contains(&span_key(node.span))
        {
            self.unsafe_function_depth += 1;
            return;
        }
        if self.pass == PointPass::Functions && !self.unsafe_context() && node.body.is_some() {
            let point = self.point(point_span, "function", label);
            self.function_targets
                .insert(span_key(node.span), point.id.clone());
            self.points.push(point);
        }
    }

    fn exit_function(&mut self, node: &mut Function<'a>, _context: &mut TraverseCtx<'a, ()>) {
        self.exit_function(node.span);
    }

    fn enter_arrow_function_expression(
        &mut self,
        node: &mut ArrowFunctionExpression<'a>,
        context: &mut TraverseCtx<'a, ()>,
    ) {
        let point_span = function_point_span(node.span, context);
        let label = if point_span == node.span {
            function_label(None, context)
        } else {
            None
        };
        if self
            .source_sensitive_functions
            .contains(&span_key(node.span))
        {
            self.unsafe_function_depth += 1;
            return;
        }
        if self.pass == PointPass::Functions && !self.unsafe_context() {
            let point = self.point(point_span, "function", label);
            self.function_targets
                .insert(span_key(node.span), point.id.clone());
            self.points.push(point);
        }
    }

    fn exit_arrow_function_expression(
        &mut self,
        node: &mut ArrowFunctionExpression<'a>,
        _context: &mut TraverseCtx<'a, ()>,
    ) {
        self.exit_function(node.span);
    }

    fn enter_with_statement(
        &mut self,
        _node: &mut WithStatement<'a>,
        _context: &mut TraverseCtx<'a, ()>,
    ) {
        self.with_depth += 1;
    }

    fn exit_with_statement(
        &mut self,
        _node: &mut WithStatement<'a>,
        _context: &mut TraverseCtx<'a, ()>,
    ) {
        self.with_depth -= 1;
    }
}

fn collect_points<'a>(
    allocator: &'a Allocator,
    program: &mut Program<'a>,
    source: &str,
    file: &str,
    source_sensitive_functions: &HashSet<SpanKey>,
) -> PointAnalysis {
    let mut analysis = PointAnalysis::default();
    for pass in [PointPass::Statements, PointPass::Functions] {
        let mut collector = PointCollector {
            source,
            file,
            pass,
            points: Vec::new(),
            statement_targets: HashMap::new(),
            function_targets: HashMap::new(),
            source_sensitive_functions,
            unsafe_function_depth: 0,
            with_depth: 0,
        };
        traverse_mut(&mut collector, allocator, program, Default::default(), ());
        analysis.points.extend(collector.points);
        analysis
            .statement_targets
            .extend(collector.statement_targets);
        analysis.function_targets.extend(collector.function_targets);
    }
    analysis
}

impl<'s> SafetyScanner<'s> {
    fn new(source: &'s str, file: &'s str) -> Self {
        Self {
            source,
            file,
            source_sensitive_functions: HashSet::new(),
            with_statements: HashSet::new(),
            function_limitations: Vec::new(),
            with_limitations: Vec::new(),
            dynamic_limitations: Vec::new(),
            unsafe_function_depth: 0,
            with_depth: 0,
        }
    }

    fn limitation(
        &self,
        span: Span,
        kind: &str,
        suffix: &str,
        reason: &str,
    ) -> CandidateLimitation {
        let (line, column) = line_and_utf16_column(self.source, span.start as usize);
        CandidateLimitation {
            id: stable_id(self.source, self.file, kind, span, suffix),
            kind: kind.to_string(),
            file: self.file.to_string(),
            line,
            column,
            source: source_slice(self.source, span).to_string(),
            reason: reason.to_string(),
        }
    }

    fn enter_source_sensitive_function<State>(
        &mut self,
        span: Span,
        context: &TraverseCtx<'_, State>,
    ) {
        let sensitive = observes_function_source(span, context);
        if sensitive {
            self.source_sensitive_functions.insert(span_key(span));
            let limitation = self.limitation(
                span,
                "semantic-safety",
                "function-source",
                "function body is left uninstrumented because this expression observes or coerces Function source text",
            );
            self.function_limitations.push(limitation);
            self.unsafe_function_depth += 1;
        }
    }

    fn exit_source_sensitive_function(&mut self, span: Span) {
        if self.source_sensitive_functions.contains(&span_key(span)) {
            self.unsafe_function_depth -= 1;
        }
    }

    fn is_unsafe_context(&self) -> bool {
        self.unsafe_function_depth > 0 || self.with_depth > 0
    }
}

impl<'a> Traverse<'a, ()> for SafetyScanner<'_> {
    fn enter_function(&mut self, node: &mut Function<'a>, context: &mut TraverseCtx<'a, ()>) {
        self.enter_source_sensitive_function(node.span, context);
    }

    fn exit_function(&mut self, node: &mut Function<'a>, _context: &mut TraverseCtx<'a, ()>) {
        self.exit_source_sensitive_function(node.span);
    }

    fn enter_arrow_function_expression(
        &mut self,
        node: &mut ArrowFunctionExpression<'a>,
        context: &mut TraverseCtx<'a, ()>,
    ) {
        self.enter_source_sensitive_function(node.span, context);
    }

    fn exit_arrow_function_expression(
        &mut self,
        node: &mut ArrowFunctionExpression<'a>,
        _context: &mut TraverseCtx<'a, ()>,
    ) {
        self.exit_source_sensitive_function(node.span);
    }

    fn enter_with_statement(
        &mut self,
        node: &mut WithStatement<'a>,
        _context: &mut TraverseCtx<'a, ()>,
    ) {
        self.with_statements.insert(span_key(node.span));
        let limitation = self.limitation(
            node.span,
            "semantic-safety",
            "with-environment",
            "with-statement body is left uninstrumented because its object environment can intercept probe identifiers",
        );
        self.with_limitations.push(limitation);
        self.with_depth += 1;
    }

    fn exit_with_statement(
        &mut self,
        _node: &mut WithStatement<'a>,
        _context: &mut TraverseCtx<'a, ()>,
    ) {
        self.with_depth -= 1;
    }

    fn enter_call_expression(
        &mut self,
        node: &mut CallExpression<'a>,
        _context: &mut TraverseCtx<'a, ()>,
    ) {
        if self.is_unsafe_context() || !expression_is_identifier(&node.callee, "eval") {
            return;
        }
        let limitation = self.limitation(
            node.span,
            "dynamic-code",
            "eval",
            "eval-generated source has no stable pre-run coverage denominator",
        );
        self.dynamic_limitations.push(limitation);
    }

    fn enter_new_expression(
        &mut self,
        node: &mut NewExpression<'a>,
        _context: &mut TraverseCtx<'a, ()>,
    ) {
        if self.is_unsafe_context() || !expression_is_identifier(&node.callee, "Function") {
            return;
        }
        let limitation = self.limitation(
            node.span,
            "dynamic-code",
            "Function",
            "Function-generated source has no stable pre-run coverage denominator",
        );
        self.dynamic_limitations.push(limitation);
    }
}

fn analyze_safety<'a>(
    allocator: &'a Allocator,
    program: &mut Program<'a>,
    source: &str,
    file: &str,
) -> SafetyAnalysis {
    // oxc_traverse uses resolved lexical scope IDs while walking ancestry.
    // Building semantics here initializes those IDs without changing the AST.
    SemanticBuilder::new().build(program);
    let mut scanner = SafetyScanner::new(source, file);
    traverse_mut(&mut scanner, allocator, program, Default::default(), ());
    let mut limitations = scanner.function_limitations;
    limitations.extend(scanner.with_limitations);
    limitations.extend(scanner.dynamic_limitations);
    SafetyAnalysis {
        source_sensitive_functions: scanner.source_sensitive_functions,
        with_statements: scanner.with_statements,
        limitations,
    }
}

fn span_key(span: Span) -> SpanKey {
    (span.start, span.end)
}

fn expression_is_identifier(expression: &Expression<'_>, name: &str) -> bool {
    matches!(expression, Expression::Identifier(identifier) if identifier.name == name)
}

fn observes_function_source<State>(span: Span, context: &TraverseCtx<'_, State>) -> bool {
    let mut child_end = span.end;
    for ancestor in context.ancestors() {
        match ancestor {
            Ancestor::ParenthesizedExpressionExpression(parent) => child_end = parent.span().end,
            Ancestor::TSAsExpressionExpression(parent) => child_end = parent.span().end,
            Ancestor::TSSatisfiesExpressionExpression(parent) => child_end = parent.span().end,
            Ancestor::TSTypeAssertionExpression(parent) => child_end = parent.span().end,
            Ancestor::TSNonNullExpressionExpression(parent) => child_end = parent.span().end,
            Ancestor::ConditionalExpressionConsequent(parent) => child_end = parent.span().end,
            Ancestor::ConditionalExpressionAlternate(parent) => child_end = parent.span().end,
            Ancestor::LogicalExpressionLeft(parent) => {
                child_end = parent.span().end;
            }
            Ancestor::LogicalExpressionRight(parent) => {
                child_end = parent.span().end;
            }
            Ancestor::SequenceExpressionExpressions(parent) => {
                if child_end != parent.span().end {
                    return false;
                }
                child_end = parent.span().end;
            }
            Ancestor::AssignmentExpressionRight(parent) => child_end = parent.span().end,
            Ancestor::ObjectPropertyKey(parent) => return *parent.computed(),
            Ancestor::MethodDefinitionKey(parent) => return *parent.computed(),
            Ancestor::PropertyDefinitionKey(parent) => return *parent.computed(),
            Ancestor::AccessorPropertyKey(parent) => return *parent.computed(),
            Ancestor::ComputedMemberExpressionExpression(_) => return true,
            Ancestor::StaticMemberExpressionObject(parent) => {
                return parent.property().name == "toString";
            }
            Ancestor::CallExpressionArguments(parent) => {
                return expression_is_identifier(parent.callee(), "String");
            }
            Ancestor::BinaryExpressionLeft(parent) => {
                return matches!(
                    parent.operator(),
                    BinaryOperator::Addition
                        | BinaryOperator::LessThan
                        | BinaryOperator::LessEqualThan
                        | BinaryOperator::GreaterThan
                        | BinaryOperator::GreaterEqualThan
                );
            }
            Ancestor::BinaryExpressionRight(parent) => {
                return matches!(
                    parent.operator(),
                    BinaryOperator::Addition
                        | BinaryOperator::LessThan
                        | BinaryOperator::LessEqualThan
                        | BinaryOperator::GreaterThan
                        | BinaryOperator::GreaterEqualThan
                );
            }
            _ => return false,
        }
    }
    false
}

pub fn analyze_candidate(source: &str, file: &str) -> Result<CandidateOutput, CandidateError> {
    let source_type = SourceType::from_path(Path::new(file))
        .map_err(|error| CandidateError::UnknownSourceType(error.to_string()))?;
    let allocator = Allocator::default();
    let mut parsed = Parser::new(&allocator, source, source_type).parse();
    if !parsed.errors.is_empty() {
        return Err(CandidateError::Parse(
            parsed
                .errors
                .into_iter()
                .map(|error| format!("{error:?}"))
                .collect(),
        ));
    }

    let safety = analyze_safety(&allocator, &mut parsed.program, source, file);
    let point_analysis = collect_points(
        &allocator,
        &mut parsed.program,
        source,
        file,
        &safety.source_sensitive_functions,
    );
    let mut collector = DecisionCollector {
        source,
        file,
        decisions: Vec::new(),
        source_sensitive_functions: &safety.source_sensitive_functions,
        with_statements: &safety.with_statements,
    };
    collector.visit_program(&parsed.program);
    let generated = Codegen::new().build(&parsed.program).code;
    Ok(CandidateOutput {
        engine: "rust-oxc".to_string(),
        complete: false,
        supported_surface: "control-decision-manifest-v1".to_string(),
        code: generated,
        decisions: collector.decisions,
        points: point_analysis.points,
        runtime: None,
        coverage_limitations: safety.limitations,
        limitations: vec![
            "candidate emits metadata only; use the private differential transform for probes"
                .to_string(),
            "coverage points, value branches, and extended branch obligations are not included"
                .to_string(),
        ],
    })
}

/// Instrument the first deliberately narrow Rust port slice.
///
/// The generated code is internal differential-test output, not a public
/// runtime ABI. It instruments control predicates of at most 32 conditions
/// with the frozen probe-v2 ternary frame while leaving every other coverage
/// surface explicitly incomplete.
pub fn instrument_candidate(source: &str, file: &str) -> Result<CandidateOutput, CandidateError> {
    let source_type = SourceType::from_path(Path::new(file))
        .map_err(|error| CandidateError::UnknownSourceType(error.to_string()))?;
    let allocator = Allocator::default();
    let mut parsed = Parser::new(&allocator, source, source_type).parse();
    if !parsed.errors.is_empty() {
        return Err(CandidateError::Parse(
            parsed
                .errors
                .into_iter()
                .map(|error| format!("{error:?}"))
                .collect(),
        ));
    }

    let safety = analyze_safety(&allocator, &mut parsed.program, source, file);
    let point_analysis = collect_points(
        &allocator,
        &mut parsed.program,
        source,
        file,
        &safety.source_sensitive_functions,
    );
    let mut collector = DecisionCollector {
        source,
        file,
        decisions: Vec::new(),
        source_sensitive_functions: &safety.source_sensitive_functions,
        with_statements: &safety.with_statements,
    };
    collector.visit_program(&parsed.program);

    let mut names = CandidateNames::new(source);
    let coverage_hit = names.allocate("__supercovCoverageHit");
    let mcdc_begin = names.allocate("__supercovMcdcBegin");
    let mcdc_condition = names.allocate("__supercovMcdcCondition");
    let mcdc_end = names.allocate("__supercovMcdcEnd");
    let mcdc_end_v2 = names.allocate("__supercovMcdcEndV2");
    let ast = AstBuilder::new(&allocator);
    let mut statement_transformer = StatementProbeTransformer {
        ast,
        coverage_hit: coverage_hit.clone(),
        targets: point_analysis.statement_targets,
        source_sensitive_functions: safety.source_sensitive_functions.clone(),
        with_statements: safety.with_statements.clone(),
    };
    statement_transformer.visit_program(&mut parsed.program);
    let mut function_transformer = FunctionProbeTransformer {
        ast,
        coverage_hit: coverage_hit.clone(),
        targets: point_analysis.function_targets,
        source_sensitive_functions: safety.source_sensitive_functions.clone(),
    };
    function_transformer.visit_program(&mut parsed.program);
    let mut transformer = ControlProbeV2Transformer {
        ast,
        file,
        decisions: &collector.decisions,
        mcdc_begin: mcdc_begin.clone(),
        mcdc_condition: mcdc_condition.clone(),
        mcdc_end: mcdc_end.clone(),
        mcdc_end_v2: mcdc_end_v2.clone(),
        names,
        scope_declarations: Vec::new(),
        decision_index: 0,
        source_sensitive_functions: safety.source_sensitive_functions.clone(),
        with_statements: safety.with_statements.clone(),
    };
    transformer.visit_program(&mut parsed.program);

    let limitations = vec![
        "only if, ternary, while, do-while, and classic-for decisions are instrumented"
            .to_string(),
        "value branches, extended branch obligations, and runtime registration remain on the TypeScript reference"
            .to_string(),
        "candidate runtime binding is differential-only and is not exposed by the public CLI"
            .to_string(),
    ];
    Ok(CandidateOutput {
        engine: "rust-oxc".to_string(),
        complete: false,
        supported_surface: "point-control-decision-probe-candidate".to_string(),
        code: Codegen::new().build(&parsed.program).code,
        decisions: collector.decisions,
        points: point_analysis.points,
        runtime: Some(CandidateRuntime {
            coverage_hit,
            mcdc_begin,
            mcdc_condition,
            mcdc_end,
            mcdc_end_v2,
        }),
        coverage_limitations: safety.limitations,
        limitations,
    })
}

struct StatementProbeTransformer<'a> {
    ast: AstBuilder<'a>,
    coverage_hit: String,
    targets: HashMap<SpanKey, Vec<String>>,
    source_sensitive_functions: HashSet<SpanKey>,
    with_statements: HashSet<SpanKey>,
}

impl<'a> StatementProbeTransformer<'a> {
    fn probe(&self, id: &str) -> Statement<'a> {
        self.ast.statement_expression(
            Span::default(),
            self.ast.expression_call(
                Span::default(),
                self.ast
                    .expression_identifier(Span::default(), self.ast.ident(&self.coverage_hit)),
                NONE,
                self.ast
                    .vec1(Argument::from(self.ast.expression_string_literal(
                        Span::default(),
                        self.ast.str(id),
                        None,
                    ))),
                false,
            ),
        )
    }

    fn take_statement_ids(&mut self, statement: &Statement<'a>) -> Vec<String> {
        let mut ids = self
            .targets
            .remove(&span_key(statement.span()))
            .unwrap_or_default();
        if let Statement::ExportNamedDeclaration(export) = statement
            && let Some(declaration) = &export.declaration
            && let Some(nested) = self.targets.remove(&span_key(declaration.span()))
        {
            ids.extend(nested);
        }
        ids
    }

    fn wrap_bare(&mut self, statement: &mut Statement<'a>) {
        let ids = self.take_statement_ids(statement);
        if ids.is_empty() {
            return;
        }
        let original = statement.take_in(self.ast.allocator);
        let mut body = self.ast.vec_with_capacity(ids.len() + 1);
        body.extend(ids.iter().map(|id| self.probe(id)));
        body.push(original);
        *statement = self.ast.statement_block(Span::default(), body);
    }
}

impl<'a> VisitMut<'a> for StatementProbeTransformer<'a> {
    fn visit_statements(&mut self, statements: &mut oxc_allocator::Vec<'a, Statement<'a>>) {
        let original = statements.take_in(self.ast.allocator);
        let mut instrumented = self.ast.vec_with_capacity(original.len() * 2);
        for mut statement in original {
            let ids = self.take_statement_ids(&statement);
            self.visit_statement(&mut statement);
            instrumented.extend(ids.iter().map(|id| self.probe(id)));
            instrumented.push(statement);
        }
        *statements = instrumented;
    }

    fn visit_function(&mut self, function: &mut Function<'a>, flags: ScopeFlags) {
        if self
            .source_sensitive_functions
            .contains(&span_key(function.span))
        {
            return;
        }
        walk_mut::walk_function(self, function, flags);
    }

    fn visit_arrow_function_expression(&mut self, function: &mut ArrowFunctionExpression<'a>) {
        if self
            .source_sensitive_functions
            .contains(&span_key(function.span))
        {
            return;
        }
        walk_mut::walk_arrow_function_expression(self, function);
    }

    fn visit_with_statement(&mut self, statement: &mut WithStatement<'a>) {
        if self.with_statements.contains(&span_key(statement.span)) {
            return;
        }
        walk_mut::walk_with_statement(self, statement);
    }

    fn visit_if_statement(&mut self, statement: &mut IfStatement<'a>) {
        self.wrap_bare(&mut statement.consequent);
        if let Some(alternate) = &mut statement.alternate {
            self.wrap_bare(alternate);
        }
        walk_mut::walk_if_statement(self, statement);
    }

    fn visit_while_statement(&mut self, statement: &mut WhileStatement<'a>) {
        self.wrap_bare(&mut statement.body);
        walk_mut::walk_while_statement(self, statement);
    }

    fn visit_do_while_statement(&mut self, statement: &mut DoWhileStatement<'a>) {
        self.wrap_bare(&mut statement.body);
        walk_mut::walk_do_while_statement(self, statement);
    }

    fn visit_for_statement(&mut self, statement: &mut ForStatement<'a>) {
        self.wrap_bare(&mut statement.body);
        walk_mut::walk_for_statement(self, statement);
    }

    fn visit_for_in_statement(&mut self, statement: &mut ForInStatement<'a>) {
        self.wrap_bare(&mut statement.body);
        walk_mut::walk_for_in_statement(self, statement);
    }

    fn visit_for_of_statement(&mut self, statement: &mut ForOfStatement<'a>) {
        self.wrap_bare(&mut statement.body);
        walk_mut::walk_for_of_statement(self, statement);
    }
}

struct FunctionProbeTransformer<'a> {
    ast: AstBuilder<'a>,
    coverage_hit: String,
    targets: HashMap<SpanKey, String>,
    source_sensitive_functions: HashSet<SpanKey>,
}

impl<'a> FunctionProbeTransformer<'a> {
    fn probe(&self, id: &str) -> Statement<'a> {
        self.ast.statement_expression(
            Span::default(),
            self.ast.expression_call(
                Span::default(),
                self.ast
                    .expression_identifier(Span::default(), self.ast.ident(&self.coverage_hit)),
                NONE,
                self.ast
                    .vec1(Argument::from(self.ast.expression_string_literal(
                        Span::default(),
                        self.ast.str(id),
                        None,
                    ))),
                false,
            ),
        )
    }
}

impl<'a> VisitMut<'a> for FunctionProbeTransformer<'a> {
    fn visit_function(&mut self, function: &mut Function<'a>, flags: ScopeFlags) {
        if self
            .source_sensitive_functions
            .contains(&span_key(function.span))
        {
            return;
        }
        if let Some(id) = self.targets.remove(&span_key(function.span))
            && let Some(body) = &mut function.body
        {
            body.statements.insert(0, self.probe(&id));
        }
        walk_mut::walk_function(self, function, flags);
    }

    fn visit_arrow_function_expression(&mut self, function: &mut ArrowFunctionExpression<'a>) {
        if self
            .source_sensitive_functions
            .contains(&span_key(function.span))
        {
            return;
        }
        if let Some(id) = self.targets.remove(&span_key(function.span)) {
            let probe = self.probe(&id);
            if function.expression {
                let original = function
                    .body
                    .statements
                    .pop()
                    .expect("expression arrow must contain its expression statement");
                let Statement::ExpressionStatement(expression) = original else {
                    panic!("expression arrow body must be represented as an expression statement");
                };
                function.expression = false;
                function.body.statements.push(probe);
                function.body.statements.push(
                    self.ast
                        .statement_return(Span::default(), Some(expression.unbox().expression)),
                );
            } else {
                function.body.statements.insert(0, probe);
            }
        }
        walk_mut::walk_arrow_function_expression(self, function);
    }
}

struct CandidateNames<'s> {
    source: &'s str,
    allocated: Vec<String>,
    suffix: usize,
}

impl<'s> CandidateNames<'s> {
    fn new(source: &'s str) -> Self {
        Self {
            source,
            allocated: Vec::new(),
            suffix: 0,
        }
    }

    fn allocate(&mut self, base: &str) -> String {
        loop {
            let candidate = if self.suffix == 0 {
                base.to_string()
            } else {
                format!("{base}{}", self.suffix)
            };
            self.suffix += 1;
            if !self.source.contains(&candidate) && !self.allocated.contains(&candidate) {
                self.allocated.push(candidate.clone());
                return candidate;
            }
        }
    }
}

struct ControlProbeV2Transformer<'a, 's> {
    ast: AstBuilder<'a>,
    file: &'s str,
    decisions: &'s [CandidateDecision],
    mcdc_begin: String,
    mcdc_condition: String,
    mcdc_end: String,
    mcdc_end_v2: String,
    names: CandidateNames<'s>,
    scope_declarations: Vec<Vec<String>>,
    decision_index: usize,
    source_sensitive_functions: HashSet<SpanKey>,
    with_statements: HashSet<SpanKey>,
}

#[derive(Clone, Copy)]
struct DecisionPlan {
    index: usize,
    condition_count: usize,
}

impl<'a> ControlProbeV2Transformer<'a, '_> {
    fn enter_declaration_scope(&mut self) {
        self.scope_declarations.push(Vec::new());
    }

    fn leave_declaration_scope(&mut self) -> Vec<String> {
        self.scope_declarations
            .pop()
            .expect("program/function scope stack must remain balanced")
    }

    fn allocate_scratch(&mut self, base: &str) -> String {
        let name = self.names.allocate(base);
        self.scope_declarations
            .last_mut()
            .expect("a control decision must be inside a program or function body")
            .push(name.clone());
        name
    }

    fn prepend_declarations(
        &self,
        names: Vec<String>,
        statements: &mut oxc_allocator::Vec<'a, Statement<'a>>,
    ) {
        if names.is_empty() {
            return;
        }
        let declarators = self.ast.vec_from_iter(names.into_iter().map(|name| {
            self.ast.variable_declarator(
                Span::default(),
                VariableDeclarationKind::Let,
                self.ast
                    .binding_pattern_binding_identifier(Span::default(), self.ast.ident(&name)),
                NONE,
                None,
                false,
            )
        }));
        statements.insert(
            0,
            Statement::VariableDeclaration(self.ast.alloc_variable_declaration(
                Span::default(),
                VariableDeclarationKind::Let,
                declarators,
                false,
            )),
        );
    }

    fn identifier(&self, name: &str) -> Expression<'a> {
        self.ast
            .expression_identifier(Span::default(), self.ast.ident(name))
    }

    fn assignment_target(&self, name: &str) -> AssignmentTarget<'a> {
        let target = self
            .ast
            .simple_assignment_target_assignment_target_identifier(
                Span::default(),
                self.ast.ident(name),
            );
        AssignmentTarget::from(target)
    }

    fn number(&self, value: u64) -> Expression<'a> {
        self.ast.expression_numeric_literal(
            Span::default(),
            value as f64,
            None,
            NumberBase::Decimal,
        )
    }

    fn string(&self, value: &str) -> Expression<'a> {
        self.ast
            .expression_string_literal(Span::default(), self.ast.str(value), None)
    }

    fn object_property(&self, name: &str, value: Expression<'a>) -> ObjectPropertyKind<'a> {
        self.ast.object_property_kind_object_property(
            Span::default(),
            PropertyKind::Init,
            self.ast
                .property_key_static_identifier(Span::default(), self.ast.ident(name)),
            value,
            false,
            false,
            false,
        )
    }

    fn decision_meta(&self, decision: &CandidateDecision) -> Expression<'a> {
        let conditions = self.ast.expression_array(
            Span::default(),
            self.ast.vec_from_iter(
                decision
                    .conditions
                    .iter()
                    .map(|condition| ArrayExpressionElement::from(self.string(condition))),
            ),
        );
        let properties = self.ast.vec_from_array([
            self.object_property("id", self.string(&decision.id)),
            self.object_property("file", self.string(&decision.file)),
            self.object_property("line", self.number(decision.line as u64)),
            self.object_property("column", self.number(decision.column as u64)),
            self.object_property("source", self.string(&decision.source)),
            self.object_property("conditions", conditions),
            self.object_property("kind", self.string(&decision.kind)),
        ]);
        Expression::ObjectExpression(
            self.ast
                .alloc_object_expression(Span::default(), properties),
        )
    }

    fn instrument_condition(
        &self,
        expression: Expression<'a>,
        frame_name: &str,
        temporary_name: &str,
        index: usize,
    ) -> Expression<'a> {
        let assign_value = self.ast.expression_assignment(
            Span::default(),
            AssignmentOperator::Assign,
            self.assignment_target(temporary_name),
            expression,
        );
        let weight = 3_u64.pow(index as u32);
        let digit = self.ast.expression_conditional(
            Span::default(),
            self.identifier(temporary_name),
            self.number(weight * 2),
            self.number(weight),
        );
        let add_digit = self.ast.expression_assignment(
            Span::default(),
            AssignmentOperator::Addition,
            self.assignment_target(frame_name),
            digit,
        );
        self.ast.expression_sequence(
            Span::default(),
            self.ast
                .vec_from_array([assign_value, add_digit, self.identifier(temporary_name)]),
        )
    }

    fn instrument_conditions(
        &self,
        expression: &mut Expression<'a>,
        frame_name: &str,
        temporary_names: &[String],
        next_index: &mut usize,
    ) {
        match expression {
            Expression::ParenthesizedExpression(parenthesized) => self.instrument_conditions(
                &mut parenthesized.expression,
                frame_name,
                temporary_names,
                next_index,
            ),
            Expression::LogicalExpression(logical)
                if matches!(logical.operator, LogicalOperator::And | LogicalOperator::Or) =>
            {
                self.instrument_conditions(
                    &mut logical.left,
                    frame_name,
                    temporary_names,
                    next_index,
                );
                self.instrument_conditions(
                    &mut logical.right,
                    frame_name,
                    temporary_names,
                    next_index,
                );
            }
            Expression::UnaryExpression(unary)
                if unary.operator.is_not() && has_compound_boolean_decision(&unary.argument) =>
            {
                self.instrument_conditions(
                    &mut unary.argument,
                    frame_name,
                    temporary_names,
                    next_index,
                );
            }
            _ => {
                let index = *next_index;
                *next_index += 1;
                let original = expression.take_in(self.ast.allocator);
                *expression =
                    self.instrument_condition(original, frame_name, &temporary_names[index], index);
            }
        }
    }

    fn instrument_condition_v1(
        &self,
        expression: Expression<'a>,
        frame_name: &str,
        index: usize,
    ) -> Expression<'a> {
        self.ast.expression_call(
            Span::default(),
            self.identifier(&self.mcdc_condition),
            NONE,
            self.ast.vec_from_array([
                Argument::from(self.identifier(frame_name)),
                Argument::from(self.number(index as u64)),
                Argument::from(expression),
            ]),
            false,
        )
    }

    fn instrument_conditions_v1(
        &self,
        expression: &mut Expression<'a>,
        frame_name: &str,
        next_index: &mut usize,
    ) {
        match expression {
            Expression::ParenthesizedExpression(parenthesized) => {
                self.instrument_conditions_v1(&mut parenthesized.expression, frame_name, next_index)
            }
            Expression::LogicalExpression(logical)
                if matches!(logical.operator, LogicalOperator::And | LogicalOperator::Or) =>
            {
                self.instrument_conditions_v1(&mut logical.left, frame_name, next_index);
                self.instrument_conditions_v1(&mut logical.right, frame_name, next_index);
            }
            Expression::UnaryExpression(unary)
                if unary.operator.is_not() && has_compound_boolean_decision(&unary.argument) =>
            {
                self.instrument_conditions_v1(&mut unary.argument, frame_name, next_index);
            }
            _ => {
                let index = *next_index;
                *next_index += 1;
                let original = expression.take_in(self.ast.allocator);
                *expression = self.instrument_condition_v1(original, frame_name, index);
            }
        }
    }

    fn reserve_decision(&mut self, test: &Expression<'a>) -> DecisionPlan {
        let mut condition_spans = Vec::new();
        collect_conditions(test, &mut condition_spans);
        let plan = DecisionPlan {
            index: self.decision_index,
            condition_count: condition_spans.len(),
        };
        self.decision_index += 1;
        plan
    }

    fn apply_decision(&mut self, test: &mut Expression<'a>, plan: DecisionPlan) {
        if plan.condition_count > 32 {
            let frame_name = self.allocate_scratch("_supercovMcdcFrame");
            let mut next_index = 0;
            self.instrument_conditions_v1(test, &frame_name, &mut next_index);
            debug_assert_eq!(next_index, plan.condition_count);

            let decision = &self.decisions[plan.index];
            let begin = self.ast.expression_call(
                Span::default(),
                self.identifier(&self.mcdc_begin),
                NONE,
                self.ast.vec_from_array([
                    Argument::from(self.string(&decision.id)),
                    Argument::from(self.decision_meta(decision)),
                ]),
                false,
            );
            let assign_frame = self.ast.expression_assignment(
                Span::default(),
                AssignmentOperator::Assign,
                self.assignment_target(&frame_name),
                begin,
            );
            let instrumented = test.take_in(self.ast.allocator);
            let end = self.ast.expression_call(
                Span::default(),
                self.identifier(&self.mcdc_end),
                NONE,
                self.ast.vec_from_array([
                    Argument::from(self.identifier(&frame_name)),
                    Argument::from(instrumented),
                ]),
                false,
            );
            *test = self.ast.expression_sequence(
                Span::default(),
                self.ast.vec_from_array([assign_frame, end]),
            );
            return;
        }

        let frame_name = self.allocate_scratch("_supercovMcdcFrame");
        let result_name = self.allocate_scratch("_supercovMcdcResult");
        let temporary_names = (0..plan.condition_count)
            .map(|_| self.allocate_scratch("_supercovMcdcValue"))
            .collect::<Vec<_>>();
        let mut next_index = 0;
        self.instrument_conditions(test, &frame_name, &temporary_names, &mut next_index);
        debug_assert_eq!(next_index, plan.condition_count);

        let original = test.take_in(self.ast.allocator);
        let assign_frame = self.ast.expression_assignment(
            Span::default(),
            AssignmentOperator::Assign,
            self.assignment_target(&frame_name),
            self.number(0),
        );
        let assign_result = self.ast.expression_assignment(
            Span::default(),
            AssignmentOperator::Assign,
            self.assignment_target(&result_name),
            original,
        );
        let arguments = self.ast.vec_from_array([
            Argument::from(self.ast.expression_string_literal(
                Span::default(),
                self.ast.str(self.file),
                None,
            )),
            Argument::from(self.number(plan.index as u64)),
            Argument::from(self.identifier(&frame_name)),
            Argument::from(self.identifier(&result_name)),
        ]);
        let record = self.ast.expression_call(
            Span::default(),
            self.identifier(&self.mcdc_end_v2),
            NONE,
            arguments,
            false,
        );
        *test = self.ast.expression_sequence(
            Span::default(),
            self.ast.vec_from_array([
                assign_frame,
                assign_result,
                record,
                self.identifier(&result_name),
            ]),
        );
    }
}

impl<'a> VisitMut<'a> for ControlProbeV2Transformer<'a, '_> {
    fn visit_program(&mut self, program: &mut Program<'a>) {
        self.enter_declaration_scope();
        walk_mut::walk_program(self, program);
        let declarations = self.leave_declaration_scope();
        self.prepend_declarations(declarations, &mut program.body);
    }

    fn visit_function_body(&mut self, body: &mut FunctionBody<'a>) {
        self.enter_declaration_scope();
        walk_mut::walk_function_body(self, body);
        let declarations = self.leave_declaration_scope();
        self.prepend_declarations(declarations, &mut body.statements);
    }

    fn visit_function(&mut self, function: &mut Function<'a>, flags: ScopeFlags) {
        if self
            .source_sensitive_functions
            .contains(&span_key(function.span))
        {
            return;
        }
        walk_mut::walk_function(self, function, flags);
    }

    fn visit_arrow_function_expression(&mut self, function: &mut ArrowFunctionExpression<'a>) {
        if self
            .source_sensitive_functions
            .contains(&span_key(function.span))
        {
            return;
        }
        walk_mut::walk_arrow_function_expression(self, function);
    }

    fn visit_with_statement(&mut self, statement: &mut WithStatement<'a>) {
        if self.with_statements.contains(&span_key(statement.span)) {
            return;
        }
        walk_mut::walk_with_statement(self, statement);
    }

    fn visit_if_statement(&mut self, statement: &mut IfStatement<'a>) {
        let plan = self.reserve_decision(&statement.test);
        self.visit_expression(&mut statement.test);
        self.apply_decision(&mut statement.test, plan);
        self.visit_statement(&mut statement.consequent);
        if let Some(alternate) = &mut statement.alternate {
            self.visit_statement(alternate);
        }
    }

    fn visit_conditional_expression(&mut self, expression: &mut ConditionalExpression<'a>) {
        let plan = self.reserve_decision(&expression.test);
        self.visit_expression(&mut expression.test);
        self.apply_decision(&mut expression.test, plan);
        self.visit_expression(&mut expression.consequent);
        self.visit_expression(&mut expression.alternate);
    }

    fn visit_while_statement(&mut self, statement: &mut WhileStatement<'a>) {
        let plan = self.reserve_decision(&statement.test);
        self.visit_expression(&mut statement.test);
        self.apply_decision(&mut statement.test, plan);
        self.visit_statement(&mut statement.body);
    }

    fn visit_do_while_statement(&mut self, statement: &mut DoWhileStatement<'a>) {
        let plan = self.reserve_decision(&statement.test);
        self.visit_statement(&mut statement.body);
        self.visit_expression(&mut statement.test);
        self.apply_decision(&mut statement.test, plan);
    }

    fn visit_for_statement(&mut self, statement: &mut ForStatement<'a>) {
        let plan = statement
            .test
            .as_ref()
            .map(|test| self.reserve_decision(test));
        if let Some(init) = &mut statement.init {
            self.visit_for_statement_init(init);
        }
        if let (Some(test), Some(plan)) = (&mut statement.test, plan) {
            self.visit_expression(test);
            self.apply_decision(test, plan);
        }
        if let Some(update) = &mut statement.update {
            self.visit_expression(update);
        }
        self.visit_statement(&mut statement.body);
    }
}

struct DecisionCollector<'s> {
    source: &'s str,
    file: &'s str,
    decisions: Vec<CandidateDecision>,
    source_sensitive_functions: &'s HashSet<SpanKey>,
    with_statements: &'s HashSet<SpanKey>,
}

impl DecisionCollector<'_> {
    fn record_decision(&mut self, test: &Expression<'_>, kind: &str) {
        let mut condition_spans = Vec::new();
        collect_conditions(test, &mut condition_spans);
        // Babel treats redundant parentheses as parser metadata rather than
        // part of the decision node. oxc preserves a ParenthesizedExpression,
        // so normalize it here to keep locations and stable IDs parser-neutral.
        let span = transparent_expression(test).span();
        let (line, column) = line_and_utf16_column(self.source, span.start as usize);
        self.decisions.push(CandidateDecision {
            id: stable_id(self.source, self.file, "decision", span, kind),
            file: self.file.to_string(),
            line,
            column,
            source: source_slice(self.source, span).to_string(),
            conditions: condition_spans
                .into_iter()
                .map(|condition| source_slice(self.source, condition).to_string())
                .collect(),
            kind: kind.to_string(),
        });
    }
}

impl<'a> Visit<'a> for DecisionCollector<'_> {
    fn visit_function(&mut self, function: &Function<'a>, flags: ScopeFlags) {
        if self
            .source_sensitive_functions
            .contains(&span_key(function.span))
        {
            return;
        }
        walk::walk_function(self, function, flags);
    }

    fn visit_arrow_function_expression(&mut self, function: &ArrowFunctionExpression<'a>) {
        if self
            .source_sensitive_functions
            .contains(&span_key(function.span))
        {
            return;
        }
        walk::walk_arrow_function_expression(self, function);
    }

    fn visit_with_statement(&mut self, statement: &WithStatement<'a>) {
        if self.with_statements.contains(&span_key(statement.span)) {
            return;
        }
        walk::walk_with_statement(self, statement);
    }

    fn visit_if_statement(&mut self, statement: &IfStatement<'a>) {
        self.record_decision(&statement.test, "if");
        self.visit_expression(&statement.test);
        self.visit_statement(&statement.consequent);
        if let Some(alternate) = &statement.alternate {
            self.visit_statement(alternate);
        }
    }

    fn visit_conditional_expression(&mut self, expression: &ConditionalExpression<'a>) {
        self.record_decision(&expression.test, "ternary");
        self.visit_expression(&expression.test);
        self.visit_expression(&expression.consequent);
        self.visit_expression(&expression.alternate);
    }

    fn visit_while_statement(&mut self, statement: &WhileStatement<'a>) {
        self.record_decision(&statement.test, "while");
        self.visit_expression(&statement.test);
        self.visit_statement(&statement.body);
    }

    fn visit_do_while_statement(&mut self, statement: &DoWhileStatement<'a>) {
        self.record_decision(&statement.test, "do-while");
        self.visit_statement(&statement.body);
        self.visit_expression(&statement.test);
    }

    fn visit_for_statement(&mut self, statement: &ForStatement<'a>) {
        if let Some(test) = &statement.test {
            self.record_decision(test, "for");
        }
        if let Some(init) = &statement.init {
            self.visit_for_statement_init(init);
        }
        if let Some(test) = &statement.test {
            self.visit_expression(test);
        }
        if let Some(update) = &statement.update {
            self.visit_expression(update);
        }
        self.visit_statement(&statement.body);
    }
}

fn transparent_expression<'a>(expression: &'a Expression<'a>) -> &'a Expression<'a> {
    match expression {
        Expression::ParenthesizedExpression(parenthesized) => {
            transparent_expression(&parenthesized.expression)
        }
        _ => expression,
    }
}

fn has_compound_boolean_decision(expression: &Expression<'_>) -> bool {
    match expression {
        Expression::ParenthesizedExpression(parenthesized) => {
            has_compound_boolean_decision(&parenthesized.expression)
        }
        Expression::LogicalExpression(logical) => {
            matches!(logical.operator, LogicalOperator::And | LogicalOperator::Or)
        }
        Expression::UnaryExpression(unary) if unary.operator.is_not() => {
            has_compound_boolean_decision(&unary.argument)
        }
        _ => false,
    }
}

fn collect_conditions(expression: &Expression<'_>, conditions: &mut Vec<Span>) {
    match expression {
        Expression::ParenthesizedExpression(parenthesized) => {
            collect_conditions(&parenthesized.expression, conditions);
        }
        Expression::LogicalExpression(logical)
            if matches!(logical.operator, LogicalOperator::And | LogicalOperator::Or) =>
        {
            collect_conditions(&logical.left, conditions);
            collect_conditions(&logical.right, conditions);
        }
        Expression::UnaryExpression(unary)
            if unary.operator.is_not() && has_compound_boolean_decision(&unary.argument) =>
        {
            collect_conditions(&unary.argument, conditions);
        }
        _ => conditions.push(expression.span()),
    }
}

fn source_slice(source: &str, span: Span) -> &str {
    &source[span.start as usize..span.end as usize]
}

fn line_and_utf16_column(source: &str, offset: usize) -> (usize, usize) {
    let prefix = &source[..offset];
    let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let column = source[line_start..offset].encode_utf16().count() + 1;
    (line, column)
}

fn stable_id(source: &str, file: &str, kind: &str, span: Span, suffix: &str) -> String {
    let start = source[..span.start as usize].encode_utf16().count();
    let end = source[..span.end as usize].encode_utf16().count();
    let digest = Sha256::digest(format!("{file}:{kind}:{start}:{end}:{suffix}").as_bytes());
    let mut id = String::with_capacity(16);
    for byte in &digest[..8] {
        write!(&mut id, "{byte:02x}").expect("writing to a String cannot fail");
    }
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str =
        "export function decide(a,b,c) {\n  if ((a && b) || !c) return 1;\n  return 0;\n}\n";

    #[test]
    fn matches_the_reference_if_decision_manifest_exactly() {
        let output = analyze_candidate(SOURCE, "app/decide.ts").unwrap();
        assert!(!output.complete);
        assert_eq!(
            output.decisions,
            vec![CandidateDecision {
                id: "2f65989a5782c5bd".to_string(),
                file: "app/decide.ts".to_string(),
                line: 2,
                column: 7,
                source: "(a && b) || !c".to_string(),
                conditions: vec!["a".to_string(), "b".to_string(), "!c".to_string()],
                kind: "if".to_string(),
            }]
        );
    }

    #[test]
    fn codegen_output_reparses_for_typescript_and_tsx() {
        for (file, source) in [
            (
                "component.tsx",
                "export const View = ({ok}: {ok: boolean}) => <div>{ok ? 'yes' : 'no'}</div>;",
            ),
            ("module.ts", SOURCE),
        ] {
            let output = analyze_candidate(source, file).unwrap();
            let allocator = Allocator::default();
            let source_type = SourceType::from_path(file).unwrap();
            let reparsed = Parser::new(&allocator, &output.code, source_type).parse();
            assert!(reparsed.errors.is_empty(), "{file}: {:?}", reparsed.errors);
        }
    }

    #[test]
    fn preserves_reference_order_across_every_control_decision_kind() {
        let source = "function run(a,b) {\n  const selected = a ? b : a;\n  while (a && b) break;\n  do { a = false; } while (a || b);\n  for (let i = 0; i < 1 && b; i++) work();\n  if (selected) return 1;\n  return 0;\n}";
        let output = instrument_candidate(source, "app/control.js").unwrap();
        assert_eq!(
            output
                .decisions
                .iter()
                .map(|decision| decision.kind.as_str())
                .collect::<Vec<_>>(),
            vec!["ternary", "while", "do-while", "for", "if"]
        );
    }

    #[test]
    fn inserts_probe_v2_without_claiming_the_unported_surfaces() {
        let output = instrument_candidate(SOURCE, "app/decide.ts").unwrap();
        assert!(!output.complete);
        assert_eq!(
            output.supported_surface,
            "point-control-decision-probe-candidate"
        );
        let runtime = output.runtime.expect("candidate runtime binding");
        assert!(output.code.contains(&runtime.mcdc_end_v2));
        assert!(output.code.contains("_supercovMcdcFrame"));
        assert!(output.code.contains("_supercovMcdcResult"));
        assert!(output.code.contains("+= _supercovMcdcValue"));
        assert_eq!(output.decisions.len(), 1);
        assert!(output.points.iter().any(|point| point.kind == "statement"));
        assert!(output.points.iter().any(|point| point.kind == "function"));

        let allocator = Allocator::default();
        let reparsed = Parser::new(
            &allocator,
            &output.code,
            SourceType::from_path("app/decide.ts").unwrap(),
        )
        .parse();
        assert!(reparsed.errors.is_empty(), "{:?}", reparsed.errors);
    }

    #[test]
    fn allocates_runtime_and_scratch_names_away_from_user_bindings() {
        let source = "const __supercovMcdcEndV2 = 1, _supercovMcdcFrame1 = 2;\nif (a && b) work();";
        let output = instrument_candidate(source, "app/collisions.js").unwrap();
        let runtime = output.runtime.expect("candidate runtime binding");
        assert_ne!(runtime.mcdc_end_v2, "__supercovMcdcEndV2");
        assert!(!output.code.contains("let _supercovMcdcFrame1,"));
    }

    #[test]
    fn instruments_wider_decisions_with_the_exact_v1_fallback() {
        let predicate = (0..33)
            .map(|index| format!("c{index}"))
            .collect::<Vec<_>>()
            .join(" && ");
        let source = format!("if ({predicate}) work();");
        let output = instrument_candidate(&source, "app/wide.js").unwrap();
        assert_eq!(output.decisions[0].conditions.len(), 33);
        let runtime = output.runtime.expect("candidate runtime binding");
        assert!(!output.code.contains(&format!("{}(", runtime.mcdc_end_v2)));
        assert!(output.code.contains(&format!("{}(", runtime.mcdc_begin)));
        assert!(
            output
                .code
                .contains(&format!("{}(", runtime.mcdc_condition))
        );
        assert!(output.code.contains(&format!("{}(", runtime.mcdc_end)));
    }

    #[test]
    fn parse_failures_are_explicit_and_never_claim_completeness() {
        assert!(matches!(
            analyze_candidate("if (", "broken.js"),
            Err(CandidateError::Parse(errors)) if !errors.is_empty()
        ));
        assert!(matches!(
            analyze_candidate("let value = 1", "unknown.extension"),
            Err(CandidateError::UnknownSourceType(_))
        ));
    }
}
