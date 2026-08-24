//! First oxc-backed vertical slice of the Rust JavaScript instrumenter.
//!
//! This candidate reports and instruments the frozen control-decision surface.
//! It is not exposed by the CLI and cannot claim a complete denominator until
//! the remaining reference transformations are ported.

use std::{fmt::Write, path::Path};

use oxc_allocator::{Allocator, TakeIn};
use oxc_ast::{
    AstBuilder, NONE,
    ast::{
        Argument, AssignmentTarget, ConditionalExpression, DoWhileStatement, Expression,
        ForStatement, FunctionBody, IfStatement, Program, Statement, VariableDeclarationKind,
        WhileStatement,
    },
};
use oxc_ast_visit::{Visit, VisitMut, walk_mut};
use oxc_codegen::Codegen;
use oxc_parser::Parser;
use oxc_span::{GetSpan, SourceType, Span};
use oxc_syntax::{
    number::NumberBase,
    operator::{AssignmentOperator, LogicalOperator},
};
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
pub struct CandidateOutput {
    pub engine: String,
    pub complete: bool,
    pub supported_surface: String,
    pub code: String,
    pub decisions: Vec<CandidateDecision>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<CandidateRuntime>,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateRuntime {
    pub mcdc_end_v2: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandidateError {
    UnknownSourceType(String),
    Parse(Vec<String>),
}

pub fn analyze_candidate(source: &str, file: &str) -> Result<CandidateOutput, CandidateError> {
    let source_type = SourceType::from_path(Path::new(file))
        .map_err(|error| CandidateError::UnknownSourceType(error.to_string()))?;
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source, source_type).parse();
    if !parsed.errors.is_empty() {
        return Err(CandidateError::Parse(
            parsed
                .errors
                .into_iter()
                .map(|error| format!("{error:?}"))
                .collect(),
        ));
    }

    let mut collector = DecisionCollector {
        source,
        file,
        decisions: Vec::new(),
    };
    collector.visit_program(&parsed.program);
    let generated = Codegen::new().build(&parsed.program).code;
    Ok(CandidateOutput {
        engine: "rust-oxc".to_string(),
        complete: false,
        supported_surface: "control-decision-manifest-v1".to_string(),
        code: generated,
        decisions: collector.decisions,
        runtime: None,
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

    let mut collector = DecisionCollector {
        source,
        file,
        decisions: Vec::new(),
    };
    collector.visit_program(&parsed.program);

    let mut names = CandidateNames::new(source);
    let runtime_name = names.allocate("__supercovMcdcEndV2");
    let ast = AstBuilder::new(&allocator);
    let mut transformer = ControlProbeV2Transformer {
        ast,
        file,
        runtime_name: runtime_name.clone(),
        names,
        scope_declarations: Vec::new(),
        decision_index: 0,
        wider_decisions: 0,
    };
    transformer.visit_program(&mut parsed.program);

    let mut limitations = vec![
        "only if, ternary, while, do-while, and classic-for decisions are instrumented"
            .to_string(),
        "coverage points, value branches, extended branch obligations, and runtime registration remain on the TypeScript reference"
            .to_string(),
        "with-environment and function-source semantic-safety exclusions are not yet ported"
            .to_string(),
        "candidate runtime binding is differential-only and is not exposed by the public CLI"
            .to_string(),
    ];
    if transformer.wider_decisions > 0 {
        limitations.push(format!(
            "{} control decision(s) exceeded 32 conditions and require the exact v1 fallback",
            transformer.wider_decisions
        ));
    }

    Ok(CandidateOutput {
        engine: "rust-oxc".to_string(),
        complete: false,
        supported_surface: "control-decision-probe-v2-candidate".to_string(),
        code: Codegen::new().build(&parsed.program).code,
        decisions: collector.decisions,
        runtime: Some(CandidateRuntime {
            mcdc_end_v2: runtime_name,
        }),
        limitations,
    })
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
    runtime_name: String,
    names: CandidateNames<'s>,
    scope_declarations: Vec<Vec<String>>,
    decision_index: usize,
    wider_decisions: usize,
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
            self.wider_decisions += 1;
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
            self.identifier(&self.runtime_name),
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
            "control-decision-probe-v2-candidate"
        );
        let runtime = output.runtime.expect("candidate runtime binding");
        assert!(output.code.contains(&runtime.mcdc_end_v2));
        assert!(output.code.contains("_supercovMcdcFrame"));
        assert!(output.code.contains("_supercovMcdcResult"));
        assert!(output.code.contains("+= _supercovMcdcValue"));
        assert_eq!(output.decisions.len(), 1);

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
    fn leaves_wider_decisions_explicitly_uninstrumented_for_exact_v1_fallback() {
        let predicate = (0..33)
            .map(|index| format!("c{index}"))
            .collect::<Vec<_>>()
            .join(" && ");
        let source = format!("if ({predicate}) work();");
        let output = instrument_candidate(&source, "app/wide.js").unwrap();
        assert_eq!(output.decisions[0].conditions.len(), 33);
        assert!(
            output
                .limitations
                .iter()
                .any(|limitation| limitation.contains("exact v1 fallback"))
        );
        let runtime = output.runtime.expect("candidate runtime binding");
        assert!(!output.code.contains(&format!("{}(", runtime.mcdc_end_v2)));
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
