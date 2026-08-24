//! First oxc-backed vertical slice of the Rust JavaScript instrumenter.
//!
//! This candidate reports and instruments the complete frozen JavaScript
//! denominator, including semantic-safety boundaries and exact wide-decision
//! fallback. It remains private until its generated runtime import, evidence
//! transport, and attribution behavior match the reference engine.

use std::{
    collections::{HashMap, HashSet},
    fmt::Write,
    path::Path,
    sync::Arc,
};

use oxc_allocator::{Allocator, CloneIn, TakeIn};
use oxc_ast::{
    AstBuilder, NONE,
    ast::{
        Argument, ArrayExpressionElement, ArrowFunctionExpression, AssignmentExpression,
        AssignmentPattern, AssignmentTarget, BindingPattern, CallExpression, CatchClause,
        ChainElement, ChainExpression, Class, ComputedMemberExpression, ConditionalExpression,
        Declaration, DoWhileStatement, ExportDefaultDeclarationKind, Expression, ForInStatement,
        ForOfStatement, ForStatement, ForStatementLeft, FormalParameter, FormalParameterKind,
        FormalParameters, Function, FunctionBody, IfStatement, ImportOrExportKind,
        LogicalExpression, NewExpression, ObjectPropertyKind, PrivateFieldExpression, Program,
        PropertyKey, PropertyKind, Statement, StaticMemberExpression, SwitchStatement,
        TryStatement, VariableDeclaration, VariableDeclarationKind, WhileStatement, WithStatement,
    },
};
use oxc_ast_visit::{Visit, VisitMut, walk, walk_mut};
use oxc_codegen::{Codegen, CodegenOptions};
use oxc_parser::Parser;
use oxc_semantic::SemanticBuilder;
use oxc_span::{GetSpan, SourceType, Span};
use oxc_syntax::{
    number::NumberBase,
    operator::{AssignmentOperator, BinaryOperator, LogicalOperator, UnaryOperator},
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
pub struct CandidateBranchAlternative {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateBranch {
    pub id: String,
    pub kind: String,
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub source: String,
    pub alternatives: Vec<CandidateBranchAlternative>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateOutput {
    pub engine: String,
    pub complete: bool,
    pub supported_surface: String,
    pub code: String,
    pub map: Option<serde_json::Value>,
    pub decisions: Vec<CandidateDecision>,
    pub points: Vec<CandidatePoint>,
    pub branches: Vec<CandidateBranch>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<CandidateRuntime>,
    pub coverage_limitations: Vec<CandidateLimitation>,
    pub limitations: Vec<String>,
}

fn restore_comment_text(
    program: &Program<'_>,
    generated: &str,
    map: oxc_sourcemap::SourceMap,
) -> Result<(String, oxc_sourcemap::SourceMap), CandidateError> {
    if program.comments.is_empty() {
        return Ok((generated.to_string(), map));
    }
    let allocator = Allocator::default();
    let reparsed = Parser::new(&allocator, generated, program.source_type).parse();
    if !reparsed.errors.is_empty() {
        return Err(CandidateError::Parse(
            reparsed
                .errors
                .into_iter()
                .map(|error| error.to_string())
                .collect(),
        ));
    }
    let mut matched = vec![false; program.comments.len()];
    let mut original_index = 0;
    let mut edits = Vec::<(usize, usize, String)>::new();
    for emitted in &reparsed.program.comments {
        let emitted_text = emitted.span.source_text(generated);
        let (index, original) = loop {
            let Some(original) = program.comments.get(original_index) else {
                return Err(CandidateError::CommentPreservation {
                    expected: program.comments.len(),
                    actual: reparsed.program.comments.len(),
                });
            };
            let index = original_index;
            original_index += 1;
            if original.kind == emitted.kind
                && equal_ignoring_whitespace(
                    original.span.source_text(program.source_text),
                    emitted_text,
                )
            {
                break (index, original);
            }
        };
        matched[index] = true;
        edits.push((
            emitted.span.start as usize,
            emitted.span.end as usize,
            original.span.source_text(program.source_text).to_string(),
        ));
    }

    let source_lines = Utf16LineIndex::new(program.source_text);
    let generated_lines = Utf16LineIndex::new(generated);
    let mut mappings = map
        .get_tokens()
        .filter_map(|token| {
            token.get_source_id()?;
            Some((
                source_lines
                    .byte_offset(token.get_src_line() as usize, token.get_src_col() as usize),
                generated_lines
                    .byte_offset(token.get_dst_line() as usize, token.get_dst_col() as usize),
            ))
        })
        .collect::<Vec<_>>();
    mappings.sort_unstable_by_key(|(source, destination)| (*source, *destination));
    for (index, original) in program.comments.iter().enumerate() {
        if matched[index] {
            continue;
        }
        let anchor = if original.attached_to > 0 {
            original.attached_to as usize
        } else {
            original.span.end as usize
        };
        let mapping_index = mappings.partition_point(|(source, _)| *source < anchor);
        let destination = mappings
            .get(mapping_index)
            .map_or(generated.len(), |(_, destination)| *destination);
        let mut text = String::new();
        text.push(if original.preceded_by_newline() {
            '\n'
        } else {
            ' '
        });
        text.push_str(original.span.source_text(program.source_text));
        text.push(if original.is_line() || original.followed_by_newline() {
            '\n'
        } else {
            ' '
        });
        edits.push((destination, destination, text));
    }
    edits.sort_by_key(|(start, end, _)| (*start, *end));
    let restored_len = edits
        .iter()
        .fold(generated.len(), |length, (start, end, text)| {
            length + text.len() - (end - start)
        });
    let mut restored = String::with_capacity(restored_len);
    let mut cursor = 0;
    for (start, end, replacement) in &edits {
        if *start < cursor {
            return Err(CandidateError::CommentPreservation {
                expected: program.comments.len(),
                actual: reparsed.program.comments.len(),
            });
        }
        restored.push_str(&generated[cursor..*start]);
        restored.push_str(replacement);
        cursor = *end;
    }
    restored.push_str(&generated[cursor..]);
    let map = shift_source_map(map, generated, &restored, &edits);
    Ok((restored, map))
}

fn equal_ignoring_whitespace(left: &str, right: &str) -> bool {
    left.chars()
        .filter(|character| !character.is_whitespace())
        .eq(right.chars().filter(|character| !character.is_whitespace()))
}

struct Utf16LineIndex<'s> {
    source: &'s str,
    starts: Vec<usize>,
}

impl<'s> Utf16LineIndex<'s> {
    fn new(source: &'s str) -> Self {
        let mut starts = Vec::with_capacity(source.lines().count() + 1);
        starts.push(0);
        starts.extend(
            source
                .char_indices()
                .filter_map(|(offset, character)| (character == '\n').then_some(offset + 1)),
        );
        Self { source, starts }
    }

    fn byte_offset(&self, target_line: usize, target_utf16_col: usize) -> usize {
        let Some(&start) = self.starts.get(target_line) else {
            return self.source.len();
        };
        let end = self
            .starts
            .get(target_line + 1)
            .copied()
            .unwrap_or(self.source.len());
        start + utf16_col_to_byte(&self.source[start..end], target_utf16_col)
    }

    fn line_col(&self, byte_offset: usize) -> (u32, u32) {
        let byte_offset = byte_offset.min(self.source.len());
        let line = self.starts.partition_point(|start| *start <= byte_offset) - 1;
        let column = self.source[self.starts[line]..byte_offset]
            .chars()
            .map(char::len_utf16)
            .sum::<usize>();
        (line as u32, column as u32)
    }
}

fn utf16_col_to_byte(line: &str, target_utf16_col: usize) -> usize {
    let mut column = 0;
    for (offset, character) in line.char_indices() {
        if column >= target_utf16_col {
            return offset;
        }
        column += character.len_utf16();
    }
    line.len()
}

fn shift_source_map(
    map: oxc_sourcemap::SourceMap,
    generated: &str,
    restored: &str,
    edits: &[(usize, usize, String)],
) -> oxc_sourcemap::SourceMap {
    let generated_lines = Utf16LineIndex::new(generated);
    let restored_lines = Utf16LineIndex::new(restored);
    let mut edit_index = 0;
    let mut shift = 0isize;
    let tokens = map
        .get_tokens()
        .map(|token| {
            let original_offset = generated_lines
                .byte_offset(token.get_dst_line() as usize, token.get_dst_col() as usize);
            while let Some((start, end, replacement)) = edits.get(edit_index) {
                if *end > original_offset {
                    break;
                }
                shift += replacement.len() as isize - (*end - *start) as isize;
                edit_index += 1;
            }
            let shifted_offset = edits
                .get(edit_index)
                .filter(|(start, end, _)| *start <= original_offset && original_offset < *end)
                .map_or_else(
                    || original_offset.saturating_add_signed(shift),
                    |(start, _, _)| start.saturating_add_signed(shift),
                );
            let (dst_line, dst_col) = restored_lines.line_col(shifted_offset);
            oxc_sourcemap::Token::new(
                dst_line,
                dst_col,
                token.get_src_line(),
                token.get_src_col(),
                token.get_source_id(),
                token.get_name_id(),
            )
        })
        .collect::<Vec<_>>();
    let mut shifted = oxc_sourcemap::SourceMap::new(
        map.get_file().cloned(),
        map.get_names().cloned().collect::<Vec<Arc<str>>>(),
        map.get_source_root().map(str::to_string),
        map.get_sources().cloned().collect::<Vec<Arc<str>>>(),
        map.get_source_contents()
            .map(|content| content.cloned())
            .collect::<Vec<Option<Arc<str>>>>(),
        tokens.into_boxed_slice(),
        None,
    );
    if let Some(ignore_list) = map.get_x_google_ignore_list() {
        shifted.set_x_google_ignore_list(ignore_list.to_vec());
    }
    if let Some(debug_id) = map.get_debug_id() {
        shifted.set_debug_id(debug_id);
    }
    shifted
}

fn generate_candidate(
    program: &Program<'_>,
    file: &str,
) -> Result<(String, Option<serde_json::Value>), CandidateError> {
    let options = CodegenOptions {
        source_map_path: Some(Path::new(file).to_path_buf()),
        ..CodegenOptions::default()
    };
    let generated = Codegen::new().with_options(options).build(program);
    let (code, map) = restore_comment_text(
        program,
        &generated.code,
        generated.map.expect("source maps are enabled"),
    )?;
    let map = Some({
        serde_json::from_str(&map.to_json_string())
            .expect("oxc must serialize its own generated source map")
    });
    Ok((code, map))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateRuntime {
    pub coverage_hit: String,
    pub mcdc_begin: String,
    pub mcdc_condition: String,
    pub mcdc_end: String,
    pub register_probe_v2: String,
    pub mcdc_end_v2: String,
    pub coverage_hit_v2: String,
    pub probe_file_v2: String,
    pub selection_begin: String,
    pub selection_right: String,
    pub selection_end: String,
    pub with_request_phase: String,
    pub optional_select: String,
    pub optional_call_begin: String,
    pub optional_call_reached: String,
    pub optional_call_continued: String,
    pub optional_call_end: String,
    pub default_selected: String,
    pub default_entered: String,
    pub try_begin: String,
    pub try_catch: String,
    pub try_end: String,
    pub loop_begin: String,
    pub loop_entered: String,
    pub loop_end: String,
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
    CommentPreservation { expected: usize, actual: usize },
}

type SpanKey = (u32, u32);

#[derive(Default)]
struct SafetyAnalysis {
    source_sensitive_functions: HashSet<SpanKey>,
    with_statements: HashSet<SpanKey>,
    semantic_limitations: Vec<CandidateLimitation>,
    dynamic_limitations: Vec<CandidateLimitation>,
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

#[derive(Clone)]
struct PointTarget {
    index: usize,
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
    let mut semantic_limitations = scanner.function_limitations;
    semantic_limitations.extend(scanner.with_limitations);
    SafetyAnalysis {
        source_sensitive_functions: scanner.source_sensitive_functions,
        with_statements: scanner.with_statements,
        semantic_limitations,
        dynamic_limitations: scanner.dynamic_limitations,
    }
}

fn span_key(span: Span) -> SpanKey {
    (span.start, span.end)
}

fn expression_is_identifier(expression: &Expression<'_>, name: &str) -> bool {
    matches!(expression, Expression::Identifier(identifier) if identifier.name == name)
}

fn binding_identifier_name(pattern: &BindingPattern<'_>) -> Option<String> {
    match pattern {
        BindingPattern::BindingIdentifier(identifier) => Some(identifier.name.to_string()),
        _ => None,
    }
}

fn expression_is_anonymous_definition(expression: &Expression<'_>) -> bool {
    match expression {
        Expression::ArrowFunctionExpression(_) => true,
        Expression::FunctionExpression(function) => function.id.is_none(),
        Expression::ClassExpression(class) => class.id.is_none(),
        Expression::ParenthesizedExpression(expression) => {
            expression_is_anonymous_definition(&expression.expression)
        }
        Expression::TSAsExpression(expression) => {
            expression_is_anonymous_definition(&expression.expression)
        }
        Expression::TSSatisfiesExpression(expression) => {
            expression_is_anonymous_definition(&expression.expression)
        }
        Expression::TSTypeAssertion(expression) => {
            expression_is_anonymous_definition(&expression.expression)
        }
        Expression::TSNonNullExpression(expression) => {
            expression_is_anonymous_definition(&expression.expression)
        }
        _ => false,
    }
}

struct AssignmentNameSafetyTransformer<'a> {
    ast: AstBuilder<'a>,
}

impl<'a> VisitMut<'a> for AssignmentNameSafetyTransformer<'a> {
    fn visit_assignment_expression(&mut self, assignment: &mut AssignmentExpression<'a>) {
        walk_mut::walk_assignment_expression(self, assignment);
        let AssignmentTarget::AssignmentTargetIdentifier(identifier) = &assignment.left else {
            return;
        };
        if assignment.operator != AssignmentOperator::Assign
            || assignment.span.start == identifier.span.start
            || !expression_is_anonymous_definition(&assignment.right)
        {
            return;
        }
        let right = assignment.right.take_in(self.ast.allocator);
        assignment.right = self.ast.expression_sequence(
            Span::default(),
            self.ast.vec_from_array([
                self.ast.expression_numeric_literal(
                    Span::default(),
                    0.0,
                    None,
                    NumberBase::Decimal,
                ),
                right,
            ]),
        );
    }
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
        decision_vector_counts: Vec::new(),
        decision_logical_nodes: HashSet::new(),
        source_sensitive_functions: &safety.source_sensitive_functions,
        with_statements: &safety.with_statements,
    };
    collector.visit_program(&parsed.program);
    let optional_analysis = collect_optional_member_branches(
        &allocator,
        &mut parsed.program,
        source,
        file,
        &safety.source_sensitive_functions,
    );
    let call_analysis = collect_optional_call_branches(
        &allocator,
        &mut parsed.program,
        source,
        file,
        &safety.source_sensitive_functions,
    );
    let assignment_analysis = collect_logical_assignment_branches(
        &allocator,
        &mut parsed.program,
        source,
        file,
        &safety.source_sensitive_functions,
    );
    let default_analysis = collect_default_branches(
        &allocator,
        &mut parsed.program,
        source,
        file,
        &safety.source_sensitive_functions,
    );
    let extended_analysis = collect_extended_branches(
        &allocator,
        &mut parsed.program,
        source,
        file,
        &safety.source_sensitive_functions,
    );
    let logical_analysis = collect_logical_value_branches(
        &allocator,
        &mut parsed.program,
        source,
        file,
        &collector.decision_logical_nodes,
        &safety.source_sensitive_functions,
    );
    let switch_analysis = collect_switch_branches(
        &allocator,
        &mut parsed.program,
        source,
        file,
        &safety.source_sensitive_functions,
    );
    let mut branches = optional_analysis.branches;
    branches.extend(call_analysis.branches);
    branches.extend(assignment_analysis.branches);
    branches.extend(default_analysis.branches);
    branches.extend(extended_analysis.branches);
    branches.extend(logical_analysis.branches);
    branches.extend(switch_analysis.branches);
    let (generated, map) = generate_candidate(&parsed.program, file)?;
    Ok(CandidateOutput {
        engine: "rust-oxc".to_string(),
        complete: false,
        supported_surface: "control-decision-manifest-v1".to_string(),
        code: generated,
        map,
        decisions: collector.decisions,
        points: point_analysis.points,
        branches,
        runtime: None,
        coverage_limitations: {
            let mut limitations = safety.semantic_limitations;
            limitations.extend(call_analysis.limitations);
            limitations.extend(default_analysis.limitations);
            limitations.extend(safety.dynamic_limitations);
            limitations
        },
        limitations: vec![
            "candidate emits metadata only; use the private differential transform for probes"
                .to_string(),
            "coverage points, value branches, and extended branch obligations are not included"
                .to_string(),
        ],
    })
}

fn json_expression<'a>(ast: AstBuilder<'a>, value: &serde_json::Value) -> Expression<'a> {
    match value {
        serde_json::Value::Null => ast.expression_null_literal(Span::default()),
        serde_json::Value::Bool(value) => ast.expression_boolean_literal(Span::default(), *value),
        serde_json::Value::Number(value) => ast.expression_numeric_literal(
            Span::default(),
            value
                .as_f64()
                .expect("coverage registration numbers must fit JavaScript"),
            None,
            NumberBase::Decimal,
        ),
        serde_json::Value::String(value) => {
            ast.expression_string_literal(Span::default(), ast.str(value), None)
        }
        serde_json::Value::Array(values) => ast.expression_array(
            Span::default(),
            ast.vec_from_iter(
                values
                    .iter()
                    .map(|value| ArrayExpressionElement::from(json_expression(ast, value))),
            ),
        ),
        serde_json::Value::Object(properties) => ast.expression_object(
            Span::default(),
            ast.vec_from_iter(properties.iter().map(|(key, value)| {
                ast.object_property_kind_object_property(
                    Span::default(),
                    PropertyKind::Init,
                    ast.property_key_static_identifier(Span::default(), ast.ident(key)),
                    json_expression(ast, value),
                    false,
                    false,
                    false,
                )
            })),
        ),
    }
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
        decision_vector_counts: Vec::new(),
        decision_logical_nodes: HashSet::new(),
        source_sensitive_functions: &safety.source_sensitive_functions,
        with_statements: &safety.with_statements,
    };
    collector.visit_program(&parsed.program);
    let optional_analysis = collect_optional_member_branches(
        &allocator,
        &mut parsed.program,
        source,
        file,
        &safety.source_sensitive_functions,
    );
    let call_analysis = collect_optional_call_branches(
        &allocator,
        &mut parsed.program,
        source,
        file,
        &safety.source_sensitive_functions,
    );
    let assignment_analysis = collect_logical_assignment_branches(
        &allocator,
        &mut parsed.program,
        source,
        file,
        &safety.source_sensitive_functions,
    );
    let default_analysis = collect_default_branches(
        &allocator,
        &mut parsed.program,
        source,
        file,
        &safety.source_sensitive_functions,
    );
    let extended_analysis = collect_extended_branches(
        &allocator,
        &mut parsed.program,
        source,
        file,
        &safety.source_sensitive_functions,
    );
    let logical_analysis = collect_logical_value_branches(
        &allocator,
        &mut parsed.program,
        source,
        file,
        &collector.decision_logical_nodes,
        &safety.source_sensitive_functions,
    );
    let switch_analysis = collect_switch_branches(
        &allocator,
        &mut parsed.program,
        source,
        file,
        &safety.source_sensitive_functions,
    );
    let mut branches = optional_analysis.branches;
    branches.extend(call_analysis.branches.clone());
    branches.extend(assignment_analysis.branches);
    branches.extend(default_analysis.branches.clone());
    branches.extend(extended_analysis.branches.clone());
    branches.extend(logical_analysis.branches);
    branches.extend(switch_analysis.branches.clone());

    let mut names = CandidateNames::new(source);
    let mcdc_begin = names.allocate("__supercovMcdcBegin");
    let mcdc_condition = names.allocate("__supercovMcdcCondition");
    let mcdc_end = names.allocate("__supercovMcdcEnd");
    let coverage_hit = names.allocate("__supercovCoverageHit");
    let register_probe_v2 = names.allocate("__supercovRegisterProbeV2");
    let mcdc_end_v2 = names.allocate("__supercovMcdcEndV2");
    let coverage_hit_v2 = names.allocate("__supercovCoverageHitV2");
    let probe_file_v2 = names.allocate("__supercovProbeFileV2");
    let _probe_clock_v2 = names.allocate("__supercovProbeClockV2");
    let _probe_hits_v2 = names.allocate("__supercovProbeHitsV2");
    let _probe_decisions_v2 = names.allocate("__supercovProbeDecisionsV2");
    let _probe_complete_v2 = names.allocate("__supercovProbeCompleteV2");
    let selection_begin = names.allocate("__supercovSelectionBegin");
    let selection_right = names.allocate("__supercovSelectionRight");
    let selection_end = names.allocate("__supercovSelectionEnd");
    let with_request_phase = names.allocate("__supercovWithRequestPhase");
    let optional_select = names.allocate("__supercovOptionalSelect");
    let optional_call_begin = names.allocate("__supercovOptionalCallBegin");
    let optional_call_reached = names.allocate("__supercovOptionalCallReached");
    let optional_call_continued = names.allocate("__supercovOptionalCallContinued");
    let optional_call_end = names.allocate("__supercovOptionalCallEnd");
    let default_selected = names.allocate("__supercovDefaultSelected");
    let default_entered = names.allocate("__supercovDefaultEntered");
    let try_begin = names.allocate("__supercovTryBegin");
    let try_catch = names.allocate("__supercovTryCatch");
    let try_end = names.allocate("__supercovTryEnd");
    let loop_begin = names.allocate("__supercovLoopBegin");
    let loop_entered = names.allocate("__supercovLoopEntered");
    let loop_end = names.allocate("__supercovLoopEnd");
    let ast = AstBuilder::new(&allocator);
    let mut assignment_name_safety = AssignmentNameSafetyTransformer { ast };
    assignment_name_safety.visit_program(&mut parsed.program);
    let point_indices = point_analysis
        .points
        .iter()
        .enumerate()
        .map(|(index, point)| (point.id.clone(), index))
        .collect::<HashMap<_, _>>();
    let statement_targets = point_analysis
        .statement_targets
        .into_iter()
        .map(|(span, ids)| {
            (
                span,
                ids.into_iter()
                    .map(|id| PointTarget {
                        index: *point_indices
                            .get(&id)
                            .expect("statement point must have a global index"),
                    })
                    .collect(),
            )
        })
        .collect();
    let function_targets = point_analysis
        .function_targets
        .into_iter()
        .map(|(span, id)| {
            (
                span,
                PointTarget {
                    index: *point_indices
                        .get(&id)
                        .expect("function point must have a global index"),
                },
            )
        })
        .collect();
    let mut statement_transformer = StatementProbeTransformer {
        ast,
        coverage_hit_v2: coverage_hit_v2.clone(),
        probe_file_v2: probe_file_v2.clone(),
        targets: statement_targets,
        source_sensitive_functions: safety.source_sensitive_functions.clone(),
        with_statements: safety.with_statements.clone(),
    };
    statement_transformer.visit_program(&mut parsed.program);
    let mut function_transformer = FunctionProbeTransformer {
        ast,
        coverage_hit_v2: coverage_hit_v2.clone(),
        probe_file_v2: probe_file_v2.clone(),
        targets: function_targets,
        source_sensitive_functions: safety.source_sensitive_functions.clone(),
    };
    function_transformer.visit_program(&mut parsed.program);
    let mut optional_transformer = OptionalMemberTransformer {
        ast,
        optional_select: optional_select.clone(),
        targets: optional_analysis.targets,
        source_sensitive_functions: safety.source_sensitive_functions.clone(),
        with_statements: safety.with_statements.clone(),
    };
    optional_transformer.visit_program(&mut parsed.program);
    let mut call_transformer = OptionalCallTransformer::new(
        ast,
        source,
        optional_call_begin.clone(),
        optional_call_reached.clone(),
        optional_call_continued.clone(),
        optional_call_end.clone(),
        call_analysis.sites,
        call_analysis.roots,
        safety.source_sensitive_functions.clone(),
        safety.with_statements.clone(),
    );
    call_transformer.visit_program(&mut parsed.program);
    let mut default_transformer = DefaultTransformer {
        ast,
        default_selected: default_selected.clone(),
        default_entered: default_entered.clone(),
        parameter_targets: default_analysis.parameter_targets,
        binding_targets: default_analysis.binding_targets,
        function_entries: Vec::new(),
        declaration_entries: HashMap::new(),
        active_declaration: Vec::new(),
        parameter_pattern_depth: 0,
        source_sensitive_functions: safety.source_sensitive_functions.clone(),
        with_statements: safety.with_statements.clone(),
    };
    default_transformer.visit_program(&mut parsed.program);
    let mut extended_transformer = ExtendedTransformer {
        ast,
        try_begin: try_begin.clone(),
        try_catch: try_catch.clone(),
        try_end: try_end.clone(),
        loop_begin: loop_begin.clone(),
        loop_entered: loop_entered.clone(),
        loop_end: loop_end.clone(),
        try_targets: extended_analysis.try_targets,
        loop_targets: extended_analysis.loop_targets,
        names: CandidateNames::new(source),
        scope_declarations: Vec::new(),
        source_sensitive_functions: safety.source_sensitive_functions.clone(),
        with_statements: safety.with_statements.clone(),
    };
    extended_transformer.visit_program(&mut parsed.program);
    let mut transformer = ControlProbeV2Transformer {
        ast,
        decisions: &collector.decisions,
        mcdc_begin: mcdc_begin.clone(),
        mcdc_condition: mcdc_condition.clone(),
        mcdc_end: mcdc_end.clone(),
        mcdc_end_v2: mcdc_end_v2.clone(),
        probe_file_v2: probe_file_v2.clone(),
        names,
        scope_declarations: Vec::new(),
        decision_index: 0,
        parameter_depth: 0,
        source_sensitive_functions: safety.source_sensitive_functions.clone(),
        with_statements: safety.with_statements.clone(),
    };
    transformer.visit_program(&mut parsed.program);
    let mut logical_transformer = LogicalValueTransformer {
        ast,
        selection_begin: selection_begin.clone(),
        selection_right: selection_right.clone(),
        selection_end: selection_end.clone(),
        names: CandidateNames::new(source),
        scope_declarations: Vec::new(),
        logical_targets: logical_analysis.logical_targets,
        assignment_targets: assignment_analysis.targets,
        source_sensitive_functions: safety.source_sensitive_functions.clone(),
        with_statements: safety.with_statements.clone(),
    };
    logical_transformer.visit_program(&mut parsed.program);
    let mut switch_transformer = SwitchTransformer {
        ast,
        coverage_hit: coverage_hit.clone(),
        targets: switch_analysis.targets,
        names: CandidateNames::new(source),
        source_sensitive_functions: safety.source_sensitive_functions.clone(),
        with_statements: safety.with_statements.clone(),
    };
    switch_transformer.visit_program(&mut parsed.program);
    let mut route_transformer = RouteRequestPhaseTransformer {
        ast,
        file,
        with_request_phase: with_request_phase.clone(),
        used: false,
        names: CandidateNames::new(source),
    };
    route_transformer.transform_program(&mut parsed.program);
    let mut request_transformer = RequestPhaseTransformer {
        ast,
        with_request_phase: with_request_phase.clone(),
        used: route_transformer.used,
        source_sensitive_functions: safety.source_sensitive_functions.clone(),
        with_statements: safety.with_statements.clone(),
    };
    request_transformer.visit_program(&mut parsed.program);
    let uses_request_phase = request_transformer.used;

    let registration = serde_json::json!({
        "decisions": &collector.decisions,
        "pointIds": point_analysis.points.iter().map(|point| &point.id).collect::<Vec<_>>(),
        "decisionVectorCounts": &collector.decision_vector_counts,
    });
    let registration_call = ast.expression_call(
        Span::default(),
        ast.expression_identifier(Span::default(), ast.ident(&register_probe_v2)),
        NONE,
        ast.vec1(Argument::from(json_expression(ast, &registration))),
        false,
    );
    parsed.program.body.insert(
        0,
        Statement::VariableDeclaration(ast.alloc_variable_declaration(
            Span::default(),
            VariableDeclarationKind::Const,
            ast.vec1(ast.variable_declarator(
                Span::default(),
                VariableDeclarationKind::Const,
                ast.binding_pattern_binding_identifier(Span::default(), ast.ident(&probe_file_v2)),
                NONE,
                Some(registration_call),
                false,
            )),
            false,
        )),
    );
    let mut runtime_imports = vec![
        ("mcdcBegin", &mcdc_begin),
        ("mcdcCondition", &mcdc_condition),
        ("mcdcEnd", &mcdc_end),
        ("coverageHit", &coverage_hit),
        ("registerProbeV2", &register_probe_v2),
        ("mcdcEndV2", &mcdc_end_v2),
        ("coverageHitV2", &coverage_hit_v2),
        ("selectionBegin", &selection_begin),
        ("selectionRight", &selection_right),
        ("selectionEnd", &selection_end),
        ("optionalSelect", &optional_select),
        ("optionalCallBegin", &optional_call_begin),
        ("optionalCallReached", &optional_call_reached),
        ("optionalCallContinued", &optional_call_continued),
        ("optionalCallEnd", &optional_call_end),
        ("defaultSelected", &default_selected),
        ("defaultEntered", &default_entered),
        ("tryBegin", &try_begin),
        ("tryCatch", &try_catch),
        ("tryEnd", &try_end),
        ("loopBegin", &loop_begin),
        ("loopEntered", &loop_entered),
        ("loopEnd", &loop_end),
    ];
    if uses_request_phase {
        runtime_imports.insert(10, ("withRequestPhase", &with_request_phase));
    }
    if parsed.program.source_type.is_script() {
        let declarators =
            ast.vec_from_iter(runtime_imports.into_iter().map(|(imported, local)| {
                let global_runtime =
                    Expression::StaticMemberExpression(ast.alloc_static_member_expression(
                        Span::default(),
                        ast.expression_identifier(Span::default(), ast.ident("globalThis")),
                        ast.identifier_name(Span::default(), ast.ident("__supercovRuntime")),
                        false,
                    ));
                let runtime_helper =
                    Expression::StaticMemberExpression(ast.alloc_static_member_expression(
                        Span::default(),
                        global_runtime,
                        ast.identifier_name(Span::default(), ast.ident(imported)),
                        false,
                    ));
                ast.variable_declarator(
                    Span::default(),
                    VariableDeclarationKind::Const,
                    ast.binding_pattern_binding_identifier(Span::default(), ast.ident(local)),
                    NONE,
                    Some(runtime_helper),
                    false,
                )
            }));
        parsed.program.body.insert(
            0,
            Statement::VariableDeclaration(ast.alloc_variable_declaration(
                Span::default(),
                VariableDeclarationKind::Const,
                declarators,
                false,
            )),
        );
    } else {
        let import_specifiers =
            ast.vec_from_iter(runtime_imports.into_iter().map(|(imported, local)| {
                ast.import_declaration_specifier_import_specifier(
                    Span::default(),
                    ast.module_export_name_identifier_name(Span::default(), ast.ident(imported)),
                    ast.binding_identifier(Span::default(), ast.ident(local)),
                    oxc_ast::ast::ImportOrExportKind::Value,
                )
            }));
        parsed.program.body.insert(
            0,
            Statement::ImportDeclaration(ast.alloc_import_declaration(
                Span::default(),
                Some(import_specifiers),
                ast.string_literal(Span::default(), ast.str("virtual:supercov-runtime"), None),
                None,
                NONE,
                oxc_ast::ast::ImportOrExportKind::Value,
            )),
        );
    }

    let limitations = vec![
        "production evidence archive parity remains to be proven against the TypeScript reference"
            .to_string(),
        "candidate runtime registration is differential-only and is not exposed by the public CLI"
            .to_string(),
    ];
    let (code, map) = generate_candidate(&parsed.program, file)?;
    Ok(CandidateOutput {
        engine: "rust-oxc".to_string(),
        complete: false,
        supported_surface: "complete-js-manifest-and-differential-probes-candidate".to_string(),
        code,
        map,
        decisions: collector.decisions,
        points: point_analysis.points,
        branches,
        runtime: Some(CandidateRuntime {
            coverage_hit,
            mcdc_begin,
            mcdc_condition,
            mcdc_end,
            register_probe_v2,
            mcdc_end_v2,
            coverage_hit_v2,
            probe_file_v2,
            selection_begin,
            selection_right,
            selection_end,
            with_request_phase,
            optional_select,
            optional_call_begin,
            optional_call_reached,
            optional_call_continued,
            optional_call_end,
            default_selected,
            default_entered,
            try_begin,
            try_catch,
            try_end,
            loop_begin,
            loop_entered,
            loop_end,
        }),
        coverage_limitations: {
            let mut limitations = safety.semantic_limitations;
            limitations.extend(call_analysis.limitations);
            limitations.extend(default_analysis.limitations);
            limitations.extend(safety.dynamic_limitations);
            limitations
        },
        limitations,
    })
}

struct StatementProbeTransformer<'a> {
    ast: AstBuilder<'a>,
    coverage_hit_v2: String,
    probe_file_v2: String,
    targets: HashMap<SpanKey, Vec<PointTarget>>,
    source_sensitive_functions: HashSet<SpanKey>,
    with_statements: HashSet<SpanKey>,
}

impl<'a> StatementProbeTransformer<'a> {
    fn probe(&self, target: &PointTarget) -> Statement<'a> {
        self.ast.statement_expression(
            Span::default(),
            self.ast.expression_call(
                Span::default(),
                self.ast
                    .expression_identifier(Span::default(), self.ast.ident(&self.coverage_hit_v2)),
                NONE,
                self.ast.vec_from_array([
                    Argument::from(self.ast.expression_identifier(
                        Span::default(),
                        self.ast.ident(&self.probe_file_v2),
                    )),
                    Argument::from(self.ast.expression_numeric_literal(
                        Span::default(),
                        target.index as f64,
                        None,
                        NumberBase::Decimal,
                    )),
                ]),
                false,
            ),
        )
    }

    fn take_statement_ids(&mut self, statement: &Statement<'a>) -> Vec<PointTarget> {
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
        body.extend(ids.iter().map(|target| self.probe(target)));
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
            instrumented.extend(ids.iter().map(|target| self.probe(target)));
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
    coverage_hit_v2: String,
    probe_file_v2: String,
    targets: HashMap<SpanKey, PointTarget>,
    source_sensitive_functions: HashSet<SpanKey>,
}

impl<'a> FunctionProbeTransformer<'a> {
    fn probe(&self, target: &PointTarget) -> Statement<'a> {
        self.ast.statement_expression(
            Span::default(),
            self.ast.expression_call(
                Span::default(),
                self.ast
                    .expression_identifier(Span::default(), self.ast.ident(&self.coverage_hit_v2)),
                NONE,
                self.ast.vec_from_array([
                    Argument::from(self.ast.expression_identifier(
                        Span::default(),
                        self.ast.ident(&self.probe_file_v2),
                    )),
                    Argument::from(self.ast.expression_numeric_literal(
                        Span::default(),
                        target.index as f64,
                        None,
                        NumberBase::Decimal,
                    )),
                ]),
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
        if let Some(target) = self.targets.remove(&span_key(function.span))
            && let Some(body) = &mut function.body
        {
            body.statements.insert(0, self.probe(&target));
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
        if let Some(target) = self.targets.remove(&span_key(function.span)) {
            let probe = self.probe(&target);
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

struct OptionalMemberTransformer<'a> {
    ast: AstBuilder<'a>,
    optional_select: String,
    targets: HashMap<SpanKey, (String, String)>,
    source_sensitive_functions: HashSet<SpanKey>,
    with_statements: HashSet<SpanKey>,
}

impl<'a> OptionalMemberTransformer<'a> {
    fn instrument_operand(
        &self,
        operand: Expression<'a>,
        short_id: &str,
        continued_id: &str,
    ) -> Expression<'a> {
        self.ast.expression_call(
            Span::default(),
            self.ast
                .expression_identifier(Span::default(), self.ast.ident(&self.optional_select)),
            NONE,
            self.ast.vec_from_array([
                Argument::from(self.ast.expression_string_literal(
                    Span::default(),
                    self.ast.str(short_id),
                    None,
                )),
                Argument::from(self.ast.expression_string_literal(
                    Span::default(),
                    self.ast.str(continued_id),
                    None,
                )),
                Argument::from(operand),
            ]),
            false,
        )
    }

    fn instrument_target(&mut self, span: Span, object: &mut Expression<'a>) {
        let Some((short_id, continued_id)) = self.targets.remove(&span_key(span)) else {
            return;
        };
        let operand = object.take_in(self.ast.allocator);
        *object = self.instrument_operand(operand, &short_id, &continued_id);
    }
}

impl<'a> VisitMut<'a> for OptionalMemberTransformer<'a> {
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

    fn visit_computed_member_expression(&mut self, member: &mut ComputedMemberExpression<'a>) {
        self.instrument_target(member.span, &mut member.object);
        walk_mut::walk_computed_member_expression(self, member);
    }

    fn visit_static_member_expression(&mut self, member: &mut StaticMemberExpression<'a>) {
        self.instrument_target(member.span, &mut member.object);
        walk_mut::walk_static_member_expression(self, member);
    }

    fn visit_private_field_expression(&mut self, member: &mut PrivateFieldExpression<'a>) {
        self.instrument_target(member.span, &mut member.object);
        walk_mut::walk_private_field_expression(self, member);
    }
}

#[derive(Clone)]
struct OptionalCallSiteRuntime {
    frame: String,
    short_id: String,
    continued_id: String,
}

struct OptionalCallTransformer<'a, 's> {
    ast: AstBuilder<'a>,
    optional_call_begin: String,
    optional_call_reached: String,
    optional_call_continued: String,
    optional_call_end: String,
    scope_declarations: Vec<Vec<String>>,
    sites: HashMap<SpanKey, OptionalCallSiteRuntime>,
    roots: HashMap<SpanKey, Vec<SpanKey>>,
    source_sensitive_functions: HashSet<SpanKey>,
    with_statements: HashSet<SpanKey>,
    _source: std::marker::PhantomData<&'s str>,
}

impl<'a, 's> OptionalCallTransformer<'a, 's> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        ast: AstBuilder<'a>,
        source: &'s str,
        optional_call_begin: String,
        optional_call_reached: String,
        optional_call_continued: String,
        optional_call_end: String,
        sites: HashMap<SpanKey, (String, String)>,
        roots: HashMap<SpanKey, Vec<SpanKey>>,
        source_sensitive_functions: HashSet<SpanKey>,
        with_statements: HashSet<SpanKey>,
    ) -> Self {
        let mut names = CandidateNames::new(source);
        let sites = sites
            .into_iter()
            .map(|(key, (short_id, continued_id))| {
                (
                    key,
                    OptionalCallSiteRuntime {
                        frame: names.allocate("_optionalCall"),
                        short_id,
                        continued_id,
                    },
                )
            })
            .collect();
        Self {
            ast,
            optional_call_begin,
            optional_call_reached,
            optional_call_continued,
            optional_call_end,
            scope_declarations: Vec::new(),
            sites,
            roots,
            source_sensitive_functions,
            with_statements,
            _source: std::marker::PhantomData,
        }
    }

    fn identifier(&self, name: &str) -> Expression<'a> {
        self.ast
            .expression_identifier(Span::default(), self.ast.ident(name))
    }

    fn assignment_target(&self, name: &str) -> AssignmentTarget<'a> {
        AssignmentTarget::from(
            self.ast
                .simple_assignment_target_assignment_target_identifier(
                    Span::default(),
                    self.ast.ident(name),
                ),
        )
    }

    fn call(&self, name: &str, arguments: oxc_allocator::Vec<'a, Argument<'a>>) -> Expression<'a> {
        self.ast.expression_call(
            Span::default(),
            self.identifier(name),
            NONE,
            arguments,
            false,
        )
    }

    fn string_argument(&self, value: &str) -> Argument<'a> {
        Argument::from(self.ast.expression_string_literal(
            Span::default(),
            self.ast.str(value),
            None,
        ))
    }

    fn reached(&self, frame: &str, value: Expression<'a>) -> Expression<'a> {
        self.call(
            &self.optional_call_reached,
            self.ast.vec_from_array([
                Argument::from(self.identifier(frame)),
                Argument::from(value),
            ]),
        )
    }

    fn enter_scope(&mut self) {
        self.scope_declarations.push(Vec::new());
    }

    fn leave_scope(&mut self, statements: &mut oxc_allocator::Vec<'a, Statement<'a>>) {
        let names = self
            .scope_declarations
            .pop()
            .expect("optional-call scope stack must remain balanced");
        if names.is_empty() {
            return;
        }
        let declarations = self.ast.vec_from_iter(names.into_iter().map(|name| {
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
                declarations,
                false,
            )),
        );
    }

    fn instrument_callee(
        &self,
        callee: Expression<'a>,
        site: &OptionalCallSiteRuntime,
    ) -> Expression<'a> {
        match callee {
            Expression::ComputedMemberExpression(mut member) => {
                let property = member.expression.take_in(self.ast.allocator);
                member.expression = self.reached(&site.frame, property);
                Expression::ComputedMemberExpression(member)
            }
            Expression::StaticMemberExpression(member) => {
                let member = member.unbox();
                let property = self.ast.expression_string_literal(
                    Span::default(),
                    self.ast.str(member.property.name.as_str()),
                    None,
                );
                Expression::ComputedMemberExpression(self.ast.alloc_computed_member_expression(
                    member.span,
                    member.object,
                    self.reached(&site.frame, property),
                    member.optional,
                ))
            }
            Expression::PrivateFieldExpression(mut member) => {
                let object = member.object.take_in(self.ast.allocator);
                member.object = self.reached(&site.frame, object);
                Expression::PrivateFieldExpression(member)
            }
            Expression::ChainExpression(mut chain) => {
                match &mut chain.expression {
                    ChainElement::ComputedMemberExpression(member) => {
                        let property = member.expression.take_in(self.ast.allocator);
                        member.expression = self.reached(&site.frame, property);
                    }
                    ChainElement::StaticMemberExpression(member) => {
                        let member = member.take_in(self.ast.allocator);
                        let property = self.ast.expression_string_literal(
                            Span::default(),
                            self.ast.str(member.property.name.as_str()),
                            None,
                        );
                        chain.expression = ChainElement::ComputedMemberExpression(
                            self.ast.alloc_computed_member_expression(
                                member.span,
                                member.object,
                                self.reached(&site.frame, property),
                                member.optional,
                            ),
                        );
                    }
                    ChainElement::PrivateFieldExpression(member) => {
                        let object = member.object.take_in(self.ast.allocator);
                        member.object = self.reached(&site.frame, object);
                    }
                    ChainElement::CallExpression(_) | ChainElement::TSNonNullExpression(_) => {
                        return self.reached(&site.frame, Expression::ChainExpression(chain));
                    }
                }
                Expression::ChainExpression(chain)
            }
            Expression::ParenthesizedExpression(mut parenthesized) => {
                let inner = parenthesized.expression.take_in(self.ast.allocator);
                parenthesized.expression = self.instrument_callee(inner, site);
                Expression::ParenthesizedExpression(parenthesized)
            }
            Expression::TSAsExpression(mut wrapped) => {
                let inner = wrapped.expression.take_in(self.ast.allocator);
                wrapped.expression = self.instrument_callee(inner, site);
                Expression::TSAsExpression(wrapped)
            }
            Expression::TSSatisfiesExpression(mut wrapped) => {
                let inner = wrapped.expression.take_in(self.ast.allocator);
                wrapped.expression = self.instrument_callee(inner, site);
                Expression::TSSatisfiesExpression(wrapped)
            }
            Expression::TSTypeAssertion(mut wrapped) => {
                let inner = wrapped.expression.take_in(self.ast.allocator);
                wrapped.expression = self.instrument_callee(inner, site);
                Expression::TSTypeAssertion(wrapped)
            }
            Expression::TSNonNullExpression(mut wrapped) => {
                let inner = wrapped.expression.take_in(self.ast.allocator);
                wrapped.expression = self.instrument_callee(inner, site);
                Expression::TSNonNullExpression(wrapped)
            }
            other => self.reached(&site.frame, other),
        }
    }

    fn instrument_call(&self, call: &mut CallExpression<'a>, site: &OptionalCallSiteRuntime) {
        let callee = call.callee.take_in(self.ast.allocator);
        call.callee = self.instrument_callee(callee, site);
        let continued = self.call(
            &self.optional_call_continued,
            self.ast.vec1(Argument::from(self.identifier(&site.frame))),
        );
        call.arguments.insert(
            0,
            Argument::SpreadElement(self.ast.alloc_spread_element(Span::default(), continued)),
        );
    }

    fn wrap_root(&mut self, expression: &mut Expression<'a>, site_keys: &[SpanKey]) {
        let sites = site_keys
            .iter()
            .map(|key| {
                self.sites
                    .get(key)
                    .expect("optional-call root must reference a known site")
                    .clone()
            })
            .collect::<Vec<_>>();
        self.scope_declarations
            .last_mut()
            .expect("optional-call root must be inside a program or function")
            .extend(sites.iter().map(|site| site.frame.clone()));

        let original = expression.take_in(self.ast.allocator);
        let mut measured = original;
        for site in sites.iter().rev() {
            measured = self.call(
                &self.optional_call_end,
                self.ast.vec_from_array([
                    Argument::from(self.identifier(&site.frame)),
                    Argument::from(measured),
                ]),
            );
        }
        let mut sequence = self.ast.vec_with_capacity(sites.len() + 1);
        for site in &sites {
            let begin = self.call(
                &self.optional_call_begin,
                self.ast.vec_from_array([
                    self.string_argument(&site.short_id),
                    self.string_argument(&site.continued_id),
                ]),
            );
            sequence.push(self.ast.expression_assignment(
                Span::default(),
                AssignmentOperator::Assign,
                self.assignment_target(&site.frame),
                begin,
            ));
        }
        sequence.push(measured);
        *expression = self.ast.expression_sequence(Span::default(), sequence);
    }
}

impl<'a> VisitMut<'a> for OptionalCallTransformer<'a, '_> {
    fn visit_program(&mut self, program: &mut Program<'a>) {
        self.enter_scope();
        walk_mut::walk_program(self, program);
        self.leave_scope(&mut program.body);
    }

    fn visit_function_body(&mut self, body: &mut FunctionBody<'a>) {
        self.enter_scope();
        walk_mut::walk_function_body(self, body);
        self.leave_scope(&mut body.statements);
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

    fn visit_call_expression(&mut self, call: &mut CallExpression<'a>) {
        walk_mut::walk_call_expression(self, call);
        if let Some(site) = self.sites.get(&span_key(call.span)).cloned() {
            self.instrument_call(call, &site);
        }
    }

    fn visit_expression(&mut self, expression: &mut Expression<'a>) {
        let key = span_key(expression.span());
        walk_mut::walk_expression(self, expression);
        if let Some(site_keys) = self.roots.remove(&key) {
            self.wrap_root(expression, &site_keys);
        }
    }
}

struct DefaultTransformer<'a> {
    ast: AstBuilder<'a>,
    default_selected: String,
    default_entered: String,
    parameter_targets: HashMap<SpanKey, DefaultTarget>,
    binding_targets: HashMap<SpanKey, DefaultTarget>,
    function_entries: Vec<Vec<Statement<'a>>>,
    declaration_entries: HashMap<SpanKey, Vec<Statement<'a>>>,
    active_declaration: Vec<SpanKey>,
    parameter_pattern_depth: usize,
    source_sensitive_functions: HashSet<SpanKey>,
    with_statements: HashSet<SpanKey>,
}

impl<'a> DefaultTransformer<'a> {
    fn identifier(&self, name: &str) -> Expression<'a> {
        self.ast
            .expression_identifier(Span::default(), self.ast.ident(name))
    }

    fn string_argument(&self, value: &str) -> Argument<'a> {
        Argument::from(self.ast.expression_string_literal(
            Span::default(),
            self.ast.str(value),
            None,
        ))
    }

    fn selected(&self, value: Expression<'a>, target: &DefaultTarget) -> Expression<'a> {
        let mut arguments = self.ast.vec_from_array([
            self.string_argument(&target.default_id),
            Argument::from(value),
        ]);
        if let Some(name) = &target.inferred_name {
            arguments.push(self.string_argument(name));
        }
        self.ast.expression_call(
            Span::default(),
            self.identifier(&self.default_selected),
            NONE,
            arguments,
            false,
        )
    }

    fn entered(&self, target: &DefaultTarget) -> Statement<'a> {
        self.ast.statement_expression(
            Span::default(),
            self.ast.expression_call(
                Span::default(),
                self.identifier(&self.default_entered),
                NONE,
                self.ast.vec_from_array([
                    self.string_argument(&target.default_id),
                    self.string_argument(&target.provided_id),
                ]),
                false,
            ),
        )
    }

    fn push_entry(&mut self, target: &DefaultTarget) {
        let entry = self.entered(target);
        if self.parameter_pattern_depth > 0 {
            self.function_entries
                .last_mut()
                .expect("parameter default must belong to a function")
                .push(entry);
        } else {
            let declaration = *self
                .active_declaration
                .last()
                .expect("binding default must belong to a declaration");
            self.declaration_entries
                .entry(declaration)
                .or_default()
                .push(entry);
        }
    }

    fn prepend_entries(&self, body: &mut Statement<'a>, entries: Vec<Statement<'a>>) {
        if entries.is_empty() {
            return;
        }
        if let Statement::BlockStatement(block) = body {
            for (index, entry) in entries.into_iter().enumerate() {
                block.body.insert(index, entry);
            }
            return;
        }
        let original = body.take_in(self.ast.allocator);
        let mut statements = self.ast.vec_with_capacity(entries.len() + 1);
        statements.extend(entries);
        statements.push(original);
        *body = self.ast.statement_block(Span::default(), statements);
    }

    fn variable_span(statement: &Statement<'a>) -> Option<SpanKey> {
        match statement {
            Statement::VariableDeclaration(declaration) => Some(span_key(declaration.span)),
            Statement::ExportNamedDeclaration(export) => match &export.declaration {
                Some(Declaration::VariableDeclaration(declaration)) => {
                    Some(span_key(declaration.span))
                }
                _ => None,
            },
            _ => None,
        }
    }

    fn loop_declaration_span(left: &ForStatementLeft<'a>) -> Option<SpanKey> {
        match left {
            ForStatementLeft::VariableDeclaration(declaration) => Some(span_key(declaration.span)),
            _ => None,
        }
    }
}

impl<'a> VisitMut<'a> for DefaultTransformer<'a> {
    fn visit_statements(&mut self, statements: &mut oxc_allocator::Vec<'a, Statement<'a>>) {
        let original = statements.take_in(self.ast.allocator);
        let mut instrumented = self.ast.vec_with_capacity(original.len() * 2);
        for mut statement in original {
            let declaration = Self::variable_span(&statement);
            self.visit_statement(&mut statement);
            instrumented.push(statement);
            if let Some(declaration) = declaration
                && let Some(entries) = self.declaration_entries.remove(&declaration)
            {
                instrumented.extend(entries);
            }
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
        self.function_entries.push(Vec::new());
        walk_mut::walk_function(self, function, flags);
        let entries = self
            .function_entries
            .pop()
            .expect("default function-entry stack must remain balanced");
        if let Some(body) = &mut function.body {
            for (index, entry) in entries.into_iter().enumerate() {
                body.statements.insert(index, entry);
            }
        }
    }

    fn visit_arrow_function_expression(&mut self, function: &mut ArrowFunctionExpression<'a>) {
        if self
            .source_sensitive_functions
            .contains(&span_key(function.span))
        {
            return;
        }
        self.function_entries.push(Vec::new());
        walk_mut::walk_arrow_function_expression(self, function);
        let entries = self
            .function_entries
            .pop()
            .expect("default arrow-entry stack must remain balanced");
        for (index, entry) in entries.into_iter().enumerate() {
            function.body.statements.insert(index, entry);
        }
    }

    fn visit_formal_parameter(&mut self, parameter: &mut FormalParameter<'a>) {
        let outer = self.parameter_targets.remove(&span_key(parameter.span));
        if let Some(target) = &outer {
            let entry = self.entered(target);
            self.function_entries
                .last_mut()
                .expect("formal parameter must belong to a function")
                .push(entry);
        }
        self.visit_decorators(&mut parameter.decorators);
        self.parameter_pattern_depth += 1;
        self.visit_binding_pattern(&mut parameter.pattern);
        self.parameter_pattern_depth -= 1;
        if let Some(annotation) = &mut parameter.type_annotation {
            self.visit_ts_type_annotation(annotation);
        }
        if let Some(initializer) = &mut parameter.initializer {
            self.visit_expression(initializer);
            if let Some(target) = outer {
                let value = initializer.take_in(self.ast.allocator);
                **initializer = self.selected(value, &target);
            }
        }
    }

    fn visit_assignment_pattern(&mut self, assignment: &mut AssignmentPattern<'a>) {
        let target = if self.parameter_pattern_depth > 0 {
            self.parameter_targets.remove(&span_key(assignment.span))
        } else {
            self.binding_targets.remove(&span_key(assignment.span))
        };
        if let Some(target) = &target {
            self.push_entry(target);
        }
        walk_mut::walk_assignment_pattern(self, assignment);
        if let Some(target) = target {
            let value = assignment.right.take_in(self.ast.allocator);
            assignment.right = self.selected(value, &target);
        }
    }

    fn visit_variable_declaration(&mut self, declaration: &mut VariableDeclaration<'a>) {
        self.active_declaration.push(span_key(declaration.span));
        walk_mut::walk_variable_declaration(self, declaration);
        self.active_declaration
            .pop()
            .expect("default declaration stack must remain balanced");
    }

    fn visit_for_in_statement(&mut self, statement: &mut ForInStatement<'a>) {
        let declaration = Self::loop_declaration_span(&statement.left);
        walk_mut::walk_for_in_statement(self, statement);
        if let Some(declaration) = declaration
            && let Some(entries) = self.declaration_entries.remove(&declaration)
        {
            self.prepend_entries(&mut statement.body, entries);
        }
    }

    fn visit_for_of_statement(&mut self, statement: &mut ForOfStatement<'a>) {
        let declaration = Self::loop_declaration_span(&statement.left);
        walk_mut::walk_for_of_statement(self, statement);
        if let Some(declaration) = declaration
            && let Some(entries) = self.declaration_entries.remove(&declaration)
        {
            self.prepend_entries(&mut statement.body, entries);
        }
    }

    fn visit_with_statement(&mut self, statement: &mut WithStatement<'a>) {
        if self.with_statements.contains(&span_key(statement.span)) {
            return;
        }
        walk_mut::walk_with_statement(self, statement);
    }
}

enum ExtendedKind {
    Try,
    Loop,
}

struct ExtendedTransformer<'a, 's> {
    ast: AstBuilder<'a>,
    try_begin: String,
    try_catch: String,
    try_end: String,
    loop_begin: String,
    loop_entered: String,
    loop_end: String,
    try_targets: HashMap<SpanKey, ExtendedTarget>,
    loop_targets: HashMap<SpanKey, ExtendedTarget>,
    names: CandidateNames<'s>,
    scope_declarations: Vec<Vec<String>>,
    source_sensitive_functions: HashSet<SpanKey>,
    with_statements: HashSet<SpanKey>,
}

impl<'a> ExtendedTransformer<'a, '_> {
    fn identifier(&self, name: &str) -> Expression<'a> {
        self.ast
            .expression_identifier(Span::default(), self.ast.ident(name))
    }

    fn assignment_target(&self, name: &str) -> AssignmentTarget<'a> {
        AssignmentTarget::from(
            self.ast
                .simple_assignment_target_assignment_target_identifier(
                    Span::default(),
                    self.ast.ident(name),
                ),
        )
    }

    fn string_argument(&self, value: &str) -> Argument<'a> {
        Argument::from(self.ast.expression_string_literal(
            Span::default(),
            self.ast.str(value),
            None,
        ))
    }

    fn call(&self, name: &str, arguments: oxc_allocator::Vec<'a, Argument<'a>>) -> Expression<'a> {
        self.ast.expression_call(
            Span::default(),
            self.identifier(name),
            NONE,
            arguments,
            false,
        )
    }

    fn call_statement(
        &self,
        name: &str,
        arguments: oxc_allocator::Vec<'a, Argument<'a>>,
    ) -> Statement<'a> {
        self.ast
            .statement_expression(Span::default(), self.call(name, arguments))
    }

    fn enter_scope(&mut self) {
        self.scope_declarations.push(Vec::new());
    }

    fn leave_scope(&mut self, statements: &mut oxc_allocator::Vec<'a, Statement<'a>>) {
        let names = self
            .scope_declarations
            .pop()
            .expect("extended scope stack must remain balanced");
        if names.is_empty() {
            return;
        }
        let declarations = self.ast.vec_from_iter(names.into_iter().map(|name| {
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
                declarations,
                false,
            )),
        );
    }

    fn scratch(&mut self, base: &str) -> String {
        let name = self.names.allocate(base);
        self.scope_declarations
            .last_mut()
            .expect("extended branch must be inside a program or function")
            .push(name.clone());
        name
    }

    fn target(statement: &Statement<'a>) -> Option<(ExtendedKind, SpanKey)> {
        match statement {
            Statement::TryStatement(node) => Some((ExtendedKind::Try, span_key(node.span))),
            Statement::ForInStatement(node) => Some((ExtendedKind::Loop, span_key(node.span))),
            Statement::ForOfStatement(node) => Some((ExtendedKind::Loop, span_key(node.span))),
            Statement::LabeledStatement(node) => Self::target(&node.body),
            _ => None,
        }
    }

    fn inner_try<'b>(statement: &'b mut Statement<'a>) -> Option<&'b mut TryStatement<'a>> {
        match statement {
            Statement::TryStatement(node) => Some(node),
            Statement::LabeledStatement(node) => Self::inner_try(&mut node.body),
            _ => None,
        }
    }

    fn inner_loop_body<'b>(statement: &'b mut Statement<'a>) -> Option<&'b mut Statement<'a>> {
        match statement {
            Statement::ForInStatement(node) => Some(&mut node.body),
            Statement::ForOfStatement(node) => Some(&mut node.body),
            Statement::LabeledStatement(node) => Self::inner_loop_body(&mut node.body),
            _ => None,
        }
    }

    fn prepend(body: &mut Statement<'a>, entry: Statement<'a>, ast: AstBuilder<'a>) {
        if let Statement::BlockStatement(block) = body {
            block.body.insert(0, entry);
            return;
        }
        let original = body.take_in(ast.allocator);
        *body = ast.statement_block(Span::default(), ast.vec_from_array([entry, original]));
    }

    fn begin_assignment(&self, frame: &str, begin: &str, target: &ExtendedTarget) -> Statement<'a> {
        let call = self.call(
            begin,
            self.ast.vec_from_array([
                self.string_argument(&target.first_id),
                self.string_argument(&target.second_id),
            ]),
        );
        self.ast.statement_expression(
            Span::default(),
            self.ast.expression_assignment(
                Span::default(),
                AssignmentOperator::Assign,
                self.assignment_target(frame),
                call,
            ),
        )
    }

    fn instrument_try(&mut self, statement: &mut Statement<'a>, target: ExtendedTarget) {
        let frame = self.scratch("_supercovTryFrame");
        let assignment = self.begin_assignment(&frame, &self.try_begin, &target);
        let node = Self::inner_try(statement).expect("try target must remain a try statement");
        node.handler
            .as_mut()
            .expect("try coverage requires a catch handler")
            .body
            .body
            .insert(
                0,
                self.call_statement(
                    &self.try_catch,
                    self.ast.vec_from_array([
                        Argument::from(self.identifier(&frame)),
                        Argument::from(self.identifier("undefined")),
                    ]),
                ),
            );
        let end = self.call_statement(
            &self.try_end,
            self.ast.vec1(Argument::from(self.identifier(&frame))),
        );
        if let Some(finalizer) = &mut node.finalizer {
            finalizer.body.insert(0, end);
        } else {
            node.finalizer = Some(
                self.ast
                    .alloc_block_statement(Span::default(), self.ast.vec1(end)),
            );
        }
        let original = statement.take_in(self.ast.allocator);
        *statement = self.ast.statement_block(
            Span::default(),
            self.ast.vec_from_array([assignment, original]),
        );
    }

    fn instrument_loop(&mut self, statement: &mut Statement<'a>, target: ExtendedTarget) {
        let frame = self.scratch("_supercovLoopFrame");
        let assignment = self.begin_assignment(&frame, &self.loop_begin, &target);
        let entered = self.call_statement(
            &self.loop_entered,
            self.ast.vec1(Argument::from(self.identifier(&frame))),
        );
        Self::prepend(
            Self::inner_loop_body(statement).expect("loop target must remain an enumeration loop"),
            entered,
            self.ast,
        );
        let original = statement.take_in(self.ast.allocator);
        let end = self.call_statement(
            &self.loop_end,
            self.ast.vec1(Argument::from(self.identifier(&frame))),
        );
        let wrapped = self.ast.statement_try(
            Span::default(),
            self.ast
                .block_statement(Span::default(), self.ast.vec1(original)),
            None::<oxc_allocator::Box<'a, CatchClause<'a>>>,
            Some(
                self.ast
                    .block_statement(Span::default(), self.ast.vec1(end)),
            ),
        );
        *statement = self.ast.statement_block(
            Span::default(),
            self.ast.vec_from_array([assignment, wrapped]),
        );
    }
}

impl<'a> VisitMut<'a> for ExtendedTransformer<'a, '_> {
    fn visit_program(&mut self, program: &mut Program<'a>) {
        self.enter_scope();
        walk_mut::walk_program(self, program);
        self.leave_scope(&mut program.body);
    }

    fn visit_function_body(&mut self, body: &mut FunctionBody<'a>) {
        self.enter_scope();
        walk_mut::walk_function_body(self, body);
        self.leave_scope(&mut body.statements);
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

    fn visit_statement(&mut self, statement: &mut Statement<'a>) {
        let Some((kind, key)) = Self::target(statement) else {
            walk_mut::walk_statement(self, statement);
            return;
        };
        match kind {
            ExtendedKind::Try => {
                if let Some(target) = self.try_targets.remove(&key) {
                    walk_mut::walk_statement(self, statement);
                    self.instrument_try(statement, target);
                } else {
                    walk_mut::walk_statement(self, statement);
                }
            }
            ExtendedKind::Loop => {
                if let Some(target) = self.loop_targets.remove(&key) {
                    walk_mut::walk_statement(self, statement);
                    self.instrument_loop(statement, target);
                } else {
                    walk_mut::walk_statement(self, statement);
                }
            }
        }
    }
}

struct LogicalValueTransformer<'a, 's> {
    ast: AstBuilder<'a>,
    selection_begin: String,
    selection_right: String,
    selection_end: String,
    names: CandidateNames<'s>,
    scope_declarations: Vec<Vec<String>>,
    logical_targets: HashMap<SpanKey, (String, String)>,
    assignment_targets: HashMap<SpanKey, (String, String)>,
    source_sensitive_functions: HashSet<SpanKey>,
    with_statements: HashSet<SpanKey>,
}

impl<'a> LogicalValueTransformer<'a, '_> {
    fn identifier(&self, name: &str) -> Expression<'a> {
        self.ast
            .expression_identifier(Span::default(), self.ast.ident(name))
    }

    fn assignment_target(&self, name: &str) -> AssignmentTarget<'a> {
        AssignmentTarget::from(
            self.ast
                .simple_assignment_target_assignment_target_identifier(
                    Span::default(),
                    self.ast.ident(name),
                ),
        )
    }

    fn call(&self, name: &str, arguments: oxc_allocator::Vec<'a, Argument<'a>>) -> Expression<'a> {
        self.ast.expression_call(
            Span::default(),
            self.identifier(name),
            NONE,
            arguments,
            false,
        )
    }

    fn string_argument(&self, value: &str) -> Argument<'a> {
        Argument::from(self.ast.expression_string_literal(
            Span::default(),
            self.ast.str(value),
            None,
        ))
    }

    fn enter_scope(&mut self) {
        self.scope_declarations.push(Vec::new());
    }

    fn leave_scope(&mut self, statements: &mut oxc_allocator::Vec<'a, Statement<'a>>) {
        let names = self
            .scope_declarations
            .pop()
            .expect("logical-value scope stack must remain balanced");
        if names.is_empty() {
            return;
        }
        let declarations = self.ast.vec_from_iter(names.into_iter().map(|name| {
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
                declarations,
                false,
            )),
        );
    }

    fn scratch(&mut self) -> String {
        let name = self.names.allocate("_supercovSelectionFrame");
        self.scope_declarations
            .last_mut()
            .expect("logical expression must be inside a program or function")
            .push(name.clone());
        name
    }

    fn instrument(
        &mut self,
        logical: oxc_allocator::Box<'a, LogicalExpression<'a>>,
        short_id: &str,
        right_id: &str,
    ) -> Expression<'a> {
        let logical = logical.unbox();
        let frame = self.scratch();
        let begin = self.call(
            &self.selection_begin,
            self.ast.vec_from_array([
                self.string_argument(short_id),
                self.string_argument(right_id),
            ]),
        );
        let assign = self.ast.expression_assignment(
            Span::default(),
            AssignmentOperator::Assign,
            self.assignment_target(&frame),
            begin,
        );
        let right = self.call(
            &self.selection_right,
            self.ast.vec_from_array([
                Argument::from(self.identifier(&frame)),
                Argument::from(logical.right),
            ]),
        );
        let selection =
            self.ast
                .expression_logical(Span::default(), logical.left, logical.operator, right);
        let end = self.call(
            &self.selection_end,
            self.ast.vec_from_array([
                Argument::from(self.identifier(&frame)),
                Argument::from(selection),
            ]),
        );
        self.ast
            .expression_sequence(Span::default(), self.ast.vec_from_array([assign, end]))
    }

    fn instrument_assignment(
        &mut self,
        assignment: oxc_allocator::Box<'a, AssignmentExpression<'a>>,
        short_id: &str,
        right_id: &str,
    ) -> Expression<'a> {
        let assignment = assignment.unbox();
        let inferred_name = match &assignment.left {
            AssignmentTarget::AssignmentTargetIdentifier(identifier)
                if assignment.span.start == identifier.span.start
                    && expression_is_anonymous_definition(&assignment.right) =>
            {
                Some(identifier.name.to_string())
            }
            _ => None,
        };
        let frame = self.scratch();
        let begin = self.call(
            &self.selection_begin,
            self.ast.vec_from_array([
                self.string_argument(short_id),
                self.string_argument(right_id),
            ]),
        );
        let assign_frame = self.ast.expression_assignment(
            Span::default(),
            AssignmentOperator::Assign,
            self.assignment_target(&frame),
            begin,
        );
        let mut right_arguments = self.ast.vec_from_array([
            Argument::from(self.identifier(&frame)),
            Argument::from(assignment.right),
        ]);
        if let Some(name) = inferred_name {
            right_arguments.push(self.string_argument(&name));
        }
        let right = self.call(&self.selection_right, right_arguments);
        let measured_assignment = self.ast.expression_assignment(
            Span::default(),
            assignment.operator,
            assignment.left,
            right,
        );
        let end = self.call(
            &self.selection_end,
            self.ast.vec_from_array([
                Argument::from(self.identifier(&frame)),
                Argument::from(measured_assignment),
            ]),
        );
        self.ast.expression_sequence(
            Span::default(),
            self.ast.vec_from_array([assign_frame, end]),
        )
    }
}

impl<'a> VisitMut<'a> for LogicalValueTransformer<'a, '_> {
    fn visit_program(&mut self, program: &mut Program<'a>) {
        self.enter_scope();
        walk_mut::walk_program(self, program);
        self.leave_scope(&mut program.body);
    }

    fn visit_function_body(&mut self, body: &mut FunctionBody<'a>) {
        self.enter_scope();
        walk_mut::walk_function_body(self, body);
        self.leave_scope(&mut body.statements);
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

    fn visit_expression(&mut self, expression: &mut Expression<'a>) {
        let key = span_key(expression.span());
        walk_mut::walk_expression(self, expression);
        if let Some((short_id, right_id)) = self.assignment_targets.remove(&key) {
            let original = expression.take_in(self.ast.allocator);
            let Expression::AssignmentExpression(assignment) = original else {
                panic!("logical-assignment target must remain an assignment expression");
            };
            *expression = self.instrument_assignment(assignment, &short_id, &right_id);
            return;
        }
        let Some((short_id, right_id)) = self.logical_targets.remove(&key) else {
            return;
        };
        let original = expression.take_in(self.ast.allocator);
        let Expression::LogicalExpression(logical) = original else {
            panic!("logical-value target must remain a logical expression");
        };
        *expression = self.instrument(logical, &short_id, &right_id);
    }
}

struct SwitchTransformer<'a, 's> {
    ast: AstBuilder<'a>,
    coverage_hit: String,
    targets: HashMap<SpanKey, SwitchTarget>,
    names: CandidateNames<'s>,
    source_sensitive_functions: HashSet<SpanKey>,
    with_statements: HashSet<SpanKey>,
}

impl<'a> SwitchTransformer<'a, '_> {
    fn identifier(&self, name: &str) -> Expression<'a> {
        self.ast
            .expression_identifier(Span::default(), self.ast.ident(name))
    }

    fn assignment_target(&self, name: &str) -> AssignmentTarget<'a> {
        AssignmentTarget::from(
            self.ast
                .simple_assignment_target_assignment_target_identifier(
                    Span::default(),
                    self.ast.ident(name),
                ),
        )
    }

    fn probe(&self, id: &str) -> Statement<'a> {
        self.ast.statement_expression(
            Span::default(),
            self.ast.expression_call(
                Span::default(),
                self.identifier(&self.coverage_hit),
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

    fn target(statement: &Statement<'a>) -> Option<SpanKey> {
        match statement {
            Statement::SwitchStatement(node) => Some(span_key(node.span)),
            Statement::LabeledStatement(node) => Self::target(&node.body),
            _ => None,
        }
    }

    fn inner_switch<'b>(statement: &'b mut Statement<'a>) -> Option<&'b mut SwitchStatement<'a>> {
        match statement {
            Statement::SwitchStatement(node) => Some(node),
            Statement::LabeledStatement(node) => Self::inner_switch(&mut node.body),
            _ => None,
        }
    }

    fn entered_assignment(&self, entered: &str) -> Statement<'a> {
        self.ast.statement_expression(
            Span::default(),
            self.ast.expression_assignment(
                Span::default(),
                AssignmentOperator::Assign,
                self.assignment_target(entered),
                self.ast.expression_boolean_literal(Span::default(), true),
            ),
        )
    }

    fn instrument(&mut self, statement: &mut Statement<'a>, target: &SwitchTarget) {
        let entered = target
            .no_match_id
            .as_ref()
            .map(|_| self.names.allocate("_supercovSwitchEntered"));
        let node = Self::inner_switch(statement).expect("switch target must remain a switch");
        for (index, case) in node.cases.iter_mut().enumerate() {
            let probe = self.probe(
                target
                    .case_ids
                    .get(index)
                    .expect("switch case target count must remain stable"),
            );
            case.consequent.insert(0, probe);
            if let Some(entered) = &entered {
                case.consequent.insert(0, self.entered_assignment(entered));
            }
        }
        let (Some(entered), Some(no_match_id)) = (entered, &target.no_match_id) else {
            return;
        };
        let declaration =
            Statement::VariableDeclaration(self.ast.alloc_variable_declaration(
                Span::default(),
                VariableDeclarationKind::Let,
                self.ast.vec1(self.ast.variable_declarator(
                    Span::default(),
                    VariableDeclarationKind::Let,
                    self.ast.binding_pattern_binding_identifier(
                        Span::default(),
                        self.ast.ident(&entered),
                    ),
                    NONE,
                    Some(self.ast.expression_boolean_literal(Span::default(), false)),
                    false,
                )),
                false,
            ));
        let original = statement.take_in(self.ast.allocator);
        let no_match = self.ast.statement_if(
            Span::default(),
            self.ast.expression_unary(
                Span::default(),
                UnaryOperator::LogicalNot,
                self.identifier(&entered),
            ),
            self.probe(no_match_id),
            None,
        );
        *statement = self.ast.statement_block(
            Span::default(),
            self.ast.vec_from_array([declaration, original, no_match]),
        );
    }
}

impl<'a> VisitMut<'a> for SwitchTransformer<'a, '_> {
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

    fn visit_statement(&mut self, statement: &mut Statement<'a>) {
        let Some(key) = Self::target(statement) else {
            walk_mut::walk_statement(self, statement);
            return;
        };
        let Some(target) = self.targets.remove(&key) else {
            walk_mut::walk_statement(self, statement);
            return;
        };
        let has_no_match = target.no_match_id.is_some();
        self.instrument(statement, &target);
        if !has_no_match {
            walk_mut::walk_statement(self, statement);
        }
    }
}

struct RouteRequestPhaseTransformer<'a, 's> {
    ast: AstBuilder<'a>,
    file: &'s str,
    with_request_phase: String,
    used: bool,
    names: CandidateNames<'s>,
}

impl<'a> RouteRequestPhaseTransformer<'a, '_> {
    fn is_remix_route(&self) -> bool {
        self.file.starts_with("app/routes/")
    }

    fn is_next_route(&self) -> bool {
        let path = Path::new(self.file);
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            return false;
        };
        let is_route_module = [
            "route.js",
            "route.jsx",
            "route.ts",
            "route.tsx",
            "route.mjs",
            "route.mts",
            "route.cjs",
            "route.cts",
        ]
        .contains(&name);
        if !is_route_module {
            return false;
        }
        let components = path
            .components()
            .filter_map(|component| component.as_os_str().to_str())
            .collect::<Vec<_>>();
        components
            .windows(2)
            .any(|window| window[0] == "app" && window[1] != name)
    }

    fn is_handler_name(&self, name: &str) -> bool {
        (self.is_remix_route() && matches!(name, "loader" | "action"))
            || (self.is_next_route()
                && matches!(
                    name,
                    "GET" | "HEAD" | "POST" | "PUT" | "DELETE" | "PATCH" | "OPTIONS"
                ))
    }

    fn identifier(&self, name: &str) -> Expression<'a> {
        self.ast
            .expression_identifier(Span::default(), self.ast.ident(name))
    }

    fn wrap_expression(&self, expression: Expression<'a>) -> Expression<'a> {
        self.ast.expression_call(
            Span::default(),
            self.identifier(&self.with_request_phase),
            NONE,
            self.ast.vec1(Argument::from(expression)),
            false,
        )
    }

    fn wrapped_named_export(&self, exported_name: &str, original_name: &str) -> Statement<'a> {
        Statement::ExportNamedDeclaration(self.ast.alloc_export_named_declaration(
            Span::default(),
            Some(self.ast.declaration_variable(
                Span::default(),
                VariableDeclarationKind::Const,
                self.ast.vec1(self.ast.variable_declarator(
                    Span::default(),
                    VariableDeclarationKind::Const,
                    self.ast.binding_pattern_binding_identifier(
                        Span::default(),
                        self.ast.ident(exported_name),
                    ),
                    NONE,
                    Some(self.wrap_expression(self.identifier(original_name))),
                    false,
                )),
                false,
            )),
            self.ast.vec(),
            None,
            ImportOrExportKind::Value,
            NONE,
        ))
    }

    fn transform_named_export(
        &mut self,
        mut export: oxc_allocator::Box<'a, oxc_ast::ast::ExportNamedDeclaration<'a>>,
        output: &mut oxc_allocator::Vec<'a, Statement<'a>>,
    ) {
        if let Some(Declaration::VariableDeclaration(declaration)) = &mut export.declaration {
            for declarator in &mut declaration.declarations {
                let Some(name) = binding_identifier_name(&declarator.id) else {
                    continue;
                };
                if !self.is_handler_name(&name) {
                    continue;
                }
                if let Some(initializer) = &mut declarator.init {
                    let original = initializer.take_in(self.ast.allocator);
                    *initializer = self.wrap_expression(original);
                    self.used = true;
                }
            }
            output.push(Statement::ExportNamedDeclaration(export));
            return;
        }

        if matches!(
            export.declaration,
            Some(Declaration::FunctionDeclaration(_))
        ) {
            let Some(Declaration::FunctionDeclaration(mut function)) = export.declaration.take()
            else {
                unreachable!();
            };
            let Some(exported_name) = function.id.as_ref().map(|id| id.name.to_string()) else {
                output.push(Statement::FunctionDeclaration(function));
                return;
            };
            if !self.is_handler_name(&exported_name) {
                export.declaration = Some(Declaration::FunctionDeclaration(function));
                output.push(Statement::ExportNamedDeclaration(export));
                return;
            }
            let original_name = self
                .names
                .allocate(&format!("__supercov{exported_name}CoverageOriginal"));
            function.id = Some(
                self.ast
                    .binding_identifier(Span::default(), self.ast.ident(&original_name)),
            );
            output.push(Statement::FunctionDeclaration(function));
            output.push(self.wrapped_named_export(&exported_name, &original_name));
            self.used = true;
            return;
        }

        if export.source.is_none() {
            output.push(Statement::ExportNamedDeclaration(export));
            return;
        }
        let source = export
            .source
            .as_ref()
            .expect("checked above")
            .clone_in(self.ast.allocator);
        let specifiers = export.specifiers.take_in(self.ast.allocator);
        let mut untouched = self.ast.vec();
        let mut handlers = Vec::new();
        for specifier in specifiers {
            let exported_name = specifier
                .exported
                .identifier_name()
                .map(|name| name.to_string());
            if exported_name
                .as_deref()
                .is_some_and(|name| self.is_handler_name(name))
            {
                handlers.push((exported_name.expect("checked above"), specifier));
            } else {
                untouched.push(specifier);
            }
        }
        if handlers.is_empty() {
            export.specifiers = untouched;
            output.push(Statement::ExportNamedDeclaration(export));
            return;
        }
        if !untouched.is_empty() {
            output.push(Statement::ExportNamedDeclaration(
                self.ast.alloc_export_named_declaration(
                    export.span,
                    None,
                    untouched,
                    Some(source.clone_in(self.ast.allocator)),
                    export.export_kind,
                    export.with_clause.take(),
                ),
            ));
        }
        for (exported_name, specifier) in handlers {
            let original_name = self
                .names
                .allocate(&format!("__supercov{exported_name}CoverageOriginal"));
            output.push(Statement::ImportDeclaration(
                self.ast.alloc_import_declaration(
                    Span::default(),
                    Some(self.ast.vec1(
                        self.ast.import_declaration_specifier_import_specifier(
                            Span::default(),
                            specifier.local,
                            self.ast.binding_identifier(
                                Span::default(),
                                self.ast.ident(&original_name),
                            ),
                            ImportOrExportKind::Value,
                        ),
                    )),
                    source.clone_in(self.ast.allocator),
                    None,
                    NONE,
                    ImportOrExportKind::Value,
                ),
            ));
            output.push(self.wrapped_named_export(&exported_name, &original_name));
        }
        self.used = true;
    }

    fn transform_default_export(
        &mut self,
        mut export: oxc_allocator::Box<'a, oxc_ast::ast::ExportDefaultDeclaration<'a>>,
        output: &mut oxc_allocator::Vec<'a, Statement<'a>>,
    ) {
        if let ExportDefaultDeclarationKind::FunctionDeclaration(mut function) =
            export.declaration.take_in(self.ast.allocator)
        {
            let original_name = self
                .names
                .allocate("__supercovHandleRequestCoverageOriginal");
            function.id = Some(
                self.ast
                    .binding_identifier(Span::default(), self.ast.ident(&original_name)),
            );
            output.push(Statement::FunctionDeclaration(function));
            output.push(Statement::ExportDefaultDeclaration(
                self.ast.alloc_export_default_declaration(
                    Span::default(),
                    ExportDefaultDeclarationKind::from(
                        self.wrap_expression(self.identifier(&original_name)),
                    ),
                ),
            ));
            self.used = true;
            return;
        }
        if export.declaration.is_expression() {
            let original = export
                .declaration
                .take_in(self.ast.allocator)
                .into_expression();
            export.declaration = ExportDefaultDeclarationKind::from(self.wrap_expression(original));
            self.used = true;
        }
        output.push(Statement::ExportDefaultDeclaration(export));
    }

    fn transform_program(&mut self, program: &mut Program<'a>) {
        let route_module = self.is_remix_route() || self.is_next_route();
        let server_entry = self.file.starts_with("app/entry.server.")
            && ["js", "jsx", "ts", "tsx", "mjs", "mts", "cjs", "cts"].contains(
                &Path::new(self.file)
                    .extension()
                    .and_then(|value| value.to_str())
                    .unwrap_or(""),
            );
        if !route_module && !server_entry {
            return;
        }
        let original = program.body.take_in(self.ast.allocator);
        let mut output = self.ast.vec_with_capacity(original.len() + 4);
        for statement in original {
            match statement {
                Statement::ExportNamedDeclaration(export) if route_module => {
                    self.transform_named_export(export, &mut output);
                }
                Statement::ExportDefaultDeclaration(export) if server_entry => {
                    self.transform_default_export(export, &mut output);
                }
                statement => output.push(statement),
            }
        }
        program.body = output;
    }
}

struct RequestPhaseTransformer<'a> {
    ast: AstBuilder<'a>,
    with_request_phase: String,
    used: bool,
    source_sensitive_functions: HashSet<SpanKey>,
    with_statements: HashSet<SpanKey>,
}

impl<'a> RequestPhaseTransformer<'a> {
    fn identifier(&self, name: &str) -> Expression<'a> {
        self.ast
            .expression_identifier(Span::default(), self.ast.ident(name))
    }

    fn callee_is(callee: &Expression<'a>, name: &str) -> bool {
        match callee {
            Expression::Identifier(identifier) => identifier.name == name,
            Expression::StaticMemberExpression(member) => member.property.name == name,
            _ => false,
        }
    }

    fn callback_candidate(argument: &Argument<'a>) -> bool {
        matches!(
            argument,
            Argument::FunctionExpression(_)
                | Argument::ArrowFunctionExpression(_)
                | Argument::Identifier(_)
                | Argument::ComputedMemberExpression(_)
                | Argument::StaticMemberExpression(_)
                | Argument::PrivateFieldExpression(_)
        )
    }

    fn already_wrapped(&self, argument: &Argument<'a>) -> bool {
        matches!(
            argument,
            Argument::CallExpression(call)
                if expression_is_identifier(&call.callee, &self.with_request_phase)
        )
    }

    fn wrap_argument(&mut self, argument: &mut Argument<'a>) {
        if self.already_wrapped(argument) || !argument.is_expression() {
            return;
        }
        let expression = argument.to_expression_mut();
        let original = expression.take_in(self.ast.allocator);
        *expression = self.ast.expression_call(
            Span::default(),
            self.identifier(&self.with_request_phase),
            NONE,
            self.ast.vec1(Argument::from(original)),
            false,
        );
        self.used = true;
    }
}

impl<'a> VisitMut<'a> for RequestPhaseTransformer<'a> {
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

    fn visit_call_expression(&mut self, call: &mut CallExpression<'a>) {
        walk_mut::walk_call_expression(self, call);
        let property = match &call.callee {
            Expression::StaticMemberExpression(member) => Some(member.property.name.as_str()),
            _ => None,
        };
        let mut callback_index = None;
        if matches!(property, Some("on" | "once" | "addListener"))
            && matches!(
                call.arguments.first(),
                Some(Argument::StringLiteral(event))
                    if matches!(event.value.as_str(), "request" | "upgrade" | "connection")
            )
        {
            callback_index = Some(1);
        } else if Self::callee_is(&call.callee, "createServer") {
            callback_index = call.arguments.iter().rposition(Self::callback_candidate);
        }
        if let Some(index) = callback_index
            && let Some(argument) = call.arguments.get_mut(index)
        {
            self.wrap_argument(argument);
        }
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
    decisions: &'s [CandidateDecision],
    mcdc_begin: String,
    mcdc_condition: String,
    mcdc_end: String,
    mcdc_end_v2: String,
    probe_file_v2: String,
    names: CandidateNames<'s>,
    scope_declarations: Vec<Vec<String>>,
    decision_index: usize,
    parameter_depth: usize,
    source_sensitive_functions: HashSet<SpanKey>,
    with_statements: HashSet<SpanKey>,
}

#[derive(Clone, Copy)]
struct DecisionPlan {
    index: usize,
    condition_count: usize,
    inline_frame: bool,
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

    fn scratch_for(&mut self, base: &str, inline: bool) -> String {
        if inline {
            self.names.allocate(base)
        } else {
            self.allocate_scratch(base)
        }
    }

    fn wrap_inline_frame(&self, expression: Expression<'a>, names: &[String]) -> Expression<'a> {
        let declarations = self.ast.vec_from_iter(names.iter().map(|name| {
            self.ast.variable_declarator(
                Span::default(),
                VariableDeclarationKind::Let,
                self.ast
                    .binding_pattern_binding_identifier(Span::default(), self.ast.ident(name)),
                NONE,
                None,
                false,
            )
        }));
        let body = self.ast.alloc_function_body(
            Span::default(),
            self.ast.vec(),
            self.ast.vec_from_array([
                Statement::VariableDeclaration(self.ast.alloc_variable_declaration(
                    Span::default(),
                    VariableDeclarationKind::Let,
                    declarations,
                    false,
                )),
                self.ast.statement_return(Span::default(), Some(expression)),
            ]),
        );
        let params = self.ast.alloc_formal_parameters(
            Span::default(),
            FormalParameterKind::ArrowFormalParameters,
            self.ast.vec(),
            NONE,
        );
        let arrow = self.ast.expression_arrow_function(
            Span::default(),
            false,
            false,
            NONE,
            params,
            NONE,
            body,
        );
        self.ast
            .expression_call(Span::default(), arrow, NONE, self.ast.vec(), false)
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
            inline_frame: self.parameter_depth > 0,
        };
        self.decision_index += 1;
        plan
    }

    fn apply_decision(&mut self, test: &mut Expression<'a>, plan: DecisionPlan) {
        if plan.condition_count > 32 {
            let frame_name = self.scratch_for("_supercovMcdcFrame", plan.inline_frame);
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
            let observed = self.ast.expression_sequence(
                Span::default(),
                self.ast.vec_from_array([assign_frame, end]),
            );
            *test = if plan.inline_frame {
                self.wrap_inline_frame(observed, &[frame_name])
            } else {
                observed
            };
            return;
        }

        let frame_name = self.scratch_for("_supercovMcdcFrame", plan.inline_frame);
        let result_name = self.scratch_for("_supercovMcdcResult", plan.inline_frame);
        let temporary_names = (0..plan.condition_count)
            .map(|_| self.scratch_for("_supercovMcdcValue", plan.inline_frame))
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
            Argument::from(self.identifier(&self.probe_file_v2)),
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
        let observed = self.ast.expression_sequence(
            Span::default(),
            self.ast.vec_from_array([
                assign_frame,
                assign_result,
                record,
                self.identifier(&result_name),
            ]),
        );
        *test = if plan.inline_frame {
            let mut names = vec![frame_name, result_name];
            names.extend(temporary_names);
            self.wrap_inline_frame(observed, &names)
        } else {
            observed
        };
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
        let outer_parameter_depth = self.parameter_depth;
        self.parameter_depth = 0;
        walk_mut::walk_function(self, function, flags);
        self.parameter_depth = outer_parameter_depth;
    }

    fn visit_arrow_function_expression(&mut self, function: &mut ArrowFunctionExpression<'a>) {
        if self
            .source_sensitive_functions
            .contains(&span_key(function.span))
        {
            return;
        }
        let outer_parameter_depth = self.parameter_depth;
        self.parameter_depth = 0;
        walk_mut::walk_arrow_function_expression(self, function);
        self.parameter_depth = outer_parameter_depth;
    }

    fn visit_formal_parameters(&mut self, parameters: &mut FormalParameters<'a>) {
        self.parameter_depth += 1;
        walk_mut::walk_formal_parameters(self, parameters);
        self.parameter_depth -= 1;
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
    decision_vector_counts: Vec<usize>,
    decision_logical_nodes: HashSet<SpanKey>,
    source_sensitive_functions: &'s HashSet<SpanKey>,
    with_statements: &'s HashSet<SpanKey>,
}

impl DecisionCollector<'_> {
    fn record_decision(&mut self, test: &Expression<'_>, kind: &str) {
        let mut condition_spans = Vec::new();
        collect_conditions(test, &mut condition_spans);
        collect_decision_logical_nodes(test, &mut self.decision_logical_nodes);
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
                .iter()
                .map(|condition| source_slice(self.source, *condition).to_string())
                .collect(),
            kind: kind.to_string(),
        });
        self.decision_vector_counts.push(
            if condition_spans.len() <= 6 && decision_conditions_are_transparent(test) {
                reachable_vector_count(test, &condition_spans)
            } else {
                0
            },
        );
    }
}

#[derive(Default)]
struct CoverageSurfaceScanner {
    found: bool,
}

impl<'a> Visit<'a> for CoverageSurfaceScanner {
    fn visit_logical_expression(&mut self, _expression: &LogicalExpression<'a>) {
        self.found = true;
    }

    fn visit_conditional_expression(&mut self, _expression: &ConditionalExpression<'a>) {
        self.found = true;
    }

    fn visit_chain_expression(&mut self, _expression: &ChainExpression<'a>) {
        self.found = true;
    }

    fn visit_function(&mut self, _function: &Function<'a>, _flags: ScopeFlags) {
        self.found = true;
    }

    fn visit_arrow_function_expression(&mut self, _function: &ArrowFunctionExpression<'a>) {
        self.found = true;
    }

    fn visit_class(&mut self, _class: &Class<'a>) {
        self.found = true;
    }

    fn visit_assignment_expression(&mut self, expression: &AssignmentExpression<'a>) {
        if expression.operator.is_logical() {
            self.found = true;
        } else {
            walk::walk_assignment_expression(self, expression);
        }
    }
}

fn decision_conditions_are_transparent(expression: &Expression<'_>) -> bool {
    fn visit_condition(expression: &Expression<'_>) -> bool {
        let mut scanner = CoverageSurfaceScanner::default();
        scanner.visit_expression(expression);
        !scanner.found
    }
    match transparent_expression(expression) {
        Expression::LogicalExpression(logical)
            if matches!(logical.operator, LogicalOperator::And | LogicalOperator::Or) =>
        {
            decision_conditions_are_transparent(&logical.left)
                && decision_conditions_are_transparent(&logical.right)
        }
        Expression::UnaryExpression(unary)
            if unary.operator.is_not() && has_compound_boolean_decision(&unary.argument) =>
        {
            decision_conditions_are_transparent(&unary.argument)
        }
        condition => visit_condition(condition),
    }
}

fn reachable_vector_count(expression: &Expression<'_>, conditions: &[Span]) -> usize {
    fn evaluate(
        expression: &Expression<'_>,
        assignment: usize,
        encoded: &mut usize,
        indices: &HashMap<SpanKey, usize>,
    ) -> bool {
        match transparent_expression(expression) {
            Expression::LogicalExpression(logical)
                if matches!(logical.operator, LogicalOperator::And | LogicalOperator::Or) =>
            {
                let left = evaluate(&logical.left, assignment, encoded, indices);
                if logical.operator == LogicalOperator::And {
                    left && evaluate(&logical.right, assignment, encoded, indices)
                } else {
                    left || evaluate(&logical.right, assignment, encoded, indices)
                }
            }
            Expression::UnaryExpression(unary)
                if unary.operator.is_not() && has_compound_boolean_decision(&unary.argument) =>
            {
                !evaluate(&unary.argument, assignment, encoded, indices)
            }
            condition => {
                let index = indices
                    .get(&span_key(condition.span()))
                    .expect("decision condition index must remain stable");
                let value = assignment & (1 << index) != 0;
                *encoded += (if value { 2 } else { 1 }) * 3_usize.pow(*index as u32);
                value
            }
        }
    }

    let indices = conditions
        .iter()
        .enumerate()
        .map(|(index, span)| (span_key(*span), index))
        .collect::<HashMap<_, _>>();
    let mut vectors = HashSet::new();
    for assignment in 0..(1_usize << conditions.len()) {
        let mut encoded = 0;
        let outcome = evaluate(expression, assignment, &mut encoded, &indices);
        vectors.insert(encoded * 2 + usize::from(outcome));
    }
    vectors.len()
}

fn collect_decision_logical_nodes(expression: &Expression<'_>, nodes: &mut HashSet<SpanKey>) {
    match expression {
        Expression::ParenthesizedExpression(parenthesized) => {
            collect_decision_logical_nodes(&parenthesized.expression, nodes);
        }
        Expression::LogicalExpression(logical)
            if matches!(logical.operator, LogicalOperator::And | LogicalOperator::Or) =>
        {
            nodes.insert(span_key(logical.span));
            collect_decision_logical_nodes(&logical.left, nodes);
            collect_decision_logical_nodes(&logical.right, nodes);
        }
        Expression::UnaryExpression(unary)
            if unary.operator.is_not() && has_compound_boolean_decision(&unary.argument) =>
        {
            collect_decision_logical_nodes(&unary.argument, nodes);
        }
        _ => {}
    }
}

#[derive(Default)]
struct LogicalBranchAnalysis {
    branches: Vec<CandidateBranch>,
    logical_targets: HashMap<SpanKey, (String, String)>,
}

#[derive(Default)]
struct LogicalAssignmentAnalysis {
    branches: Vec<CandidateBranch>,
    targets: HashMap<SpanKey, (String, String)>,
}

#[derive(Default)]
struct OptionalMemberAnalysis {
    branches: Vec<CandidateBranch>,
    targets: HashMap<SpanKey, (String, String)>,
}

#[derive(Default)]
struct OptionalCallAnalysis {
    branches: Vec<CandidateBranch>,
    sites: HashMap<SpanKey, (String, String)>,
    roots: HashMap<SpanKey, Vec<SpanKey>>,
    limitations: Vec<CandidateLimitation>,
}

#[derive(Clone)]
struct DefaultTarget {
    default_id: String,
    provided_id: String,
    inferred_name: Option<String>,
}

#[derive(Default)]
struct DefaultAnalysis {
    branches: Vec<CandidateBranch>,
    parameter_targets: HashMap<SpanKey, DefaultTarget>,
    binding_targets: HashMap<SpanKey, DefaultTarget>,
    limitations: Vec<CandidateLimitation>,
}

#[derive(Clone)]
struct ExtendedTarget {
    first_id: String,
    second_id: String,
}

#[derive(Default)]
struct ExtendedAnalysis {
    branches: Vec<CandidateBranch>,
    try_targets: HashMap<SpanKey, ExtendedTarget>,
    loop_targets: HashMap<SpanKey, ExtendedTarget>,
}

#[derive(Clone)]
struct SwitchTarget {
    case_ids: Vec<String>,
    no_match_id: Option<String>,
}

#[derive(Default)]
struct SwitchAnalysis {
    branches: Vec<CandidateBranch>,
    targets: HashMap<SpanKey, SwitchTarget>,
}

struct SwitchCollector<'s> {
    source: &'s str,
    file: &'s str,
    source_sensitive_functions: &'s HashSet<SpanKey>,
    unsafe_function_depth: usize,
    with_depth: usize,
    suppressed_depth: usize,
    suppressed_nodes: Vec<bool>,
    analysis: SwitchAnalysis,
}

impl SwitchCollector<'_> {
    fn unsafe_context(&self) -> bool {
        self.unsafe_function_depth > 0 || self.with_depth > 0
    }

    fn exit_source_function(&mut self, span: Span) {
        if self.source_sensitive_functions.contains(&span_key(span)) {
            self.unsafe_function_depth -= 1;
        }
    }
}

impl<'a> Traverse<'a, ()> for SwitchCollector<'_> {
    fn enter_function(&mut self, node: &mut Function<'a>, _context: &mut TraverseCtx<'a, ()>) {
        if self
            .source_sensitive_functions
            .contains(&span_key(node.span))
        {
            self.unsafe_function_depth += 1;
        }
    }

    fn exit_function(&mut self, node: &mut Function<'a>, _context: &mut TraverseCtx<'a, ()>) {
        self.exit_source_function(node.span);
    }

    fn enter_arrow_function_expression(
        &mut self,
        node: &mut ArrowFunctionExpression<'a>,
        _context: &mut TraverseCtx<'a, ()>,
    ) {
        if self
            .source_sensitive_functions
            .contains(&span_key(node.span))
        {
            self.unsafe_function_depth += 1;
        }
    }

    fn exit_arrow_function_expression(
        &mut self,
        node: &mut ArrowFunctionExpression<'a>,
        _context: &mut TraverseCtx<'a, ()>,
    ) {
        self.exit_source_function(node.span);
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

    fn enter_switch_statement(
        &mut self,
        node: &mut SwitchStatement<'a>,
        _context: &mut TraverseCtx<'a, ()>,
    ) {
        let has_default = node.cases.iter().any(|case| case.test.is_none());
        let transformed = self.suppressed_depth == 0 && !self.unsafe_context();
        let suppresses = transformed && !has_default;
        self.suppressed_nodes.push(suppresses);
        if suppresses {
            self.suppressed_depth += 1;
        }
        if !transformed {
            return;
        }
        let id = stable_id(self.source, self.file, "switch", node.span, "");
        let mut case_ids = Vec::with_capacity(node.cases.len());
        let mut alternatives = Vec::with_capacity(node.cases.len() + usize::from(!has_default));
        for (index, case) in node.cases.iter().enumerate() {
            let alternative_id = format!("{id}:case:{index}");
            let label = case.test.as_ref().map_or_else(
                || "default".to_string(),
                |test| format!("case {}", source_slice(self.source, test.span())),
            );
            case_ids.push(alternative_id.clone());
            alternatives.push(CandidateBranchAlternative {
                id: alternative_id,
                label,
            });
        }
        let no_match_id = (!has_default).then(|| format!("{id}:no-match"));
        if let Some(no_match_id) = &no_match_id {
            alternatives.push(CandidateBranchAlternative {
                id: no_match_id.clone(),
                label: "no matching case".to_string(),
            });
        }
        let (line, column) = line_and_utf16_column(self.source, node.span.start as usize);
        self.analysis.branches.push(CandidateBranch {
            id,
            kind: "switch".to_string(),
            file: self.file.to_string(),
            line,
            column,
            source: source_slice(self.source, node.discriminant.span()).to_string(),
            alternatives,
        });
        self.analysis.targets.insert(
            span_key(node.span),
            SwitchTarget {
                case_ids,
                no_match_id,
            },
        );
    }

    fn exit_switch_statement(
        &mut self,
        _node: &mut SwitchStatement<'a>,
        _context: &mut TraverseCtx<'a, ()>,
    ) {
        if self
            .suppressed_nodes
            .pop()
            .expect("switch collector stack must remain balanced")
        {
            self.suppressed_depth -= 1;
        }
    }
}

fn collect_switch_branches<'a>(
    allocator: &'a Allocator,
    program: &mut Program<'a>,
    source: &str,
    file: &str,
    source_sensitive_functions: &HashSet<SpanKey>,
) -> SwitchAnalysis {
    let mut collector = SwitchCollector {
        source,
        file,
        source_sensitive_functions,
        unsafe_function_depth: 0,
        with_depth: 0,
        suppressed_depth: 0,
        suppressed_nodes: Vec::new(),
        analysis: SwitchAnalysis::default(),
    };
    traverse_mut(&mut collector, allocator, program, Default::default(), ());
    collector.analysis
}

struct ExtendedCollector<'s> {
    source: &'s str,
    file: &'s str,
    source_sensitive_functions: &'s HashSet<SpanKey>,
    unsafe_function_depth: usize,
    with_depth: usize,
    analysis: ExtendedAnalysis,
}

impl ExtendedCollector<'_> {
    fn unsafe_context(&self) -> bool {
        self.unsafe_function_depth > 0 || self.with_depth > 0
    }

    fn enter_try(&mut self, node: &TryStatement<'_>) {
        let transformed = !self.unsafe_context() && node.handler.is_some();
        if !transformed {
            return;
        }
        let id = stable_id(self.source, self.file, "try-catch", node.span, "");
        let success_id = format!("{id}:success");
        let catch_id = format!("{id}:catch");
        let (line, column) = line_and_utf16_column(self.source, node.span.start as usize);
        self.analysis.branches.push(CandidateBranch {
            id,
            kind: "try-catch".to_string(),
            file: self.file.to_string(),
            line,
            column,
            source: "try / catch".to_string(),
            alternatives: vec![
                CandidateBranchAlternative {
                    id: success_id.clone(),
                    label: "try completed without catch".to_string(),
                },
                CandidateBranchAlternative {
                    id: catch_id.clone(),
                    label: "catch entered".to_string(),
                },
            ],
        });
        self.analysis.try_targets.insert(
            span_key(node.span),
            ExtendedTarget {
                first_id: success_id,
                second_id: catch_id,
            },
        );
    }

    fn enter_loop(&mut self, span: Span, right: &Expression<'_>, kind: &str) {
        let transformed = !self.unsafe_context();
        if !transformed {
            return;
        }
        let id = stable_id(self.source, self.file, kind, span, "");
        let zero_id = format!("{id}:zero");
        let entered_id = format!("{id}:entered");
        let (line, column) = line_and_utf16_column(self.source, span.start as usize);
        self.analysis.branches.push(CandidateBranch {
            id,
            kind: kind.to_string(),
            file: self.file.to_string(),
            line,
            column,
            source: source_slice(self.source, right.span()).to_string(),
            alternatives: vec![
                CandidateBranchAlternative {
                    id: zero_id.clone(),
                    label: "zero iterations".to_string(),
                },
                CandidateBranchAlternative {
                    id: entered_id.clone(),
                    label: "one or more iterations".to_string(),
                },
            ],
        });
        self.analysis.loop_targets.insert(
            span_key(span),
            ExtendedTarget {
                first_id: zero_id,
                second_id: entered_id,
            },
        );
    }

    fn exit_source_function(&mut self, span: Span) {
        if self.source_sensitive_functions.contains(&span_key(span)) {
            self.unsafe_function_depth -= 1;
        }
    }
}

impl<'a> Traverse<'a, ()> for ExtendedCollector<'_> {
    fn enter_function(&mut self, node: &mut Function<'a>, _context: &mut TraverseCtx<'a, ()>) {
        if self
            .source_sensitive_functions
            .contains(&span_key(node.span))
        {
            self.unsafe_function_depth += 1;
        }
    }

    fn exit_function(&mut self, node: &mut Function<'a>, _context: &mut TraverseCtx<'a, ()>) {
        self.exit_source_function(node.span);
    }

    fn enter_arrow_function_expression(
        &mut self,
        node: &mut ArrowFunctionExpression<'a>,
        _context: &mut TraverseCtx<'a, ()>,
    ) {
        if self
            .source_sensitive_functions
            .contains(&span_key(node.span))
        {
            self.unsafe_function_depth += 1;
        }
    }

    fn exit_arrow_function_expression(
        &mut self,
        node: &mut ArrowFunctionExpression<'a>,
        _context: &mut TraverseCtx<'a, ()>,
    ) {
        self.exit_source_function(node.span);
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

    fn enter_try_statement(
        &mut self,
        node: &mut TryStatement<'a>,
        _context: &mut TraverseCtx<'a, ()>,
    ) {
        self.enter_try(node);
    }

    fn exit_try_statement(
        &mut self,
        _node: &mut TryStatement<'a>,
        _context: &mut TraverseCtx<'a, ()>,
    ) {
    }

    fn enter_for_in_statement(
        &mut self,
        node: &mut ForInStatement<'a>,
        _context: &mut TraverseCtx<'a, ()>,
    ) {
        self.enter_loop(node.span, &node.right, "for-in");
    }

    fn exit_for_in_statement(
        &mut self,
        _node: &mut ForInStatement<'a>,
        _context: &mut TraverseCtx<'a, ()>,
    ) {
    }

    fn enter_for_of_statement(
        &mut self,
        node: &mut ForOfStatement<'a>,
        _context: &mut TraverseCtx<'a, ()>,
    ) {
        self.enter_loop(node.span, &node.right, "for-of");
    }

    fn exit_for_of_statement(
        &mut self,
        _node: &mut ForOfStatement<'a>,
        _context: &mut TraverseCtx<'a, ()>,
    ) {
    }
}

fn collect_extended_branches<'a>(
    allocator: &'a Allocator,
    program: &mut Program<'a>,
    source: &str,
    file: &str,
    source_sensitive_functions: &HashSet<SpanKey>,
) -> ExtendedAnalysis {
    let mut collector = ExtendedCollector {
        source,
        file,
        source_sensitive_functions,
        unsafe_function_depth: 0,
        with_depth: 0,
        analysis: ExtendedAnalysis::default(),
    };
    traverse_mut(&mut collector, allocator, program, Default::default(), ());
    collector.analysis
}

struct DefaultCollector<'s> {
    source: &'s str,
    file: &'s str,
    source_sensitive_functions: &'s HashSet<SpanKey>,
    unsafe_function_depth: usize,
    with_depth: usize,
    analysis: DefaultAnalysis,
}

impl DefaultCollector<'_> {
    fn unsafe_context(&self) -> bool {
        self.unsafe_function_depth > 0 || self.with_depth > 0
    }

    fn target(
        &mut self,
        span: Span,
        left: &BindingPattern<'_>,
        right: &Expression<'_>,
    ) -> DefaultTarget {
        let id = stable_id(self.source, self.file, "default-value", span, "");
        let default_id = format!("{id}:default");
        let provided_id = format!("{id}:provided");
        let (line, column) = line_and_utf16_column(self.source, span.start as usize);
        self.analysis.branches.push(CandidateBranch {
            id,
            kind: "default-value".to_string(),
            file: self.file.to_string(),
            line,
            column,
            source: source_slice(self.source, span).to_string(),
            alternatives: vec![
                CandidateBranchAlternative {
                    id: default_id.clone(),
                    label: "default evaluated".to_string(),
                },
                CandidateBranchAlternative {
                    id: provided_id.clone(),
                    label: "value provided".to_string(),
                },
            ],
        });
        DefaultTarget {
            default_id,
            provided_id,
            inferred_name: binding_identifier_name(left)
                .filter(|_| expression_is_anonymous_definition(right)),
        }
    }

    fn collect_binding_pattern(&mut self, pattern: &BindingPattern<'_>, parameter: bool) {
        match pattern {
            BindingPattern::AssignmentPattern(assignment) => {
                let target = self.target(assignment.span, &assignment.left, &assignment.right);
                if parameter {
                    self.analysis
                        .parameter_targets
                        .insert(span_key(assignment.span), target);
                } else {
                    self.analysis
                        .binding_targets
                        .insert(span_key(assignment.span), target);
                }
                self.collect_binding_pattern(&assignment.left, parameter);
            }
            BindingPattern::ObjectPattern(object) => {
                for property in &object.properties {
                    self.collect_binding_pattern(&property.value, parameter);
                }
                if let Some(rest) = &object.rest {
                    self.collect_binding_pattern(&rest.argument, parameter);
                }
            }
            BindingPattern::ArrayPattern(array) => {
                for element in array.elements.iter().flatten() {
                    self.collect_binding_pattern(element, parameter);
                }
                if let Some(rest) = &array.rest {
                    self.collect_binding_pattern(&rest.argument, parameter);
                }
            }
            BindingPattern::BindingIdentifier(_) => {}
        }
    }

    fn collect_parameters(&mut self, parameters: &FormalParameters<'_>) {
        for parameter in &parameters.items {
            if let Some(initializer) = &parameter.initializer {
                let target = self.target(parameter.span, &parameter.pattern, initializer);
                self.analysis
                    .parameter_targets
                    .insert(span_key(parameter.span), target);
            }
            self.collect_binding_pattern(&parameter.pattern, true);
        }
        if let Some(rest) = &parameters.rest {
            self.collect_binding_pattern(&rest.rest.argument, true);
        }
    }

    fn has_binding_default(pattern: &BindingPattern<'_>) -> bool {
        match pattern {
            BindingPattern::AssignmentPattern(_) => true,
            BindingPattern::ObjectPattern(object) => {
                object
                    .properties
                    .iter()
                    .any(|property| Self::has_binding_default(&property.value))
                    || object
                        .rest
                        .as_ref()
                        .is_some_and(|rest| Self::has_binding_default(&rest.argument))
            }
            BindingPattern::ArrayPattern(array) => {
                array
                    .elements
                    .iter()
                    .flatten()
                    .any(Self::has_binding_default)
                    || array
                        .rest
                        .as_ref()
                        .is_some_and(|rest| Self::has_binding_default(&rest.argument))
            }
            BindingPattern::BindingIdentifier(_) => false,
        }
    }

    fn exit_source_function(&mut self, span: Span) {
        if self.source_sensitive_functions.contains(&span_key(span)) {
            self.unsafe_function_depth -= 1;
        }
    }
}

impl<'a> Traverse<'a, ()> for DefaultCollector<'_> {
    fn enter_function(&mut self, node: &mut Function<'a>, _context: &mut TraverseCtx<'a, ()>) {
        if self
            .source_sensitive_functions
            .contains(&span_key(node.span))
        {
            self.unsafe_function_depth += 1;
            return;
        }
        if !self.unsafe_context() && node.body.is_some() {
            self.collect_parameters(&node.params);
        }
    }

    fn exit_function(&mut self, node: &mut Function<'a>, _context: &mut TraverseCtx<'a, ()>) {
        self.exit_source_function(node.span);
    }

    fn enter_arrow_function_expression(
        &mut self,
        node: &mut ArrowFunctionExpression<'a>,
        _context: &mut TraverseCtx<'a, ()>,
    ) {
        if self
            .source_sensitive_functions
            .contains(&span_key(node.span))
        {
            self.unsafe_function_depth += 1;
            return;
        }
        if !self.unsafe_context() {
            self.collect_parameters(&node.params);
        }
    }

    fn exit_arrow_function_expression(
        &mut self,
        node: &mut ArrowFunctionExpression<'a>,
        _context: &mut TraverseCtx<'a, ()>,
    ) {
        self.exit_source_function(node.span);
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

    fn enter_variable_declaration(
        &mut self,
        node: &mut VariableDeclaration<'a>,
        context: &mut TraverseCtx<'a, ()>,
    ) {
        if self.unsafe_context() {
            return;
        }
        let has_default = node
            .declarations
            .iter()
            .any(|declaration| Self::has_binding_default(&declaration.id));
        if !has_default {
            return;
        }
        if matches!(
            context.ancestors().next(),
            Some(Ancestor::ForStatementInit(_))
        ) {
            let (line, column) = line_and_utf16_column(self.source, node.span.start as usize);
            self.analysis.limitations.push(CandidateLimitation {
                id: stable_id(
                    self.source,
                    self.file,
                    "dynamic-code",
                    node.span,
                    "for-init-default",
                ),
                kind: "dynamic-code".to_string(),
                file: self.file.to_string(),
                line,
                column,
                source: source_slice(self.source, node.span).to_string(),
                reason: "destructuring defaults in a classic for initializer cannot yet be finalized without restructuring control flow".to_string(),
            });
            return;
        }
        for declaration in &node.declarations {
            self.collect_binding_pattern(&declaration.id, false);
        }
    }
}

fn collect_default_branches<'a>(
    allocator: &'a Allocator,
    program: &mut Program<'a>,
    source: &str,
    file: &str,
    source_sensitive_functions: &HashSet<SpanKey>,
) -> DefaultAnalysis {
    let mut collector = DefaultCollector {
        source,
        file,
        source_sensitive_functions,
        unsafe_function_depth: 0,
        with_depth: 0,
        analysis: DefaultAnalysis::default(),
    };
    traverse_mut(&mut collector, allocator, program, Default::default(), ());
    collector.analysis
}

struct OptionalCallCollector<'s> {
    source: &'s str,
    file: &'s str,
    source_sensitive_functions: &'s HashSet<SpanKey>,
    unsafe_function_depth: usize,
    with_depth: usize,
    chain_roots: Vec<SpanKey>,
    analysis: OptionalCallAnalysis,
}

impl OptionalCallCollector<'_> {
    fn unsafe_context(&self) -> bool {
        self.unsafe_function_depth > 0 || self.with_depth > 0
    }

    fn exit_source_function(&mut self, span: Span) {
        if self.source_sensitive_functions.contains(&span_key(span)) {
            self.unsafe_function_depth -= 1;
        }
    }
}

impl<'a> Traverse<'a, ()> for OptionalCallCollector<'_> {
    fn enter_function(&mut self, node: &mut Function<'a>, _context: &mut TraverseCtx<'a, ()>) {
        if self
            .source_sensitive_functions
            .contains(&span_key(node.span))
        {
            self.unsafe_function_depth += 1;
        }
    }

    fn exit_function(&mut self, node: &mut Function<'a>, _context: &mut TraverseCtx<'a, ()>) {
        self.exit_source_function(node.span);
    }

    fn enter_arrow_function_expression(
        &mut self,
        node: &mut ArrowFunctionExpression<'a>,
        _context: &mut TraverseCtx<'a, ()>,
    ) {
        if self
            .source_sensitive_functions
            .contains(&span_key(node.span))
        {
            self.unsafe_function_depth += 1;
        }
    }

    fn exit_arrow_function_expression(
        &mut self,
        node: &mut ArrowFunctionExpression<'a>,
        _context: &mut TraverseCtx<'a, ()>,
    ) {
        self.exit_source_function(node.span);
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

    fn enter_chain_expression(
        &mut self,
        node: &mut ChainExpression<'a>,
        context: &mut TraverseCtx<'a, ()>,
    ) {
        let root = match context.ancestors().next() {
            Some(Ancestor::UnaryExpressionArgument(parent))
                if *parent.operator() == UnaryOperator::Delete =>
            {
                span_key(*parent.span())
            }
            _ => span_key(node.span),
        };
        self.chain_roots.push(root);
    }

    fn exit_chain_expression(
        &mut self,
        _node: &mut ChainExpression<'a>,
        _context: &mut TraverseCtx<'a, ()>,
    ) {
        self.chain_roots.pop();
    }

    fn enter_call_expression(
        &mut self,
        node: &mut CallExpression<'a>,
        _context: &mut TraverseCtx<'a, ()>,
    ) {
        if !node.optional || self.unsafe_context() {
            return;
        }
        let root = *self
            .chain_roots
            .last()
            .expect("an optional call must be enclosed by a chain expression");
        let id = stable_id(self.source, self.file, "optional-chain", node.span, "call");
        let short_id = format!("{id}:short");
        let continued_id = format!("{id}:continued");
        let (line, column) = line_and_utf16_column(self.source, node.span.start as usize);
        self.analysis.sites.insert(
            span_key(node.span),
            (short_id.clone(), continued_id.clone()),
        );
        self.analysis
            .roots
            .entry(root)
            .or_default()
            .push(span_key(node.span));
        self.analysis.branches.push(CandidateBranch {
            id,
            kind: "optional-chain".to_string(),
            file: self.file.to_string(),
            line,
            column,
            source: source_slice(self.source, node.span).to_string(),
            alternatives: vec![
                CandidateBranchAlternative {
                    id: short_id,
                    label: "nullish / short-circuited".to_string(),
                },
                CandidateBranchAlternative {
                    id: continued_id,
                    label: "non-nullish / continued".to_string(),
                },
            ],
        });
    }
}

fn collect_optional_call_branches<'a>(
    allocator: &'a Allocator,
    program: &mut Program<'a>,
    source: &str,
    file: &str,
    source_sensitive_functions: &HashSet<SpanKey>,
) -> OptionalCallAnalysis {
    let mut collector = OptionalCallCollector {
        source,
        file,
        source_sensitive_functions,
        unsafe_function_depth: 0,
        with_depth: 0,
        chain_roots: Vec::new(),
        analysis: OptionalCallAnalysis::default(),
    };
    traverse_mut(&mut collector, allocator, program, Default::default(), ());
    collector.analysis
}

struct OptionalMemberCollector<'s> {
    source: &'s str,
    file: &'s str,
    source_sensitive_functions: &'s HashSet<SpanKey>,
    unsafe_function_depth: usize,
    with_depth: usize,
    analysis: OptionalMemberAnalysis,
}

impl OptionalMemberCollector<'_> {
    fn record(&mut self, span: Span, optional: bool) {
        if !optional || self.unsafe_function_depth > 0 || self.with_depth > 0 {
            return;
        }
        let id = stable_id(self.source, self.file, "optional-chain", span, "");
        let short_id = format!("{id}:short");
        let continued_id = format!("{id}:continued");
        let (line, column) = line_and_utf16_column(self.source, span.start as usize);
        self.analysis
            .targets
            .insert(span_key(span), (short_id.clone(), continued_id.clone()));
        self.analysis.branches.push(CandidateBranch {
            id,
            kind: "optional-chain".to_string(),
            file: self.file.to_string(),
            line,
            column,
            source: source_slice(self.source, span).to_string(),
            alternatives: vec![
                CandidateBranchAlternative {
                    id: short_id,
                    label: "nullish / short-circuited".to_string(),
                },
                CandidateBranchAlternative {
                    id: continued_id,
                    label: "non-nullish / continued".to_string(),
                },
            ],
        });
    }

    fn exit_source_function(&mut self, span: Span) {
        if self.source_sensitive_functions.contains(&span_key(span)) {
            self.unsafe_function_depth -= 1;
        }
    }
}

impl<'a> Traverse<'a, ()> for OptionalMemberCollector<'_> {
    fn enter_function(&mut self, node: &mut Function<'a>, _context: &mut TraverseCtx<'a, ()>) {
        if self
            .source_sensitive_functions
            .contains(&span_key(node.span))
        {
            self.unsafe_function_depth += 1;
        }
    }

    fn exit_function(&mut self, node: &mut Function<'a>, _context: &mut TraverseCtx<'a, ()>) {
        self.exit_source_function(node.span);
    }

    fn enter_arrow_function_expression(
        &mut self,
        node: &mut ArrowFunctionExpression<'a>,
        _context: &mut TraverseCtx<'a, ()>,
    ) {
        if self
            .source_sensitive_functions
            .contains(&span_key(node.span))
        {
            self.unsafe_function_depth += 1;
        }
    }

    fn exit_arrow_function_expression(
        &mut self,
        node: &mut ArrowFunctionExpression<'a>,
        _context: &mut TraverseCtx<'a, ()>,
    ) {
        self.exit_source_function(node.span);
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

    fn enter_computed_member_expression(
        &mut self,
        node: &mut ComputedMemberExpression<'a>,
        _context: &mut TraverseCtx<'a, ()>,
    ) {
        self.record(node.span, node.optional);
    }

    fn enter_static_member_expression(
        &mut self,
        node: &mut StaticMemberExpression<'a>,
        _context: &mut TraverseCtx<'a, ()>,
    ) {
        self.record(node.span, node.optional);
    }

    fn enter_private_field_expression(
        &mut self,
        node: &mut PrivateFieldExpression<'a>,
        _context: &mut TraverseCtx<'a, ()>,
    ) {
        self.record(node.span, node.optional);
    }
}

fn collect_optional_member_branches<'a>(
    allocator: &'a Allocator,
    program: &mut Program<'a>,
    source: &str,
    file: &str,
    source_sensitive_functions: &HashSet<SpanKey>,
) -> OptionalMemberAnalysis {
    let mut collector = OptionalMemberCollector {
        source,
        file,
        source_sensitive_functions,
        unsafe_function_depth: 0,
        with_depth: 0,
        analysis: OptionalMemberAnalysis::default(),
    };
    traverse_mut(&mut collector, allocator, program, Default::default(), ());
    collector.analysis
}

struct LogicalAssignmentCollector<'s> {
    source: &'s str,
    file: &'s str,
    source_sensitive_functions: &'s HashSet<SpanKey>,
    unsafe_function_depth: usize,
    with_depth: usize,
    analysis: LogicalAssignmentAnalysis,
}

impl LogicalAssignmentCollector<'_> {
    fn exit_source_function(&mut self, span: Span) {
        if self.source_sensitive_functions.contains(&span_key(span)) {
            self.unsafe_function_depth -= 1;
        }
    }
}

impl<'a> Traverse<'a, ()> for LogicalAssignmentCollector<'_> {
    fn enter_function(&mut self, node: &mut Function<'a>, _context: &mut TraverseCtx<'a, ()>) {
        if self
            .source_sensitive_functions
            .contains(&span_key(node.span))
        {
            self.unsafe_function_depth += 1;
        }
    }

    fn exit_function(&mut self, node: &mut Function<'a>, _context: &mut TraverseCtx<'a, ()>) {
        self.exit_source_function(node.span);
    }

    fn enter_arrow_function_expression(
        &mut self,
        node: &mut ArrowFunctionExpression<'a>,
        _context: &mut TraverseCtx<'a, ()>,
    ) {
        if self
            .source_sensitive_functions
            .contains(&span_key(node.span))
        {
            self.unsafe_function_depth += 1;
        }
    }

    fn exit_arrow_function_expression(
        &mut self,
        node: &mut ArrowFunctionExpression<'a>,
        _context: &mut TraverseCtx<'a, ()>,
    ) {
        self.exit_source_function(node.span);
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

    fn exit_assignment_expression(
        &mut self,
        node: &mut AssignmentExpression<'a>,
        _context: &mut TraverseCtx<'a, ()>,
    ) {
        if self.unsafe_function_depth > 0 || self.with_depth > 0 || !node.operator.is_logical() {
            return;
        }
        let operator = match node.operator {
            AssignmentOperator::LogicalAnd => "&&=",
            AssignmentOperator::LogicalOr => "||=",
            AssignmentOperator::LogicalNullish => "??=",
            _ => unreachable!("logical assignment filter must be exhaustive"),
        };
        let id = stable_id(
            self.source,
            self.file,
            "logical-assignment",
            node.span,
            operator,
        );
        let short_id = format!("{id}:short");
        let right_id = format!("{id}:right");
        let (line, column) = line_and_utf16_column(self.source, node.span.start as usize);
        self.analysis
            .targets
            .insert(span_key(node.span), (short_id.clone(), right_id.clone()));
        self.analysis.branches.push(CandidateBranch {
            id,
            kind: "logical-assignment".to_string(),
            file: self.file.to_string(),
            line,
            column,
            source: source_slice(self.source, node.span).to_string(),
            alternatives: vec![
                CandidateBranchAlternative {
                    id: short_id,
                    label: "assignment skipped".to_string(),
                },
                CandidateBranchAlternative {
                    id: right_id,
                    label: "right evaluated / assigned".to_string(),
                },
            ],
        });
    }
}

fn collect_logical_assignment_branches<'a>(
    allocator: &'a Allocator,
    program: &mut Program<'a>,
    source: &str,
    file: &str,
    source_sensitive_functions: &HashSet<SpanKey>,
) -> LogicalAssignmentAnalysis {
    let mut collector = LogicalAssignmentCollector {
        source,
        file,
        source_sensitive_functions,
        unsafe_function_depth: 0,
        with_depth: 0,
        analysis: LogicalAssignmentAnalysis::default(),
    };
    traverse_mut(&mut collector, allocator, program, Default::default(), ());
    collector.analysis
}

struct LogicalBranchCollector<'s> {
    source: &'s str,
    file: &'s str,
    decision_logical_nodes: &'s HashSet<SpanKey>,
    source_sensitive_functions: &'s HashSet<SpanKey>,
    unsafe_function_depth: usize,
    with_depth: usize,
    analysis: LogicalBranchAnalysis,
}

impl LogicalBranchCollector<'_> {
    fn exit_source_function(&mut self, span: Span) {
        if self.source_sensitive_functions.contains(&span_key(span)) {
            self.unsafe_function_depth -= 1;
        }
    }
}

impl<'a> Traverse<'a, ()> for LogicalBranchCollector<'_> {
    fn enter_function(&mut self, node: &mut Function<'a>, _context: &mut TraverseCtx<'a, ()>) {
        if self
            .source_sensitive_functions
            .contains(&span_key(node.span))
        {
            self.unsafe_function_depth += 1;
        }
    }

    fn exit_function(&mut self, node: &mut Function<'a>, _context: &mut TraverseCtx<'a, ()>) {
        self.exit_source_function(node.span);
    }

    fn enter_arrow_function_expression(
        &mut self,
        node: &mut ArrowFunctionExpression<'a>,
        _context: &mut TraverseCtx<'a, ()>,
    ) {
        if self
            .source_sensitive_functions
            .contains(&span_key(node.span))
        {
            self.unsafe_function_depth += 1;
        }
    }

    fn exit_arrow_function_expression(
        &mut self,
        node: &mut ArrowFunctionExpression<'a>,
        _context: &mut TraverseCtx<'a, ()>,
    ) {
        self.exit_source_function(node.span);
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

    fn exit_logical_expression(
        &mut self,
        node: &mut LogicalExpression<'a>,
        _context: &mut TraverseCtx<'a, ()>,
    ) {
        if self.unsafe_function_depth > 0
            || self.with_depth > 0
            || self.decision_logical_nodes.contains(&span_key(node.span))
        {
            return;
        }
        let operator = match node.operator {
            LogicalOperator::And => "&&",
            LogicalOperator::Or => "||",
            LogicalOperator::Coalesce => "??",
        };
        let id = stable_id(self.source, self.file, "logical-value", node.span, operator);
        let short_id = format!("{id}:short");
        let right_id = format!("{id}:right");
        let (line, column) = line_and_utf16_column(self.source, node.span.start as usize);
        self.analysis
            .logical_targets
            .insert(span_key(node.span), (short_id.clone(), right_id.clone()));
        self.analysis.branches.push(CandidateBranch {
            id,
            kind: "logical-value".to_string(),
            file: self.file.to_string(),
            line,
            column,
            source: source_slice(self.source, node.span).to_string(),
            alternatives: vec![
                CandidateBranchAlternative {
                    id: short_id,
                    label: "short-circuit / left selected".to_string(),
                },
                CandidateBranchAlternative {
                    id: right_id,
                    label: "right evaluated / selected".to_string(),
                },
            ],
        });
    }
}

fn collect_logical_value_branches<'a>(
    allocator: &'a Allocator,
    program: &mut Program<'a>,
    source: &str,
    file: &str,
    decision_logical_nodes: &HashSet<SpanKey>,
    source_sensitive_functions: &HashSet<SpanKey>,
) -> LogicalBranchAnalysis {
    let mut collector = LogicalBranchCollector {
        source,
        file,
        decision_logical_nodes,
        source_sensitive_functions,
        unsafe_function_depth: 0,
        with_depth: 0,
        analysis: LogicalBranchAnalysis::default(),
    };
    traverse_mut(&mut collector, allocator, program, Default::default(), ());
    collector.analysis
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
            let map = output.map.as_ref().expect("candidate source map");
            assert_eq!(map["version"], 3);
            assert_eq!(map["sources"][0], file);
            assert_eq!(map["sourcesContent"][0], source);
            assert!(
                map["mappings"]
                    .as_str()
                    .is_some_and(|value| !value.is_empty())
            );
            let allocator = Allocator::default();
            let source_type = SourceType::from_path(file).unwrap();
            let reparsed = Parser::new(&allocator, &output.code, source_type).parse();
            assert!(reparsed.errors.is_empty(), "{file}: {:?}", reparsed.errors);
        }
    }

    #[test]
    fn preserves_comment_payloads_byte_for_byte() {
        let comment = "/*---\ndescription: >\n  nested indentation is data\ninfo: |\n  first\n    second\n---*/";
        let source = format!("{comment}\nfunction run() {{ return 1; }}\n");
        let output = instrument_candidate(&source, "app/comments.js").unwrap();
        assert!(output.code.contains(comment), "{}", output.code);
    }

    #[test]
    fn source_map_destinations_follow_restored_comments() {
        let source = "function run() {\n  // first omitted comment\n  // second omitted comment\n  return 1;\n}\n";
        let output = analyze_candidate(source, "app/comment-map.js").unwrap();
        let encoded = serde_json::to_string(output.map.as_ref().unwrap()).unwrap();
        let map = oxc_sourcemap::SourceMap::from_json_string(&encoded).unwrap();
        let return_token = map
            .get_tokens()
            .find(|token| token.get_src_line() == 3 && token.get_src_col() == 2)
            .expect("return token mapping");
        let offset = Utf16LineIndex::new(&output.code).byte_offset(
            return_token.get_dst_line() as usize,
            return_token.get_dst_col() as usize,
        );
        assert!(
            output.code[offset..].starts_with("return"),
            "{}",
            output.code
        );
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
            "complete-js-manifest-and-differential-probes-candidate"
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
    fn wraps_framework_request_exports_without_changing_the_public_api() {
        for (file, source, export_prefix) in [
            (
                "app/routes/example.ts",
                "export const loader = async ({ request }) => request.url;",
                "export const loader = ",
            ),
            (
                "app/routes/example.ts",
                "export async function action({ request }) { return request.method; }",
                "export const action = ",
            ),
            (
                "app/api/items/route.ts",
                "export function GET(request) { return Response.json({ url: request.url }); }",
                "export const GET = ",
            ),
            (
                "app/routes/example.ts",
                "export { generateAction as action } from './generateAction';",
                "export const action = ",
            ),
            (
                "app/entry.server.tsx",
                "export default async function handleRequest(request) { return request.url; }",
                "export default ",
            ),
        ] {
            let output = instrument_candidate(source, file).unwrap();
            let runtime = output.runtime.as_ref().expect("runtime binding");
            assert!(
                output
                    .code
                    .contains(&format!("{export_prefix}{}(", runtime.with_request_phase)),
                "{file}: {}",
                output.code
            );
            assert!(
                output.code.contains(&format!(
                    "withRequestPhase as {}",
                    runtime.with_request_phase
                )),
                "{file}: {}",
                output.code
            );
            let allocator = Allocator::default();
            let reparsed = Parser::new(
                &allocator,
                &output.code,
                SourceType::from_path(file).unwrap(),
            )
            .parse();
            assert!(reparsed.errors.is_empty(), "{file}: {:?}", reparsed.errors);
        }
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
