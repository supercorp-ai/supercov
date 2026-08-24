//! First oxc-backed vertical slice of the Rust JavaScript instrumenter.
//!
//! This candidate intentionally reports only the frozen metadata for `if`
//! decisions. It is not exposed by the CLI and cannot claim a complete
//! denominator until the remaining reference transformations are ported.

use std::{fmt::Write, path::Path};

use oxc_allocator::Allocator;
use oxc_ast::ast::{Expression, IfStatement};
use oxc_ast_visit::{Visit, walk};
use oxc_codegen::Codegen;
use oxc_parser::Parser;
use oxc_span::{GetSpan, SourceType, Span};
use oxc_syntax::operator::LogicalOperator;
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
    pub limitations: Vec<String>,
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
        supported_surface: "if-decision-manifest-v1".to_string(),
        code: generated,
        decisions: collector.decisions,
        limitations: vec![
            "candidate emits metadata only; probe insertion is not implemented".to_string(),
            "only if-statement decision metadata is currently compared".to_string(),
        ],
    })
}

struct DecisionCollector<'s> {
    source: &'s str,
    file: &'s str,
    decisions: Vec<CandidateDecision>,
}

impl<'a> Visit<'a> for DecisionCollector<'_> {
    fn visit_if_statement(&mut self, statement: &IfStatement<'a>) {
        let mut condition_spans = Vec::new();
        collect_conditions(&statement.test, &mut condition_spans);
        let span = statement.test.span();
        let (line, column) = line_and_utf16_column(self.source, span.start as usize);
        self.decisions.push(CandidateDecision {
            id: stable_id(self.source, self.file, "decision", span, "if"),
            file: self.file.to_string(),
            line,
            column,
            source: source_slice(self.source, span).to_string(),
            conditions: condition_spans
                .into_iter()
                .map(|condition| source_slice(self.source, condition).to_string())
                .collect(),
            kind: "if".to_string(),
        });
        walk::walk_if_statement(self, statement);
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
