//! Supercov-owned Java and Kotlin parsing and obligation discovery.
//!
//! JaCoCo instruments bytecode; Supercov instruments source, for the same
//! reason it does everywhere else. Bytecode branches are the compiler's
//! branches, and a condition the compiler folded away is one no test can be
//! asked about. The denominator has to be the source the author wrote.
//!
//! Java and Kotlin share this module because they share a runtime and most of
//! a model. Where they differ they differ sharply, and each difference is
//! named at the point it matters rather than hidden behind a trait.

use std::collections::BTreeMap;

use tree_sitter::{Node, Parser};

use crate::coverage_analysis::PointKind;
use crate::coverage_report::{
    BranchAlternativeMeta, BranchMeta, CoverageManifest, DecisionMeta, PointMeta,
};
use crate::go_instrumenter::{GoEdit, GoProbe, GoProbeTarget};

/// The runtime class instrumented source calls.
pub const RUNTIME_CLASS: &str = "com.supercorp.supercov.Supercov";
/// The array probes store into, qualified so no import is needed.
pub const HITS: &str = "com.supercorp.supercov.Supercov.HITS";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum JvmLanguage {
    Java,
    Kotlin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JvmInstrumenterError {
    Parse(String),
}

impl std::fmt::Display for JvmInstrumenterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JvmInstrumenterError::Parse(detail) => write!(f, "JVM parse error: {detail}"),
        }
    }
}

pub fn parse(
    source: &str,
    language: JvmLanguage,
) -> Result<tree_sitter::Tree, JvmInstrumenterError> {
    let mut parser = Parser::new();
    let grammar = match language {
        JvmLanguage::Java => tree_sitter_java::LANGUAGE.into(),
        JvmLanguage::Kotlin => tree_sitter_kotlin_ng::LANGUAGE.into(),
    };
    parser
        .set_language(&grammar)
        .map_err(|error| JvmInstrumenterError::Parse(error.to_string()))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| JvmInstrumenterError::Parse("parser returned no tree".into()))?;
    if tree.root_node().has_error() {
        return Err(JvmInstrumenterError::Parse("source does not parse".into()));
    }
    Ok(tree)
}

#[derive(Debug, Clone, PartialEq)]
pub struct JvmFileObligations {
    pub manifest: CoverageManifest,
    pub probes: BTreeMap<u64, GoProbe>,
    pub edits: Vec<GoEdit>,
    pub decision_widths: Vec<u8>,
}

/// Statements Java nests directly inside a block. A declaration that cannot
/// execute carries no coverage question and is absent rather than uncovered.
fn is_java_statement(kind: &str) -> bool {
    matches!(
        kind,
        "assert_statement"
            | "break_statement"
            | "continue_statement"
            | "do_statement"
            | "enhanced_for_statement"
            | "expression_statement"
            | "for_statement"
            | "if_statement"
            | "labeled_statement"
            | "local_variable_declaration"
            | "return_statement"
            | "switch_expression"
            | "synchronized_statement"
            | "throw_statement"
            | "try_statement"
            | "try_with_resources_statement"
            | "while_statement"
            | "yield_statement"
    )
}

fn is_kotlin_statement(kind: &str) -> bool {
    matches!(
        kind,
        "assignment"
            | "call_expression"
            | "do_while_statement"
            | "for_statement"
            | "if_expression"
            | "jump_expression"
            | "property_declaration"
            | "when_expression"
            | "while_statement"
    )
}

struct Collector<'a> {
    file: &'a str,
    source: &'a str,
    language: JvmLanguage,
    next_probe: &'a mut u64,
    edits: Vec<GoEdit>,
    points: Vec<PointMeta>,
    branches: Vec<BranchMeta>,
    decisions: Vec<DecisionMeta>,
    probes: BTreeMap<u64, GoProbe>,
    limitations: Vec<serde_json::Value>,
    widths: Vec<u8>,
    counter: usize,
}

impl Collector<'_> {
    fn id(&mut self, prefix: &str) -> String {
        self.counter += 1;
        format!("{prefix}{}", self.counter)
    }

    fn probe(&mut self, target: GoProbeTarget, at: usize) -> u64 {
        *self.next_probe += 1;
        let id = *self.next_probe;
        self.probes.insert(id, GoProbe { id, target, at });
        id
    }

    fn edit(&mut self, at: usize, rank: i32, text: String) {
        self.edits.push(GoEdit { at, rank, text });
    }

    fn position(&self, node: Node) -> (usize, usize) {
        let start = node.start_position();
        (start.row + 1, start.column + 1)
    }

    fn text(&self, node: Node) -> String {
        self.source[node.byte_range()]
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .to_owned()
    }

    /// A probe in statement position. Java needs the semicolon; Kotlin's
    /// newline-terminated statements do not mind one either way, and writing
    /// it keeps a probe and the statement it precedes on one line so neither
    /// moves the other's reported position.
    fn store(&mut self, at: usize, probe: u64) {
        self.edit(at, 100, format!("{HITS}[{probe}] = 2; "));
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
        let at = match kind {
            PointKind::Function => match body_block(node, self.language) {
                Some(body) => body.start_byte() + 1,
                // An expression-bodied Kotlin function has no block to open.
                // Its expression is still measured; the function itself simply
                // has nowhere to record being entered.
                None => return,
            },
            PointKind::Statement => node.start_byte(),
        };
        let probe = self.probe(target, at);
        self.store(at, probe);
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

    fn add_branch(&mut self, node: Node, kind: &str, labels: &[&str]) -> Vec<u64> {
        let (line, column) = self.position(node);
        let id = self.id("b");
        let mut probes = Vec::new();
        let alternatives = labels
            .iter()
            .map(|label| {
                let alternative = format!("{id}.{label}");
                probes.push(self.probe(
                    GoProbeTarget::Alternative {
                        branch: id.clone(),
                        alternative: alternative.clone(),
                    },
                    node.start_byte(),
                ));
                BranchAlternativeMeta {
                    id: alternative,
                    label: (*label).to_owned(),
                }
            })
            .collect();
        self.branches.push(BranchMeta {
            id,
            kind: kind.to_owned(),
            file: self.file.to_owned(),
            line,
            column,
            source: self.text(node),
            alternatives,
        });
        probes
    }

    fn add_decision(&mut self, node: Node, kind: &str) -> Option<usize> {
        let mut leaves = Vec::new();
        condition_nodes(node, self.source, &mut leaves);
        if leaves.len() < 2 {
            return None;
        }
        let conditions = leaves
            .iter()
            .map(|leaf| self.source[leaf.byte_range()].trim().to_owned())
            .collect::<Vec<_>>();
        let (line, column) = self.position(node);
        let id = self.id("d");
        let index = self.widths.len();
        self.widths.push(leaves.len().min(64) as u8);
        for (position, leaf) in leaves.iter().enumerate() {
            // Java and Kotlin both evaluate an argument only when the call is
            // reached, so a wrapped right-hand operand runs exactly when the
            // unwrapped one would have.
            self.edit(
                leaf.start_byte(),
                20,
                format!("{RUNTIME_CLASS}.c({index}, {position}, "),
            );
            self.edit(leaf.end_byte(), 20, ")".to_owned());
        }
        self.decisions.push(DecisionMeta {
            id,
            file: self.file.to_owned(),
            line,
            column,
            source: self.text(node),
            conditions,
            kind: kind.to_owned(),
        });
        Some(index)
    }

    /// Give a branch arm somewhere to record itself.
    ///
    /// `if (x) return;` has no block, so a probe before the statement would
    /// leave it outside the `if` entirely: it would run unconditionally while
    /// the return stayed guarded, and the report would claim the arm was
    /// taken. Braces are the only honest fix.
    ///
    /// The arm also needs its own point. Walking never reaches it, because a
    /// statement is recognised by its parent being a block and this one's
    /// parent is the branch.
    fn ensure_block(&mut self, node: Node) {
        if node.kind() == "block" {
            return;
        }
        self.edit(node.start_byte(), 60, "{ ".to_owned());
        self.edit(node.end_byte(), 60, " }".to_owned());
        if is_statement(node.kind(), self.language) {
            // Ranked above the brace so that applying right to left leaves the
            // brace outermost and the probe within it.
            self.add_point(node, PointKind::Statement, None);
        }
    }
}

fn body_block<'t>(node: Node<'t>, language: JvmLanguage) -> Option<Node<'t>> {
    let body = node.child_by_field_name("body")?;
    match language {
        JvmLanguage::Java => (body.kind() == "block").then_some(body),
        JvmLanguage::Kotlin => {
            if body.kind() == "block" {
                return Some(body);
            }
            let mut cursor = body.walk();
            body.children(&mut cursor)
                .find(|child| child.kind() == "block")
        }
    }
}

/// Flatten a boolean expression into its independent conditions.
///
/// `&&` and `||` are the only short-circuiting operators either language has,
/// so they are the only ones that split a decision. `!` negates a condition
/// rather than introducing one, and parentheses are transparent. Java's
/// non-short-circuiting `&` and `|` deliberately do not split: both operands
/// always run, so neither can independently affect the outcome in the sense
/// MC/DC means.
fn condition_nodes<'t>(node: Node<'t>, source: &str, out: &mut Vec<Node<'t>>) {
    match node.kind() {
        "binary_expression" => {
            let operator = node
                .child_by_field_name("operator")
                .map(|op| &source[op.byte_range()])
                .unwrap_or("");
            if operator == "&&" || operator == "||" {
                if let Some(left) = node.child_by_field_name("left") {
                    condition_nodes(left, source, out);
                }
                if let Some(right) = node.child_by_field_name("right") {
                    condition_nodes(right, source, out);
                }
                return;
            }
            out.push(node);
        }
        "parenthesized_expression" => {
            let mut cursor = node.walk();
            match node.children(&mut cursor).find(|child| child.is_named()) {
                Some(inner) => condition_nodes(inner, source, out),
                None => out.push(node),
            }
        }
        _ => out.push(node),
    }
}

/// The expression inside `if (...)`. Java parenthesises its condition; Kotlin
/// does not.
fn condition_of<'t>(node: Node<'t>, language: JvmLanguage) -> Option<Node<'t>> {
    let condition = node.child_by_field_name("condition")?;
    if language == JvmLanguage::Java && condition.kind() == "parenthesized_expression" {
        let mut cursor = condition.walk();
        return condition
            .children(&mut cursor)
            .find(|child| child.is_named());
    }
    Some(condition)
}

pub fn build_jvm_obligations(
    file: &str,
    source: &str,
    language: JvmLanguage,
    next_probe: &mut u64,
) -> Result<JvmFileObligations, JvmInstrumenterError> {
    let tree = parse(source, language)?;
    let mut collector = Collector {
        file,
        source,
        language,
        next_probe,
        edits: Vec::new(),
        points: Vec::new(),
        branches: Vec::new(),
        decisions: Vec::new(),
        probes: BTreeMap::new(),
        limitations: Vec::new(),
        widths: Vec::new(),
        counter: 0,
    };
    walk(&mut collector, tree.root_node());
    Ok(JvmFileObligations {
        manifest: CoverageManifest {
            decisions: collector.decisions,
            points: collector.points,
            branches: collector.branches,
            limitations: collector.limitations,
            unmeasured: Vec::new(),
            scope: None,
        },
        probes: collector.probes,
        edits: collector.edits,
        decision_widths: collector.widths,
    })
}

fn walk(collector: &mut Collector, node: Node) {
    let language = collector.language;
    match node.kind() {
        "method_declaration" | "constructor_declaration" | "function_declaration" => {
            let label = node
                .child_by_field_name("name")
                .map(|name| collector.source[name.byte_range()].to_owned());
            collector.add_point(node, PointKind::Function, label);
        }
        "if_statement" | "if_expression" => {
            if let Some(condition) = condition_of(node, language) {
                let probes = collector.add_branch(node, "if", &["true", "false"]);
                let decision = collector.add_decision(condition, "if");
                let wrapper = match decision {
                    Some(index) => {
                        format!("{RUNTIME_CLASS}.bd({}, {}, {index}, ", probes[0], probes[1])
                    }
                    None => format!("{RUNTIME_CLASS}.b({}, {}, ", probes[0], probes[1]),
                };
                collector.edit(condition.start_byte(), 5, wrapper);
                collector.edit(condition.end_byte(), 5, ")".to_owned());
                // An arm written without braces has nowhere to record itself.
                for field in ["consequence", "alternative"] {
                    if let Some(arm) = node.child_by_field_name(field) {
                        collector.ensure_block(arm);
                    }
                }
            }
        }
        "while_statement" | "for_statement" | "do_statement" | "do_while_statement" => {
            match node.child_by_field_name("condition") {
                Some(condition) => {
                    let inner = if language == JvmLanguage::Java
                        && condition.kind() == "parenthesized_expression"
                    {
                        let mut cursor = condition.walk();
                        condition
                            .children(&mut cursor)
                            .find(|child| child.is_named())
                            .unwrap_or(condition)
                    } else {
                        condition
                    };
                    let probes = collector.add_branch(node, "loop", &["true", "false"]);
                    let decision = collector.add_decision(inner, "loop");
                    let wrapper = match decision {
                        Some(index) => {
                            format!("{RUNTIME_CLASS}.bd({}, {}, {index}, ", probes[0], probes[1])
                        }
                        None => format!("{RUNTIME_CLASS}.b({}, {}, ", probes[0], probes[1]),
                    };
                    collector.edit(inner.start_byte(), 5, wrapper);
                    collector.edit(inner.end_byte(), 5, ")".to_owned());
                }
                None => {
                    let (line, column) = collector.position(node);
                    collector.limitations.push(serde_json::json!({
                        "kind": "loop-without-condition",
                        "file": collector.file,
                        "line": line,
                        "column": column,
                        "detail": "a for-each or unconditional loop has no condition to observe, so no branch obligation is recorded for it",
                    }));
                }
            }
            if let Some(body) = node.child_by_field_name("body") {
                collector.ensure_block(body);
            }
        }
        "enhanced_for_statement" => {
            let (line, column) = collector.position(node);
            collector.limitations.push(serde_json::json!({
                "kind": "loop-without-condition",
                "file": collector.file,
                "line": line,
                "column": column,
                "detail": "a for-each loop has no condition to observe, so no branch obligation is recorded for it",
            }));
            if let Some(body) = node.child_by_field_name("body") {
                collector.ensure_block(body);
            }
        }
        kind if in_statement_position(node, language) && is_statement(kind, language) => {
            collector.add_point(node, PointKind::Statement, None);
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.is_named() {
            walk(collector, child);
        }
    }
}

fn is_statement(kind: &str, language: JvmLanguage) -> bool {
    match language {
        JvmLanguage::Java => is_java_statement(kind),
        JvmLanguage::Kotlin => is_kotlin_statement(kind),
    }
}

/// Whether a probe may be placed before this node.
///
/// Both languages have slots that hold a statement but are not statement
/// positions — a `for` loop's initialiser and update, a resource in
/// `try-with-resources`. A probe there does not compile. Everything that is
/// genuinely a statement is a child of a block or a switch group, so that is
/// the rule rather than a list of exceptions to remember.
fn in_statement_position(node: Node, _language: JvmLanguage) -> bool {
    node.parent().is_some_and(|parent| {
        matches!(
            parent.kind(),
            "block" | "statements" | "switch_block_statement_group" | "constructor_body"
        )
    })
}

/// Apply edits right to left so earlier offsets stay valid.
pub fn rewrite(source: &str, edits: &[GoEdit]) -> String {
    crate::go_instrumenter::rewrite(source, edits)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn java(source: &str) -> (JvmFileObligations, String) {
        let mut next = 0;
        let obligations =
            build_jvm_obligations("X.java", source, JvmLanguage::Java, &mut next).expect("java");
        let out = rewrite(source, &obligations.edits);
        parse(&out, JvmLanguage::Java)
            .unwrap_or_else(|error| panic!("rewritten Java does not parse: {error}\n{out}"));
        (obligations, out)
    }

    const SAMPLE: &str = r#"class Classify {
    String classify(int a, boolean b) {
        if (a > 10 && b) {
            return "big";
        }
        for (int i = 0; i < a; i++) {
            System.out.print(i);
        }
        return "small";
    }
}
"#;

    #[test]
    fn a_short_circuiting_operator_splits_a_decision_and_a_bitwise_one_does_not() {
        // `&` and `|` evaluate both operands always, so neither can
        // independently affect the outcome in the sense MC/DC means. Counting
        // them would put obligations in the denominator no test could satisfy.
        let (short_circuit, _) = java(SAMPLE);
        assert_eq!(
            short_circuit.manifest.decisions[0].conditions,
            ["a > 10", "b"]
        );

        let (bitwise, _) = java(
            "class X { boolean f(boolean a, boolean b) { if (a & b) { return true; } return false; } }",
        );
        assert!(
            bitwise.manifest.decisions.is_empty(),
            "{:?}",
            bitwise.manifest.decisions
        );
    }

    #[test]
    fn an_arm_written_without_braces_gets_them() {
        // `if (x) return;` has no block, so a probe before the statement would
        // sit outside the `if`: it would run unconditionally while the return
        // stayed guarded, and the report would claim the arm was taken.
        let (_, out) = java("class X { int f(int a) { if (a > 1) return 1; else return 2; } }");
        assert!(out.contains("{ "), "{out}");
        let guarded = out.find("return 1").expect("consequence");
        let opened = out[..guarded].rfind('{').expect("a brace before it");
        let probe = out[..guarded].rfind("HITS[").expect("a probe before it");
        assert!(
            opened < probe,
            "the probe must be inside the braces:\n{out}"
        );
    }

    #[test]
    fn a_probe_never_lands_where_java_forbids_a_statement() {
        // A `for` loop's initialiser and update hold statements but are not
        // statement positions, and try-with-resources holds declarations.
        let (_, out) = java(
            "import java.io.*;\nclass X { void f(int a) throws Exception { for (int i = 0; i < a; i++) { g(); } try (Reader r = open()) { g(); } } void g() {} Reader open() { return null; } }",
        );
        assert!(
            !out.contains("for (com.supercorp"),
            "probe in a for initialiser:\n{out}"
        );
        assert!(
            !out.contains("try (com.supercorp"),
            "probe in a resource:\n{out}"
        );
    }

    #[test]
    fn a_for_each_loop_records_a_limitation_not_an_obligation() {
        // There is no condition to observe. Declaring a branch nothing can
        // measure would put an obligation in the denominator no test could
        // ever satisfy.
        let (obligations, _) =
            java("class X { void f(int[] xs) { for (int x : xs) { g(x); } } void g(int x) {} }");
        assert!(
            obligations
                .manifest
                .branches
                .iter()
                .all(|b| b.kind != "loop")
        );
        assert_eq!(obligations.manifest.limitations.len(), 1);
        assert_eq!(
            obligations.manifest.limitations[0]["kind"],
            "loop-without-condition"
        );
    }

    #[test]
    fn kotlin_shares_the_model_and_differs_where_it_must() {
        // Kotlin's `if` is an expression and its condition is not
        // parenthesised, so the wrapper has to find a different child.
        let source = "fun f(a: Int, b: Boolean): String {\n    if (a > 10 && b) {\n        return \"big\"\n    }\n    return \"small\"\n}\n";
        let mut next = 0;
        let obligations =
            build_jvm_obligations("X.kt", source, JvmLanguage::Kotlin, &mut next).expect("kotlin");
        assert_eq!(
            obligations.manifest.decisions[0].conditions,
            ["a > 10", "b"]
        );
        let out = rewrite(source, &obligations.edits);
        parse(&out, JvmLanguage::Kotlin)
            .unwrap_or_else(|error| panic!("rewritten Kotlin does not parse: {error}\n{out}"));
        assert!(out.contains(".bd("), "{out}");
        assert!(
            out.contains(".c(0, 0, ") && out.contains(".c(0, 1, "),
            "{out}"
        );
    }
}
