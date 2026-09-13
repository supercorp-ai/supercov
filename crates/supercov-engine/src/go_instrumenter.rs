//! Supercov-owned Go parsing and obligation discovery.
//!
//! Go's own `go test -cover` is statement-level and knows nothing about which
//! test reached a line, which branch arm was taken, or whether a condition
//! independently affected its decision. It stays a development oracle, exactly
//! as LLVM does for Rust; the product measures from this tree.
//!
//! The denominator comes from a lossless concrete syntax tree, so every
//! obligation carries the byte range it was discovered at. A line Supercov
//! cannot anchor is not silently dropped: it is either an obligation with a
//! location or it is absent from the manifest entirely.

use std::collections::BTreeMap;

use tree_sitter::{Node, Parser};

use crate::coverage_analysis::PointKind;
use crate::coverage_report::{
    BranchAlternativeMeta, BranchMeta, CoverageManifest, DecisionMeta, PointMeta,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoInstrumenterError {
    Parse(String),
}

impl std::fmt::Display for GoInstrumenterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GoInstrumenterError::Parse(detail) => write!(f, "Go parse error: {detail}"),
        }
    }
}

/// Where a probe has to observe, paired with the obligation it answers for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoProbeTarget {
    Statement {
        id: String,
    },
    Function {
        id: String,
    },
    /// One arm of a branch: the runtime records which alternative ran.
    Alternative {
        branch: String,
        alternative: String,
    },
    /// One condition inside a decision, with its position in the tree, so the
    /// runtime can report the vector MC/DC needs rather than a single bit.
    Condition {
        decision: String,
        index: usize,
    },
    /// The decision's own outcome.
    Outcome {
        decision: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoProbe {
    pub id: u64,
    pub target: GoProbeTarget,
    /// Byte offset the probe observes, for the rewriter that comes next.
    pub at: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GoFileObligations {
    pub manifest: CoverageManifest,
    pub probes: BTreeMap<u64, GoProbe>,
}

pub fn parse(source: &str) -> Result<tree_sitter::Tree, GoInstrumenterError> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_go::LANGUAGE.into())
        .map_err(|error| GoInstrumenterError::Parse(error.to_string()))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| GoInstrumenterError::Parse("parser returned no tree".into()))?;
    if tree.root_node().has_error() {
        return Err(GoInstrumenterError::Parse(format!(
            "syntax error near byte {}",
            first_error_offset(tree.root_node()).unwrap_or(0)
        )));
    }
    Ok(tree)
}

fn first_error_offset(node: Node) -> Option<usize> {
    if node.is_error() || node.is_missing() {
        return Some(node.start_byte());
    }
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .filter(|child| child.has_error())
        .find_map(first_error_offset)
}

/// Statements Go nests directly inside a block. Declarations that cannot
/// execute -- an import, a type -- carry no coverage question and are absent
/// rather than counted as uncovered.
fn is_statement(kind: &str) -> bool {
    matches!(
        kind,
        "assignment_statement"
            | "break_statement"
            | "const_declaration"
            | "continue_statement"
            | "dec_statement"
            | "defer_statement"
            | "expression_statement"
            | "expression_switch_statement"
            | "fallthrough_statement"
            | "for_statement"
            | "go_statement"
            | "goto_statement"
            | "if_statement"
            | "inc_statement"
            | "labeled_statement"
            | "return_statement"
            | "select_statement"
            | "send_statement"
            | "short_var_declaration"
            | "type_switch_statement"
            | "var_declaration"
    )
}

struct Collector<'a> {
    file: &'a str,
    source: &'a str,
    next_probe: &'a mut u64,
    points: Vec<PointMeta>,
    branches: Vec<BranchMeta>,
    decisions: Vec<DecisionMeta>,
    probes: BTreeMap<u64, GoProbe>,
    counter: usize,
}

impl<'a> Collector<'a> {
    fn id(&mut self, prefix: &str) -> String {
        self.counter += 1;
        format!("{prefix}{}", self.counter)
    }

    fn probe(&mut self, target: GoProbeTarget, at: usize) {
        *self.next_probe += 1;
        let id = *self.next_probe;
        self.probes.insert(id, GoProbe { id, target, at });
    }

    /// Line and column are one-based, matching every other frontend.
    fn position(&self, node: Node) -> (usize, usize) {
        let start = node.start_position();
        (start.row + 1, start.column + 1)
    }

    fn text(&self, node: Node) -> String {
        let raw = &self.source[node.byte_range()];
        let line = raw.lines().next().unwrap_or("");
        line.trim().to_owned()
    }

    fn add_point(&mut self, node: Node, kind: PointKind, label: Option<String>) {
        let (line, column) = self.position(node);
        let id = self.id(match kind {
            PointKind::Function => "f",
            PointKind::Statement => "s",
        });
        let target = match kind {
            PointKind::Function => GoProbeTarget::Function { id: id.clone() },
            PointKind::Statement => GoProbeTarget::Statement { id: id.clone() },
        };
        self.probe(target, node.start_byte());
        self.points.push(PointMeta {
            id,
            kind,
            file: self.file.to_owned(),
            line,
            column,
            source: self.text(node),
            label,
        });
    }

    fn add_branch(&mut self, node: Node, kind: &str, labels: &[&str]) -> String {
        let (line, column) = self.position(node);
        let id = self.id("b");
        let alternatives = labels
            .iter()
            .map(|label| {
                let alternative = format!("{id}.{label}");
                self.probe(
                    GoProbeTarget::Alternative {
                        branch: id.clone(),
                        alternative: alternative.clone(),
                    },
                    node.start_byte(),
                );
                BranchAlternativeMeta {
                    id: alternative,
                    label: (*label).to_owned(),
                }
            })
            .collect();
        self.branches.push(BranchMeta {
            id: id.clone(),
            kind: kind.to_owned(),
            file: self.file.to_owned(),
            line,
            column,
            source: self.text(node),
            alternatives,
        });
        id
    }

    /// Record a boolean expression as a decision when it has more than one
    /// condition. A single condition needs no MC/DC obligation: its branch
    /// outcomes already say everything independence could.
    fn add_decision(&mut self, node: Node, kind: &str) {
        let conditions = conditions_of(node, self.source);
        if conditions.len() < 2 {
            return;
        }
        let (line, column) = self.position(node);
        let id = self.id("d");
        for (index, _) in conditions.iter().enumerate() {
            self.probe(
                GoProbeTarget::Condition {
                    decision: id.clone(),
                    index,
                },
                node.start_byte(),
            );
        }
        self.probe(
            GoProbeTarget::Outcome {
                decision: id.clone(),
            },
            node.start_byte(),
        );
        self.decisions.push(DecisionMeta {
            id,
            file: self.file.to_owned(),
            line,
            column,
            source: self.text(node),
            conditions,
            kind: kind.to_owned(),
        });
    }

    fn walk(&mut self, node: Node) {
        match node.kind() {
            "function_declaration" | "method_declaration" | "func_literal" => {
                let label = node
                    .child_by_field_name("name")
                    .map(|name| self.source[name.byte_range()].to_owned());
                self.add_point(node, PointKind::Function, label);
            }
            "if_statement" => {
                // An `if` without an else still has two outcomes: the body ran,
                // or control passed it by. Recording only the taken arm would
                // make an untested guard look exercised.
                self.add_branch(node, "if", &["true", "false"]);
                if let Some(condition) = node.child_by_field_name("condition") {
                    self.add_decision(condition, "if");
                }
            }
            "for_statement" => {
                self.add_branch(node, "loop", &["entered", "skipped"]);
                if let Some(clause) = node.child_by_field_name("condition") {
                    self.add_decision(clause, "loop");
                }
            }
            "expression_switch_statement" | "type_switch_statement" | "select_statement" => {
                let kind = match node.kind() {
                    "expression_switch_statement" => "switch",
                    "type_switch_statement" => "type-switch",
                    _ => "select",
                };
                let mut labels = Vec::new();
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    match child.kind() {
                        "expression_case" | "type_case" | "communication_case" => {
                            labels.push(self.text(child))
                        }
                        "default_case" => labels.push("default".to_owned()),
                        _ => {}
                    }
                }
                // A switch with no default can fall through every case, which
                // is an outcome a reader has to be able to see.
                if !labels.iter().any(|label| label == "default") {
                    labels.push("no case matched".to_owned());
                }
                let borrowed = labels.iter().map(String::as_str).collect::<Vec<_>>();
                self.add_branch(node, kind, &borrowed);
            }
            kind if is_statement(kind) => {
                self.add_point(node, PointKind::Statement, None);
            }
            _ => {}
        }
        // `if`, `for` and `switch` are statements too, and their own point is
        // what says the construct was reached at all.
        if matches!(
            node.kind(),
            "if_statement"
                | "for_statement"
                | "expression_switch_statement"
                | "type_switch_statement"
                | "select_statement"
        ) {
            self.add_point(node, PointKind::Statement, None);
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.is_named() {
                self.walk(child);
            }
        }
    }
}

/// Flatten a boolean expression into its independent conditions.
///
/// `&&` and `||` are the only short-circuiting operators Go has, so they are
/// the only ones that split a decision. `!` negates a condition rather than
/// introducing one, and a parenthesised group is transparent.
fn conditions_of(node: Node, source: &str) -> Vec<String> {
    fn collect(node: Node, source: &str, out: &mut Vec<String>) {
        match node.kind() {
            "binary_expression" => {
                let operator = node
                    .child_by_field_name("operator")
                    .map(|op| &source[op.byte_range()])
                    .unwrap_or("");
                if operator == "&&" || operator == "||" {
                    if let Some(left) = node.child_by_field_name("left") {
                        collect(left, source, out);
                    }
                    if let Some(right) = node.child_by_field_name("right") {
                        collect(right, source, out);
                    }
                    return;
                }
                out.push(source[node.byte_range()].trim().to_owned());
            }
            "parenthesized_expression" => {
                let mut cursor = node.walk();
                match node.children(&mut cursor).find(|child| child.is_named()) {
                    Some(inner) => collect(inner, source, out),
                    None => out.push(source[node.byte_range()].trim().to_owned()),
                }
            }
            _ => out.push(source[node.byte_range()].trim().to_owned()),
        }
    }
    let mut out = Vec::new();
    collect(node, source, &mut out);
    out
}

pub fn build_go_obligations(
    file: &str,
    source: &str,
    next_probe: &mut u64,
) -> Result<GoFileObligations, GoInstrumenterError> {
    let tree = parse(source)?;
    let mut collector = Collector {
        file,
        source,
        next_probe,
        points: Vec::new(),
        branches: Vec::new(),
        decisions: Vec::new(),
        probes: BTreeMap::new(),
        counter: 0,
    };
    let mut cursor = tree.root_node().walk();
    for child in tree.root_node().children(&mut cursor) {
        if child.is_named() {
            collector.walk(child);
        }
    }
    Ok(GoFileObligations {
        manifest: CoverageManifest {
            decisions: collector.decisions,
            points: collector.points,
            branches: collector.branches,
            limitations: Vec::new(),
            unmeasured: Vec::new(),
            scope: None,
        },
        probes: collector.probes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"package main

import "fmt"

func classify(a int, b bool) string {
	if a > 10 && b {
		return "big"
	}
	for i := 0; i < a; i++ {
		fmt.Println(i)
	}
	switch {
	case a == 0:
		return "zero"
	default:
		return "small"
	}
}
"#;

    fn obligations(source: &str) -> GoFileObligations {
        let mut next = 0;
        build_go_obligations("main.go", source, &mut next).expect("obligations")
    }

    #[test]
    fn a_short_circuiting_operator_splits_a_decision_and_nothing_else_does() {
        // `&&` and `||` are the only operators that let one condition decide
        // the outcome without the other running, which is what MC/DC is about.
        // A comparison is one condition however many operands it reads.
        let go = obligations(SAMPLE);
        let decision = go
            .manifest
            .decisions
            .iter()
            .find(|d| d.kind == "if")
            .expect("the if carries a decision");
        assert_eq!(decision.conditions, ["a > 10", "b"]);

        // A single condition needs no independence obligation: the branch
        // outcomes already say everything it could.
        let simple = obligations(
            "package main\nfunc f(a int) bool {\n\tif a > 1 {\n\t\treturn true\n\t}\n\treturn false\n}\n",
        );
        assert!(
            simple.manifest.decisions.is_empty(),
            "{:?}",
            simple.manifest.decisions
        );
    }

    #[test]
    fn negation_and_parentheses_do_not_invent_conditions() {
        // `!b` is `b` negated, not a second condition, and a parenthesised
        // group is transparent. Counting either as extra would inflate the
        // MC/DC denominator with obligations no test can satisfy separately.
        let go = obligations(
            "package main\nfunc f(a int, b bool, c bool) bool {\n\tif (a > 1 || !b) && c {\n\t\treturn true\n\t}\n\treturn false\n}\n",
        );
        let decision = &go.manifest.decisions[0];
        assert_eq!(decision.conditions, ["a > 1", "!b", "c"]);
    }

    #[test]
    fn every_branching_construct_records_the_outcome_that_was_not_taken() {
        // An `if` with no else, a loop that never runs, and a switch that
        // matches nothing are all outcomes a reader has to see. Recording only
        // the arm that executed would make an untested guard look exercised.
        let go = obligations(SAMPLE);
        let by_kind = |kind: &str| {
            go.manifest
                .branches
                .iter()
                .find(|b| b.kind == kind)
                .map(|b| {
                    b.alternatives
                        .iter()
                        .map(|a| a.label.clone())
                        .collect::<Vec<_>>()
                })
        };
        assert_eq!(by_kind("if").unwrap(), ["true", "false"]);
        assert_eq!(by_kind("loop").unwrap(), ["entered", "skipped"]);
        let switch = by_kind("switch").unwrap();
        assert!(switch.contains(&"default".to_owned()), "{switch:?}");
        assert!(
            !switch.contains(&"no case matched".to_owned()),
            "a default already covers it"
        );

        // Without a default, the fall-through outcome is named explicitly.
        let open = obligations(
            "package main\nfunc f(a int) {\n\tswitch a {\n\tcase 1:\n\t\treturn\n\t}\n}\n",
        );
        let labels = open
            .manifest
            .branches
            .iter()
            .find(|b| b.kind == "switch")
            .unwrap()
            .alternatives
            .iter()
            .map(|a| a.label.clone())
            .collect::<Vec<_>>();
        assert!(labels.contains(&"no case matched".to_owned()), "{labels:?}");
    }

    #[test]
    fn functions_and_statements_are_separate_obligations_with_locations() {
        let go = obligations(SAMPLE);
        let functions = go
            .manifest
            .points
            .iter()
            .filter(|p| p.kind == PointKind::Function)
            .collect::<Vec<_>>();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].label.as_deref(), Some("classify"));
        assert_eq!(functions[0].line, 5);

        let statements = go
            .manifest
            .points
            .iter()
            .filter(|p| p.kind == PointKind::Statement)
            .count();
        assert!(
            statements >= 6,
            "expected the body's statements, got {statements}"
        );
        // Every obligation is anchored; an unanchorable line is absent rather
        // than counted as uncovered.
        assert!(
            go.manifest
                .points
                .iter()
                .all(|p| p.line > 0 && p.column > 0)
        );
        // Imports and type declarations carry no coverage question.
        assert!(
            !go.manifest
                .points
                .iter()
                .any(|p| p.source.starts_with("import"))
        );
    }

    #[test]
    fn every_obligation_has_a_probe_and_malformed_source_is_refused() {
        let go = obligations(SAMPLE);
        let expected = go.manifest.points.len()
            + go.manifest
                .branches
                .iter()
                .map(|b| b.alternatives.len())
                .sum::<usize>()
            + go.manifest
                .decisions
                .iter()
                .map(|d| d.conditions.len() + 1)
                .sum::<usize>();
        assert_eq!(go.probes.len(), expected);

        // Guessing at a file that does not parse would put obligations on lines
        // that may not exist.
        let mut next = 0;
        assert!(matches!(
            build_go_obligations("broken.go", "package main\nfunc f( {", &mut next),
            Err(GoInstrumenterError::Parse(_))
        ));
    }
}
