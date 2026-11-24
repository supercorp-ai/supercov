//! Each mutant of a JavaScript or TypeScript file, linked to the evidence
//! that decides how far the run's tests got toward killing it.
//!
//! `js_mutants::enumerate` lists StrykerJS's mutants on the program as
//! written. Instrumentation then measures the same program with points,
//! decisions, branches and probes, each found by its exact span. A mutant is
//! linked to one of them only when their spans say it is the same code: the
//! statement and function that contain it, the decision it is or is part of,
//! the logical selection it is or is an operand of, the optional link it
//! removes, and the comparison or infection probe placed on it. Nothing here
//! matches text. A link is absent when no such evidence exists -- a probe
//! skipped in a source-sensitive function leaves no `probe` link, so a
//! missing record there never reads as "not evaluated".
//!
//! `js_weak_mutation` turns these links and a run's observations into one
//! status per mutant.

use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};
use std::collections::BTreeMap;

use oxc_ast::ast::{
    ArrowFunctionExpression, AssignmentExpression, BinaryExpression, CallExpression,
    ComputedMemberExpression, ConditionalExpression, DoWhileStatement, Expression, ForStatement,
    IfStatement, LogicalExpression, PrivateFieldExpression, Program, StaticMemberExpression,
    UnaryExpression, WhileStatement,
};
use oxc_ast_visit::{Visit, walk};
use oxc_span::{GetSpan, Span};
use oxc_syntax::operator::{BinaryOperator, LogicalOperator, UnaryOperator};

use crate::js_instrumenter::{
    CandidateBranch, CandidateDecision, CandidatePoint, expression_has_effects,
    infection_probe_site,
};
use crate::js_mutants::LedgerMutant;
use crate::mutant_list::ListedMutants;
pub use crate::mutant_list::{
    ArrowLink, BranchLink, BranchRole, CandidateMutant, ChainLink, DecisionLink, MutantOperator,
    MutantsFile, StaticNote,
};

/// The mutants of the files in `recorded` (each file's point ids, as the
/// run's manifest has them), read from under `root`.
pub(crate) fn list_mutants(
    root: &std::path::Path,
    recorded: &BTreeMap<String, std::collections::BTreeSet<String>>,
) -> ListedMutants {
    // The run's other languages' files have no JavaScript mutants.
    let files = recorded
        .keys()
        .filter(|file| {
            crate::mutant_list::language_of_path(file) == Some(crate::mutant_list::JAVASCRIPT)
        })
        .cloned()
        .collect::<Vec<_>>();
    let listed = crate::threads::parallel_map(&files, |file| {
        let source = std::fs::read_to_string(root.join(file)).ok()?;
        let elide = crate::typescript_imports::elides_type_imports(root, file);
        let output = crate::js_instrumenter::listing_mutants(|| {
            crate::js_instrumenter::instrument_with_import_policy(
                &source,
                file,
                "capability.mjs",
                elide,
            )
        })
        .ok()?;
        let points = output
            .points
            .iter()
            .map(|point| point.id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        (Some(&points) == recorded.get(file)).then_some(output.mutants)
    });
    let mut mutants = BTreeMap::new();
    let mut changed = Vec::new();
    for (file, listed) in files.into_iter().zip(listed) {
        match listed {
            Some(list) if list.is_empty() => {}
            Some(list) => {
                mutants.insert(file, list);
            }
            None => changed.push(file),
        }
    }
    ListedMutants {
        mutants: MutantsFile::listed(
            crate::mutant_list::JAVASCRIPT,
            crate::js_mutants::MUTANT_SET,
            mutants,
        ),
        changed,
    }
}

type SpanKey = (u32, u32);

fn key(span: Span) -> SpanKey {
    (span.start, span.end)
}

/// How an expression's value is used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mark {
    /// Only its truthiness: a test, a `!`'s operand.
    Truthiness,
    /// The left operand of `&&` or `||`: its value is the result only when
    /// it short-circuits.
    LeftOf(LogicalOperator),
    /// The right operand of this logical expression: its value is the
    /// logical expression's.
    Inside(SpanKey),
}

/// A decision's conditions and, for each `&&`/`||` node and condition in it,
/// the conditions it covers and whether it is made of `&&` and `||` alone.
#[derive(Debug, Default)]
struct DecisionShape {
    conditions: usize,
    nodes: HashMap<SpanKey, (usize, usize, bool)>,
}

impl DecisionShape {
    /// As `collect_conditions` numbers the conditions.
    fn build(&mut self, expression: &Expression<'_>) -> (usize, usize, bool) {
        match expression {
            Expression::ParenthesizedExpression(inner) => self.build(&inner.expression),
            Expression::LogicalExpression(logical)
                if matches!(logical.operator, LogicalOperator::And | LogicalOperator::Or) =>
            {
                let (first, _, left) = self.build(&logical.left);
                let (_, end, right) = self.build(&logical.right);
                let node = (first, end, left && right);
                self.nodes.insert(key(logical.span), node);
                node
            }
            Expression::UnaryExpression(unary)
                if unary.operator == UnaryOperator::LogicalNot && compound(&unary.argument) =>
            {
                let (first, end, _) = self.build(&unary.argument);
                (first, end, false)
            }
            leaf => {
                let index = self.conditions;
                self.conditions += 1;
                let node = (index, index + 1, true);
                self.nodes.insert(key(leaf.span()), node);
                node
            }
        }
    }
}

/// An `&&` or `||` under any parentheses and `!`s.
fn compound(expression: &Expression<'_>) -> bool {
    match expression {
        Expression::ParenthesizedExpression(inner) => compound(&inner.expression),
        Expression::UnaryExpression(unary) if unary.operator == UnaryOperator::LogicalNot => {
            compound(&unary.argument)
        }
        Expression::LogicalExpression(logical) => {
            matches!(logical.operator, LogicalOperator::And | LogicalOperator::Or)
        }
        _ => false,
    }
}

/// An expression whose value is always a boolean, so its truthiness is its
/// value.
fn boolean_valued(expression: &Expression<'_>) -> bool {
    match expression.without_parentheses() {
        Expression::BinaryExpression(binary) => comparison(binary.operator),
        Expression::UnaryExpression(unary) => unary.operator == UnaryOperator::LogicalNot,
        Expression::BooleanLiteral(_) => true,
        Expression::LogicalExpression(logical) => {
            boolean_valued(&logical.left) && boolean_valued(&logical.right)
        }
        Expression::ConditionalExpression(conditional) => {
            boolean_valued(&conditional.consequent) && boolean_valued(&conditional.alternate)
        }
        _ => false,
    }
}

fn comparison(operator: BinaryOperator) -> bool {
    matches!(
        operator,
        BinaryOperator::LessThan
            | BinaryOperator::LessEqualThan
            | BinaryOperator::GreaterThan
            | BinaryOperator::GreaterEqualThan
            | BinaryOperator::Equality
            | BinaryOperator::Inequality
            | BinaryOperator::StrictEquality
            | BinaryOperator::StrictInequality
            | BinaryOperator::In
            | BinaryOperator::Instanceof
    )
}

/// A concise arrow body that is never `undefined`, whatever it evaluates to.
fn never_undefined(expression: &Expression<'_>) -> bool {
    match expression {
        Expression::ParenthesizedExpression(inner) => never_undefined(&inner.expression),
        Expression::TSAsExpression(inner) => never_undefined(&inner.expression),
        Expression::TSSatisfiesExpression(inner) => never_undefined(&inner.expression),
        Expression::TSNonNullExpression(inner) => never_undefined(&inner.expression),
        Expression::TSTypeAssertion(inner) => never_undefined(&inner.expression),
        Expression::StringLiteral(_)
        | Expression::NumericLiteral(_)
        | Expression::BigIntLiteral(_)
        | Expression::BooleanLiteral(_)
        | Expression::NullLiteral(_)
        | Expression::RegExpLiteral(_)
        | Expression::TemplateLiteral(_)
        | Expression::ArrayExpression(_)
        | Expression::ObjectExpression(_)
        | Expression::FunctionExpression(_)
        | Expression::ArrowFunctionExpression(_)
        | Expression::ClassExpression(_)
        | Expression::NewExpression(_)
        | Expression::UpdateExpression(_)
        | Expression::BinaryExpression(_)
        | Expression::JSXElement(_)
        | Expression::JSXFragment(_) => true,
        Expression::UnaryExpression(unary) => unary.operator != UnaryOperator::Void,
        // `a || b` gives `a` only when it is truthy; `a ?? b` only when it
        // is neither null nor undefined.
        Expression::LogicalExpression(logical) => match logical.operator {
            LogicalOperator::And => {
                never_undefined(&logical.left) && never_undefined(&logical.right)
            }
            LogicalOperator::Or | LogicalOperator::Coalesce => never_undefined(&logical.right),
        },
        Expression::ConditionalExpression(conditional) => {
            never_undefined(&conditional.consequent) && never_undefined(&conditional.alternate)
        }
        Expression::SequenceExpression(sequence) => {
            sequence.expressions.last().is_some_and(never_undefined)
        }
        Expression::AssignmentExpression(assignment) => {
            assignment.operator == oxc_syntax::operator::AssignmentOperator::Assign
                && never_undefined(&assignment.right)
        }
        _ => false,
    }
}

/// What linking needs to know of the program as written, gathered before
/// any pass rewrites it.
#[derive(Debug, Default)]
pub(crate) struct MutantFacts {
    /// Binary, logical and assignment expressions: the operator and its byte
    /// offset in the source.
    operators: HashMap<SpanKey, (&'static str, u32)>,
    /// Logical expressions and logical assignments: the operands' spans and
    /// the operator.
    operands: HashMap<SpanKey, (Span, Span, &'static str)>,
    marks: HashMap<SpanKey, Mark>,
    /// Comparisons and the logical expressions made of booleans.
    boolean: HashSet<SpanKey>,
    /// Each decision's shape, by its span.
    decisions: HashMap<SpanKey, DecisionShape>,
    /// Each optional link and the optional links of its chain before it.
    chains: HashMap<SpanKey, Vec<Span>>,
    arrows: HashMap<SpanKey, ArrowLink>,
    /// Expressions whose evaluation runs code or changes state (as
    /// `expression_has_effects` counts it), outside functions they define.
    effects: HashSet<SpanKey>,
    /// Per expression being visited, whether one inside it has effects.
    effect_stack: Vec<bool>,
    /// Equivalent mutants and untestable boundaries, from the program alone.
    statics: crate::js_static_mutants::StaticFacts,
}

struct FactCollector<'s> {
    source: &'s str,
    decision_spans: HashSet<SpanKey>,
    facts: MutantFacts,
}

impl MutantFacts {
    pub(crate) fn collect(
        program: &Program<'_>,
        source: &str,
        decisions: &[CandidateDecision],
    ) -> Self {
        let lines = Lines::new(source);
        let decision_spans = decisions
            .iter()
            .filter_map(|decision| lines.span(decision.line, decision.column, &decision.source))
            .map(key)
            .collect();
        let mut collector = FactCollector {
            source,
            decision_spans,
            facts: MutantFacts::default(),
        };
        collector.visit_program(program);
        let mut facts = collector.facts;
        facts.statics = crate::js_static_mutants::StaticFacts::collect(program);
        facts
    }

    /// How the expression at `span` is used, through right operands.
    fn mark(&self, span: SpanKey) -> Option<Mark> {
        let mut current = span;
        // Each step is to an enclosing expression: it ends.
        loop {
            match self.marks.get(&current) {
                Some(Mark::Inside(parent)) => current = *parent,
                other => return other.copied(),
            }
        }
    }

    /// Whether the mutant's own value, not only its truthiness, can reach
    /// the program where the mutant's truthiness equals the original's.
    fn value_used(&self, mutant: &LedgerMutant) -> bool {
        let span = key(mutant.span);
        if self.boolean.contains(&span) {
            return false;
        }
        match mutant.mutator {
            "ConditionalExpression" => {
                let forced = match mutant.replacement.as_str() {
                    "true" => true,
                    "false" => false,
                    _ => return false,
                };
                match self.mark(span) {
                    Some(Mark::Truthiness) => false,
                    // A left operand's value is the result only where it
                    // short-circuits, which the forced literal may make it.
                    Some(Mark::LeftOf(LogicalOperator::And)) => !forced,
                    Some(Mark::LeftOf(LogicalOperator::Or)) => forced,
                    _ => true,
                }
            }
            "LogicalOperator" => self.mark(span) != Some(Mark::Truthiness),
            _ => false,
        }
    }
}

impl FactCollector<'_> {
    fn mark(&mut self, expression: &Expression<'_>, mark: Mark) {
        self.facts
            .marks
            .insert(key(expression.without_parentheses().span()), mark);
    }

    /// The operator between two operands: the first text after the left one
    /// that is not a comment, a space or a closing parenthesis (an opening
    /// one cannot come before the operator).
    fn operator(&mut self, span: Span, left: Span, right: Span, text: &'static str) {
        let gap = &self.source[left.end as usize..right.start as usize];
        let mut offset = 0;
        let found = loop {
            let rest = &gap[offset..];
            if rest.starts_with(text) {
                break Some(offset);
            }
            if rest.starts_with("//") {
                // Any line terminator ends it; the terminator is a space.
                offset += rest
                    .find(['\n', '\r', '\u{2028}', '\u{2029}'])
                    .unwrap_or(rest.len());
            } else if rest.starts_with("/*") {
                offset += rest.find("*/").map_or(rest.len(), |end| end + 2);
            } else if rest.starts_with(|character: char| {
                // A byte order mark is JavaScript whitespace, not Unicode's.
                character.is_whitespace() || character == '\u{feff}' || character == ')'
            }) {
                offset += rest.chars().next().map_or(0, char::len_utf8);
            } else {
                // A script's HTML-like comment (`<!--`), which the scan does
                // not read: the operator is not shown.
                break None;
            }
        };
        if let Some(offset) = found {
            self.facts
                .operators
                .insert(key(span), (text, left.end + offset as u32));
        }
    }

    fn chain(&mut self, span: Span, mut below: &Expression<'_>) {
        let mut earlier = Vec::new();
        loop {
            below = match below {
                Expression::StaticMemberExpression(member) => {
                    if member.optional {
                        earlier.push(member.span);
                    }
                    &member.object
                }
                Expression::ComputedMemberExpression(member) => {
                    if member.optional {
                        earlier.push(member.span);
                    }
                    &member.object
                }
                Expression::PrivateFieldExpression(member) => {
                    if member.optional {
                        earlier.push(member.span);
                    }
                    &member.object
                }
                Expression::CallExpression(call) => {
                    if call.optional {
                        earlier.push(call.span);
                    }
                    &call.callee
                }
                Expression::TSNonNullExpression(inner) => &inner.expression,
                _ => break,
            };
        }
        earlier.reverse();
        self.facts.chains.insert(key(span), earlier);
    }
}

impl<'a> Visit<'a> for FactCollector<'_> {
    fn visit_expression(&mut self, expression: &Expression<'a>) {
        if !matches!(expression, Expression::ParenthesizedExpression(_)) {
            let span = key(expression.span());
            if self.decision_spans.contains(&span) {
                self.facts.decisions.entry(span).or_insert_with(|| {
                    let mut shape = DecisionShape::default();
                    shape.build(expression);
                    shape
                });
            }
        }
        self.facts.effect_stack.push(false);
        walk::walk_expression(self, expression);
        let inside = self.facts.effect_stack.pop().unwrap_or(false);
        let own = inside
            || match expression {
                Expression::CallExpression(_)
                | Expression::NewExpression(_)
                | Expression::AssignmentExpression(_)
                | Expression::UpdateExpression(_)
                | Expression::AwaitExpression(_)
                | Expression::YieldExpression(_)
                | Expression::TaggedTemplateExpression(_)
                | Expression::ImportExpression(_)
                | Expression::ClassExpression(_) => true,
                Expression::UnaryExpression(unary) => unary.operator == UnaryOperator::Delete,
                _ => false,
            };
        if own {
            self.facts.effects.insert(key(expression.span()));
            if let Some(outer) = self.facts.effect_stack.last_mut() {
                *outer = true;
            }
        }
    }

    // What a function defines runs when it is called, not when the
    // expression that defines it is evaluated.
    fn visit_function(
        &mut self,
        function: &oxc_ast::ast::Function<'a>,
        flags: oxc_syntax::scope::ScopeFlags,
    ) {
        let outer = std::mem::take(&mut self.facts.effect_stack);
        walk::walk_function(self, function, flags);
        self.facts.effect_stack = outer;
    }

    fn visit_if_statement(&mut self, statement: &IfStatement<'a>) {
        self.mark(&statement.test, Mark::Truthiness);
        walk::walk_if_statement(self, statement);
    }

    fn visit_while_statement(&mut self, statement: &WhileStatement<'a>) {
        self.mark(&statement.test, Mark::Truthiness);
        walk::walk_while_statement(self, statement);
    }

    fn visit_do_while_statement(&mut self, statement: &DoWhileStatement<'a>) {
        self.mark(&statement.test, Mark::Truthiness);
        walk::walk_do_while_statement(self, statement);
    }

    fn visit_for_statement(&mut self, statement: &ForStatement<'a>) {
        if let Some(test) = &statement.test {
            self.mark(test, Mark::Truthiness);
        }
        walk::walk_for_statement(self, statement);
    }

    fn visit_conditional_expression(&mut self, expression: &ConditionalExpression<'a>) {
        self.mark(&expression.test, Mark::Truthiness);
        walk::walk_conditional_expression(self, expression);
    }

    fn visit_unary_expression(&mut self, expression: &UnaryExpression<'a>) {
        if expression.operator == UnaryOperator::LogicalNot {
            self.mark(&expression.argument, Mark::Truthiness);
        }
        walk::walk_unary_expression(self, expression);
    }

    fn visit_binary_expression(&mut self, expression: &BinaryExpression<'a>) {
        self.operator(
            expression.span,
            expression.left.span(),
            expression.right.span(),
            expression.operator.as_str(),
        );
        if comparison(expression.operator) {
            self.facts.boolean.insert(key(expression.span));
        }
        walk::walk_binary_expression(self, expression);
    }

    fn visit_logical_expression(&mut self, expression: &LogicalExpression<'a>) {
        let operator = expression.operator.as_str();
        let (left, right) = (
            expression.left.without_parentheses().span(),
            expression.right.without_parentheses().span(),
        );
        self.operator(
            expression.span,
            expression.left.span(),
            expression.right.span(),
            operator,
        );
        self.facts
            .operands
            .insert(key(expression.span), (left, right, operator));
        if expression.operator != LogicalOperator::Coalesce {
            self.mark(&expression.left, Mark::LeftOf(expression.operator));
        }
        self.mark(&expression.right, Mark::Inside(key(expression.span)));
        if boolean_valued(&expression.left) && boolean_valued(&expression.right) {
            self.facts.boolean.insert(key(expression.span));
        }
        walk::walk_logical_expression(self, expression);
    }

    fn visit_assignment_expression(&mut self, expression: &AssignmentExpression<'a>) {
        let operator = expression.operator.as_str();
        let target = expression.left.span();
        self.operator(expression.span, target, expression.right.span(), operator);
        if expression.operator.is_logical() {
            self.facts.operands.insert(
                key(expression.span),
                (
                    target,
                    expression.right.without_parentheses().span(),
                    operator,
                ),
            );
        }
        walk::walk_assignment_expression(self, expression);
    }

    fn visit_static_member_expression(&mut self, expression: &StaticMemberExpression<'a>) {
        if expression.optional {
            self.chain(expression.span, &expression.object);
        }
        walk::walk_static_member_expression(self, expression);
    }

    fn visit_computed_member_expression(&mut self, expression: &ComputedMemberExpression<'a>) {
        if expression.optional {
            self.chain(expression.span, &expression.object);
        }
        walk::walk_computed_member_expression(self, expression);
    }

    fn visit_private_field_expression(&mut self, expression: &PrivateFieldExpression<'a>) {
        if expression.optional {
            self.chain(expression.span, &expression.object);
        }
        walk::walk_private_field_expression(self, expression);
    }

    fn visit_call_expression(&mut self, expression: &CallExpression<'a>) {
        if expression.optional {
            self.chain(expression.span, &expression.callee);
        }
        walk::walk_call_expression(self, expression);
    }

    fn visit_arrow_function_expression(&mut self, arrow: &ArrowFunctionExpression<'a>) {
        let outer = std::mem::take(&mut self.facts.effect_stack);
        self.visit_arrow_body(arrow);
        self.facts.effect_stack = outer;
    }
}

impl<'a> FactCollector<'_> {
    fn visit_arrow_body(&mut self, arrow: &ArrowFunctionExpression<'a>) {
        if let Some(body) = arrow.get_expression() {
            self.facts.arrows.insert(
                key(arrow.span),
                ArrowLink {
                    effects: expression_has_effects(body),
                    never_undefined: never_undefined(body),
                },
            );
        }
        walk::walk_arrow_function_expression(self, arrow);
    }
}

/// Line starts as the instrumenter counts lines (`\n` only), to turn a
/// point's line, UTF-16 column and source text back into its exact span.
struct Lines<'s> {
    source: &'s str,
    starts: Vec<usize>,
}

impl<'s> Lines<'s> {
    fn new(source: &'s str) -> Self {
        let starts = std::iter::once(0)
            .chain(source.match_indices('\n').map(|(index, _)| index + 1))
            .collect();
        Self { source, starts }
    }

    fn span(&self, line: usize, column: usize, text: &str) -> Option<Span> {
        let mut offset = *self.starts.get(line.checked_sub(1)?)?;
        let mut units = 0;
        for character in self.source[offset..].chars() {
            if units >= column.checked_sub(1)? {
                break;
            }
            units += character.len_utf16();
            offset += character.len_utf8();
        }
        let end = offset + text.len();
        (units + 1 == column && self.source.get(offset..end) == Some(text))
            .then(|| Span::new(offset as u32, end as u32))
    }
}

/// Points whose spans nest, to find the innermost one containing a span.
struct Nested<'p> {
    items: Vec<(Span, &'p str)>,
    parents: Vec<Option<usize>>,
}

impl<'p> Nested<'p> {
    fn new(mut items: Vec<(Span, &'p str)>) -> Self {
        items.sort_by(|left, right| {
            left.0
                .start
                .cmp(&right.0.start)
                .then(right.0.end.cmp(&left.0.end))
        });
        let mut parents = Vec::with_capacity(items.len());
        let mut open: Vec<usize> = Vec::new();
        for (span, _) in &items {
            while let Some(&top) = open.last() {
                if items[top].0.end >= span.end {
                    break;
                }
                open.pop();
            }
            parents.push(open.last().copied());
            open.push(parents.len() - 1);
        }
        Self { items, parents }
    }

    /// The last point starting at or before `span`, then its enclosing ones:
    /// every point containing `span` is among them, the innermost first.
    fn innermost(&self, span: Span) -> Option<&'p str> {
        let last = self
            .items
            .partition_point(|(item, _)| item.start <= span.start);
        let mut index = last.checked_sub(1);
        while let Some(at) = index {
            // Each of these starts at or before `span`, so it contains
            // `span` when it ends at or after it.
            let (item, id) = self.items[at];
            if item.end >= span.end {
                return Some(id);
            }
            index = self.parents[at];
        }
        None
    }
}

/// What instrumentation measured and placed in the file.
pub(crate) struct Evidence<'e> {
    pub points: &'e [CandidatePoint],
    pub decisions: &'e [CandidateDecision],
    pub branches: &'e [CandidateBranch],
    pub comparisons: &'e [(Span, String)],
    pub probes: &'e [String],
    /// The read points of module-level mutants, by the mutated span
    /// (`js_value_reads`).
    pub reads: &'e HashMap<SpanKey, crate::mutant_list::ReadLink>,
}

/// The mutants of `file`, each with its links.
pub(crate) fn link(
    file: &str,
    source: &str,
    ledger: Vec<LedgerMutant>,
    facts: &MutantFacts,
    evidence: &Evidence<'_>,
) -> Vec<CandidateMutant> {
    let lines = Lines::new(source);
    let spanned = |kind: &str| {
        evidence
            .points
            .iter()
            .filter(|point| point.kind == kind)
            .filter_map(|point| {
                lines
                    .span(point.line, point.column, &point.source)
                    .map(|span| (span, point.id.as_str()))
            })
            .collect::<Vec<_>>()
    };
    let statements = Nested::new(spanned("statement"));
    let functions = Nested::new(spanned("function"));

    let mut whole_decisions = HashMap::<SpanKey, &str>::default();
    let mut decision_nodes = HashMap::<SpanKey, (&str, usize, usize, bool)>::default();
    for decision in evidence.decisions {
        let Some(span) = lines.span(decision.line, decision.column, &decision.source) else {
            continue;
        };
        let Some(shape) = facts.decisions.get(&key(span)) else {
            continue;
        };
        whole_decisions.entry(key(span)).or_insert(&decision.id);
        for (node, (first, end, pure)) in &shape.nodes {
            decision_nodes
                .entry(*node)
                .or_insert((&decision.id, *first, *end, *pure));
        }
    }

    let mut selections = HashMap::<SpanKey, (&str, BranchRole, &str)>::default();
    let mut operands = HashMap::<SpanKey, (&str, BranchRole, &str)>::default();
    let mut chains = HashMap::<SpanKey, &str>::default();
    for branch in evidence.branches {
        let Some(span) = lines.span(branch.line, branch.column, &branch.source) else {
            continue;
        };
        match branch.kind.as_str() {
            "logical-value" | "logical-assignment" => {
                let Some((left, right, operator)) = facts.operands.get(&key(span)) else {
                    continue;
                };
                selections
                    .entry(key(span))
                    .or_insert((&branch.id, BranchRole::Value, operator));
                operands
                    .entry(key(*left))
                    .or_insert((&branch.id, BranchRole::Left, operator));
                operands
                    .entry(key(*right))
                    .or_insert((&branch.id, BranchRole::Right, operator));
            }
            "optional-chain" => {
                chains.entry(key(span)).or_insert(&branch.id);
            }
            _ => {}
        }
    }

    // A site two different comparisons share decides neither.
    let mut compared = HashMap::<&str, HashSet<SpanKey>>::default();
    for (span, site) in evidence.comparisons {
        compared.entry(site).or_default().insert(key(*span));
    }
    let compare_sites = compared
        .into_iter()
        .filter_map(|(site, spans)| {
            (spans.len() == 1).then(|| (*spans.iter().next().expect("one span"), site))
        })
        .collect::<HashMap<_, _>>();
    let probes = evidence
        .probes
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();

    ledger
        .into_iter()
        .map(|mutant| {
            let span = mutant.span;
            let at = key(span);
            let decision = whole_decisions
                .get(&at)
                .map(|id| DecisionLink {
                    id: (*id).to_owned(),
                    condition: None,
                    prefix: None,
                })
                .or_else(|| {
                    let (id, first, end, pure) = *decision_nodes.get(&at)?;
                    let (condition, prefix) = if end == first + 1 {
                        (Some(first), None)
                    } else if first == 0 && pure {
                        (None, Some(end))
                    } else {
                        return None;
                    };
                    Some(DecisionLink {
                        id: id.to_owned(),
                        condition,
                        prefix,
                    })
                });
            let branch =
                selections
                    .get(&at)
                    .or_else(|| operands.get(&at))
                    .map(|(id, role, operator)| BranchLink {
                        id: (*id).to_owned(),
                        role: *role,
                        operator: (*operator).to_owned(),
                    });
            let chain = chains.get(&at).map(|id| ChainLink {
                id: (*id).to_owned(),
                earlier: facts
                    .chains
                    .get(&at)
                    .into_iter()
                    .flatten()
                    .filter_map(|link| chains.get(&key(*link)).map(|id| (*id).to_owned()))
                    .collect(),
            });
            let probe_kind = match mutant.mutator {
                "ArithmeticOperator" => Some("arith"),
                "AssignmentOperator" => Some("assign"),
                "MethodExpression" => Some("method"),
                "ArrowFunction" => Some("arrow"),
                _ => None,
            };
            let probe = probe_kind
                .map(|kind| infection_probe_site(file, source, span, kind))
                .filter(|site| probes.contains(site.as_str()));
            let compare = match mutant.mutator {
                "EqualityOperator" | "ConditionalExpression" => {
                    compare_sites.get(&at).map(|site| (*site).to_owned())
                }
                _ => None,
            };
            let arrow = (mutant.mutator == "ArrowFunction")
                .then(|| facts.arrows.get(&at).copied())
                .flatten();
            let operator = facts
                .operators
                .get(&at)
                .map(|(text, offset)| MutantOperator {
                    text: (*text).to_owned(),
                    at: (offset - span.start) as usize,
                });
            let equivalent = facts.statics.equivalent(&mutant);
            let untestable = facts.statics.untestable(&mutant);
            CandidateMutant {
                id: format!(
                    "{file}:{}:{}-{}:{}:{}:{}",
                    mutant.start.0,
                    mutant.start.1,
                    mutant.end.0,
                    mutant.end.1,
                    mutant.mutator,
                    mutant.replacement
                ),
                mutator: mutant.mutator.to_owned(),
                value_used: facts.value_used(&mutant),
                effects: mutant.mutator == "ConditionalExpression"
                    && facts.effects.contains(&key(mutant.span)),
                replacement: mutant.replacement,
                start: mutant.start,
                end: mutant.end,
                original: source[span.start as usize..span.end as usize].to_owned(),
                operator,
                statement: statements.innermost(span).map(str::to_owned),
                function: functions.innermost(span).map(str::to_owned),
                decision,
                branch,
                chain,
                compare,
                probe,
                arrow,
                equivalent,
                untestable,
                reads: evidence.reads.get(&at).cloned(),
                is_static: false,
                unplaceable: None,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::js_instrumenter::{
        CandidateOutput, MUTANTS_FOR_TEST, PROBES_FOR_TEST, instrument_candidate,
    };

    /// `source` instrumented with the mutants on, with or without their
    /// probes.
    fn instrumented(source: &str, probes: bool) -> CandidateOutput {
        MUTANTS_FOR_TEST.with(|flag| flag.set(Some(true)));
        PROBES_FOR_TEST.with(|flag| flag.set(probes));
        let output = instrument_candidate(source, "src/links.ts");
        MUTANTS_FOR_TEST.with(|flag| flag.set(None));
        PROBES_FOR_TEST.with(|flag| flag.set(true));
        output.unwrap()
    }

    fn mutant<'o>(
        output: &'o CandidateOutput,
        original: &str,
        mutator: &str,
        replacement: &str,
    ) -> &'o CandidateMutant {
        let found = output
            .mutants
            .iter()
            .filter(|mutant| {
                mutant.original == original
                    && mutant.mutator == mutator
                    && mutant.replacement == replacement
            })
            .collect::<Vec<_>>();
        assert_eq!(
            found.len(),
            1,
            "{mutator} {replacement} on `{original}`: {:#?}",
            output.mutants
        );
        found[0]
    }

    fn point<'o>(output: &'o CandidateOutput, kind: &str, source: &str) -> &'o str {
        &output
            .points
            .iter()
            .find(|point| point.kind == kind && point.source == source)
            .unwrap_or_else(|| panic!("no {kind} `{source}`: {:#?}", output.points))
            .id
    }

    fn decision<'o>(output: &'o CandidateOutput, source: &str) -> &'o str {
        &output
            .decisions
            .iter()
            .find(|decision| decision.source == source)
            .unwrap_or_else(|| panic!("no decision `{source}`: {:#?}", output.decisions))
            .id
    }

    fn branch<'o>(output: &'o CandidateOutput, kind: &str, source: &str) -> &'o str {
        &output
            .branches
            .iter()
            .find(|branch| branch.kind == kind && branch.source == source)
            .unwrap_or_else(|| panic!("no {kind} `{source}`: {:#?}", output.branches))
            .id
    }

    #[test]
    fn nothing_is_enumerated_with_the_gate_off_and_the_code_is_unchanged_with_it_on() {
        let source =
            "export function f(a: number) {\n  if (a > 0) return \"x\";\n  return a + 1;\n}\n";
        MUTANTS_FOR_TEST.with(|flag| flag.set(Some(false)));
        let off = instrument_candidate(source, "src/links.ts");
        MUTANTS_FOR_TEST.with(|flag| flag.set(None));
        let off = off.unwrap();
        assert!(off.mutants.is_empty());
        let serialized = serde_json::to_value(&off).unwrap();
        assert!(serialized.get("mutants").is_none(), "{serialized}");
        let on = instrumented(source, false);
        assert!(!on.mutants.is_empty());
        assert_eq!(on.code, off.code);
        // Each mutant carries StrykerJS's id, span and original text.
        let literal = mutant(&on, "\"x\"", "StringLiteral", "empty");
        assert_eq!(literal.id, "src/links.ts:2:21-2:24:StringLiteral:empty");
        assert_eq!((literal.start, literal.end), ((2, 21), (2, 24)));
        // Without probes there is nothing placed to link to.
        assert!(
            on.mutants
                .iter()
                .all(|m| m.compare.is_none() && m.probe.is_none())
        );
    }

    #[test]
    fn a_mutant_is_linked_to_its_innermost_statement_and_function() {
        let source = concat!(
            "export function f(a: number, b: boolean) {\n",
            "  if (a > 0 && b) {\n",
            "    return \"x\";\n",
            "  }\n",
            "  const g = (x: number) => x + 1;\n",
            "  return g;\n",
            "}\n",
        );
        let output = instrumented(source, false);
        let function = point(&output, "function", &source[7..source.len() - 1]);
        let literal = mutant(&output, "\"x\"", "StringLiteral", "empty");
        assert_eq!(
            literal.statement.as_deref(),
            Some(point(&output, "statement", "return \"x\";"))
        );
        assert_eq!(literal.function.as_deref(), Some(function));
        // A concise arrow's body has no statement of its own: the one around
        // the arrow, and the arrow's own function point.
        let sum = mutant(&output, "x + 1", "ArithmeticOperator", "-");
        assert_eq!(
            sum.statement.as_deref(),
            Some(point(
                &output,
                "statement",
                "const g = (x: number) => x + 1;"
            ))
        );
        assert_eq!(
            sum.function.as_deref(),
            Some(point(&output, "function", "(x: number) => x + 1"))
        );
        assert_eq!(
            sum.operator,
            Some(MutantOperator {
                text: "+".into(),
                at: 2
            })
        );
        // A function body: no statement contains it, its function does.
        let body = output
            .mutants
            .iter()
            .find(|m| m.mutator == "BlockStatement" && m.start == (1, 42))
            .unwrap();
        assert_eq!(body.statement, None);
        assert_eq!(body.function.as_deref(), Some(function));
    }

    #[test]
    fn decisions_are_linked_whole_by_condition_and_by_prefix() {
        let source = concat!(
            "export function f(a: number, b: number, c: number) {\n",
            "  if (a > 0 && b < 1 && c === 2) return 1;\n",
            "  if ((a > 0 || b > 0) && c > 0) return 2;\n",
            "  if (!(a > 1 && b > 1) || c > 1) return 3;\n",
            "  return 0;\n",
            "}\n",
        );
        let output = instrumented(source, false);
        let first = decision(&output, "a > 0 && b < 1 && c === 2");
        let link = |original: &str, mutator: &str, replacement: &str, line: u32| {
            output
                .mutants
                .iter()
                .find(|m| {
                    m.original == original
                        && m.mutator == mutator
                        && m.replacement == replacement
                        && m.start.0 == line
                })
                .unwrap_or_else(|| panic!("{mutator} on `{original}`"))
                .decision
                .clone()
        };
        let whole = |id: &str| {
            Some(DecisionLink {
                id: id.into(),
                condition: None,
                prefix: None,
            })
        };
        let condition = |id: &str, index| {
            Some(DecisionLink {
                id: id.into(),
                condition: Some(index),
                prefix: None,
            })
        };
        let prefix = |id: &str, width| {
            Some(DecisionLink {
                id: id.into(),
                condition: None,
                prefix: Some(width),
            })
        };
        assert_eq!(
            link(
                "a > 0 && b < 1 && c === 2",
                "ConditionalExpression",
                "false",
                2
            ),
            whole(first)
        );
        assert_eq!(
            link("a > 0", "ConditionalExpression", "true", 2),
            condition(first, 0)
        );
        assert_eq!(
            link("b < 1", "ConditionalExpression", "true", 2),
            condition(first, 1)
        );
        assert_eq!(
            link("c === 2", "ConditionalExpression", "true", 2),
            condition(first, 2)
        );
        assert_eq!(
            link("c === 2", "EqualityOperator", "!==", 2),
            condition(first, 2)
        );
        assert_eq!(
            link("a > 0 && b < 1", "ConditionalExpression", "true", 2),
            prefix(first, 2)
        );
        assert_eq!(
            link("a > 0 && b < 1", "LogicalOperator", "||", 2),
            prefix(first, 2)
        );
        let second = decision(&output, "(a > 0 || b > 0) && c > 0");
        assert_eq!(
            link("a > 0 || b > 0", "ConditionalExpression", "true", 3),
            prefix(second, 2)
        );
        assert_eq!(
            link("b > 0", "ConditionalExpression", "false", 3),
            condition(second, 1)
        );
        // Under a `!` the first two conditions are a prefix, but the
        // negated one around them is no `&&`/`||` of conditions.
        let third = decision(&output, "!(a > 1 && b > 1) || c > 1");
        assert_eq!(
            link("a > 1 && b > 1", "ConditionalExpression", "true", 4),
            prefix(third, 2)
        );
        assert_eq!(
            link(
                "!(a > 1 && b > 1) || c > 1",
                "ConditionalExpression",
                "true",
                4
            ),
            whole(third)
        );
        // An `if` test's truthiness is all the program reads of it.
        assert!(!mutant(&output, "a > 0 && b < 1", "LogicalOperator", "||").value_used);
    }

    #[test]
    fn logical_selections_and_their_operands_are_linked_from_the_syntax_tree() {
        let source = concat!(
            "export function g(a: number, b: number, x: unknown, y: unknown) {\n",
            "  const v = x ?? (a > b);\n",
            "  const w = (a < b) || x;\n",
            "  const z = x && y;\n",
            "  return [v, w, z, !(x || y)];\n",
            "}\n",
        );
        let output = instrumented(source, false);
        let coalesce = branch(&output, "logical-value", "x ?? (a > b)");
        assert_eq!(
            mutant(&output, "a > b", "ConditionalExpression", "true").branch,
            Some(BranchLink {
                id: coalesce.into(),
                role: BranchRole::Right,
                operator: "??".into()
            })
        );
        assert_eq!(
            mutant(&output, "x ?? (a > b)", "LogicalOperator", "&&").branch,
            Some(BranchLink {
                id: coalesce.into(),
                role: BranchRole::Value,
                operator: "??".into()
            })
        );
        let or = branch(&output, "logical-value", "(a < b) || x");
        assert_eq!(
            mutant(&output, "a < b", "ConditionalExpression", "false").branch,
            Some(BranchLink {
                id: or.into(),
                role: BranchRole::Left,
                operator: "||".into()
            })
        );
        // A selection whose value is kept can differ from `true` while
        // agreeing in truthiness; a comparison's value is its truthiness,
        // and a `!` reads only that.
        let kept = mutant(&output, "(a < b) || x", "ConditionalExpression", "true");
        assert!(kept.value_used);
        assert!(!mutant(&output, "a < b", "ConditionalExpression", "false").value_used);
        assert!(!mutant(&output, "a < b", "ConditionalExpression", "false").effects);
        assert!(mutant(&output, "x && y", "LogicalOperator", "||").value_used);
        assert!(!mutant(&output, "x || y", "LogicalOperator", "&&").value_used);
    }

    /// A condition that calls something has effects the literal replacing it
    /// skips; one that only defines a function that would does not.
    #[test]
    fn a_condition_s_effects_are_known_from_the_source() {
        let source = "export function t(a, f) {\n  if (a > 0 && !f(a)) return 1;\n  const g = () => a > 1 && f(a);\n  if (g && a > 2) return 2;\n  if ((() => f(a)) && a > 3) return 3;\n  return 0;\n}\n";
        let output = instrumented(source, false);
        let effects =
            |original: &str| mutant(&output, original, "ConditionalExpression", "false").effects;
        assert!(effects("a > 0 && !f(a)"));
        assert!(effects("a > 1 && f(a)"));
        assert!(!effects("g && a > 2"));
        assert!(!effects("(() => f(a)) && a > 3"));
    }

    #[test]
    fn an_optional_link_names_the_links_before_it_in_its_chain() {
        let source = "export const h = (o: any) => [o?.a?.b, (o?.c)?.d, o?.e?.()];\n";
        let output = instrumented(source, false);
        let id = |source: &str| branch(&output, "optional-chain", source).to_owned();
        let chain = |original: &str, link: &str| {
            mutant(&output, original, "OptionalChaining", link)
                .chain
                .clone()
        };
        assert_eq!(
            chain("o?.a", ".a"),
            Some(ChainLink {
                id: id("o?.a"),
                earlier: vec![]
            })
        );
        assert_eq!(
            chain("o?.a?.b", ".b"),
            Some(ChainLink {
                id: id("o?.a?.b"),
                earlier: vec![id("o?.a")]
            })
        );
        // A parenthesized chain is a chain of its own.
        assert_eq!(
            chain("(o?.c)?.d", ".d"),
            Some(ChainLink {
                id: id("(o?.c)?.d"),
                earlier: vec![]
            })
        );
        assert_eq!(
            chain("o?.e?.()", "()"),
            Some(ChainLink {
                id: id("o?.e?.()"),
                earlier: vec![id("o?.e")]
            })
        );
    }

    #[test]
    fn comparison_and_probe_sites_are_linked_only_where_placed() {
        let source = concat!(
            "export function plain(a: number, b: number, xs: number[]) {\n",
            "  return [a < b, a + b, xs.some((x) => x > a), xs.map((x) => x.toFixed())];\n",
            "}\n",
            "export const text = (function (a: number, b: number) { return [a < b, a + b]; }).toString();\n",
            "export const chained = (a: number, b: number, c: number) => [a < b < c];\n",
        );
        let probed = instrumented(source, true);
        let on_line = |line: u32, original: &str, mutator: &str, replacement: &str| {
            probed
                .mutants
                .iter()
                .find(|m| {
                    m.start.0 == line
                        && m.original == original
                        && m.mutator == mutator
                        && m.replacement == replacement
                })
                .unwrap_or_else(|| panic!("{mutator} on `{original}` at line {line}"))
        };
        assert_eq!(
            on_line(2, "a < b", "EqualityOperator", "<=")
                .compare
                .as_deref(),
            Some("src/links.ts:2:11")
        );
        assert_eq!(
            on_line(2, "a < b", "ConditionalExpression", "true")
                .compare
                .as_deref(),
            Some("src/links.ts:2:11")
        );
        assert_eq!(
            on_line(2, "a + b", "ArithmeticOperator", "-")
                .probe
                .as_deref(),
            Some("src/links.ts:2:18-2:23#arith")
        );
        let some = on_line(2, "xs.some((x) => x > a)", "MethodExpression", "every");
        assert_eq!(some.probe.as_deref(), Some("src/links.ts:2:25-2:46#method"));
        // The predicate's arrow is read by its method's probe.
        let predicate = on_line(2, "(x) => x > a", "ArrowFunction", "undefined");
        assert_eq!(
            predicate.probe.as_deref(),
            Some("src/links.ts:2:33-2:45#arrow")
        );
        assert_eq!(
            predicate.arrow,
            Some(ArrowLink {
                effects: false,
                never_undefined: true
            })
        );
        let mapped = on_line(2, "(x) => x.toFixed()", "ArrowFunction", "undefined");
        assert_eq!(mapped.probe, None);
        assert_eq!(
            mapped.arrow,
            Some(ArrowLink {
                effects: true,
                never_undefined: false
            })
        );
        // A function whose source is read is left as written: no probe, so
        // no link.
        assert_eq!(on_line(4, "a < b", "EqualityOperator", "<=").compare, None);
        assert_eq!(on_line(4, "a + b", "ArithmeticOperator", "-").probe, None);
        // Two comparisons at one site: neither is decided by it.
        assert_eq!(on_line(5, "a < b", "EqualityOperator", "<=").compare, None);
        assert_eq!(
            on_line(5, "a < b < c", "EqualityOperator", "<=").compare,
            None
        );
        // With the probes off nothing is placed.
        let plain = instrumented(source, false);
        assert!(
            plain
                .mutants
                .iter()
                .all(|m| m.compare.is_none() && m.probe.is_none())
        );
        assert_eq!(plain.mutants.len(), probed.mutants.len());
    }

    #[test]
    fn mutants_round_trip_through_their_json() {
        let source = "export const h = (o: any, a: number) => o?.a ?? a > 1;\n";
        let output = instrumented(source, true);
        let file = MutantsFile {
            schema_version: crate::mutant_list::MUTANTS_SCHEMA_VERSION,
            mutant_set: Some(crate::js_mutants::MUTANT_SET.into()),
            mutant_sets: Default::default(),
            languages: Default::default(),
            files: BTreeMap::from([("src/links.ts".to_owned(), output.mutants.clone())]),
            tests_without_values: Vec::new(),
        };
        // Written without the ids its mutants' places make.
        let encoded = serde_json::to_string(&file).unwrap();
        assert!(encoded.starts_with(
            "{\"schemaVersion\":1,\"mutantSet\":\"stryker-js@10.1.1\",\"files\":{\"src/links.ts\":[{\"mutator\":"
        ));
        let stored: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        for mutant in stored["files"]["src/links.ts"].as_array().unwrap() {
            assert!(mutant.get("id").is_none(), "{mutant}");
            // A site in the file is written from its line.
            if let Some(site) = mutant.get("compare") {
                assert_eq!(site, ":1:49");
            }
        }
        let decoded: MutantsFile = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, file);
    }

    /// What the program alone says of a mutant is linked to it and stored
    /// with it: a static rule's equivalence and an untestable boundary.
    #[test]
    fn static_rules_are_linked_and_stored() {
        let source = "export function f(a: number, b: number) {\n  const n = a - b;\n  return [n && n > 0, Date.now() > a, a > 0];\n}\n";
        let output = instrumented(source, true);
        let guarded = mutant(&output, "n > 0", "EqualityOperator", ">=");
        assert_eq!(
            guarded.equivalent.as_ref().map(|note| note.rule.as_str()),
            Some("zero-guarded")
        );
        assert_eq!(guarded.untestable, None);
        let other_way = mutant(&output, "n > 0", "EqualityOperator", "<=");
        assert_eq!(other_way.equivalent, None);
        let clock = mutant(&output, "Date.now() > a", "EqualityOperator", ">=");
        assert_eq!(clock.equivalent, None);
        assert_eq!(
            clock.untestable.as_ref().map(|note| note.rule.as_str()),
            Some("clock")
        );
        let plain = mutant(&output, "a > 0", "EqualityOperator", ">=");
        assert_eq!((&plain.equivalent, &plain.untestable), (&None, &None));
        let file = MutantsFile {
            schema_version: crate::mutant_list::MUTANTS_SCHEMA_VERSION,
            mutant_set: Some(crate::js_mutants::MUTANT_SET.into()),
            mutant_sets: Default::default(),
            languages: Default::default(),
            files: BTreeMap::from([("src/links.ts".to_owned(), output.mutants.clone())]),
            tests_without_values: Vec::new(),
        };
        let encoded = serde_json::to_string(&file).unwrap();
        assert!(encoded.contains("\"equivalent\":{\"rule\":\"zero-guarded\",\"reason\":"));
        assert!(encoded.contains("\"untestable\":{\"rule\":\"clock\",\"reason\":"));
        let decoded: MutantsFile = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, file);
        // The verdicts: equivalent before anything ran, the hint kept.
        let results = crate::js_weak_mutation::results(&[], Some(decoded)).unwrap();
        let listed = |original: &str, descriptor: &str| {
            results
                .mutants
                .iter()
                .find(|m| m.original == original && m.descriptor == descriptor)
                .unwrap()
                .clone()
        };
        let equivalent = listed("n > 0", ">=");
        assert_eq!(
            (equivalent.status, equivalent.code.as_str()),
            (
                crate::coverage_index::MutantStatus::Equivalent,
                "zero-guarded"
            )
        );
        let hinted = listed("Date.now() > a", ">=");
        assert_eq!(
            hinted.status,
            crate::coverage_index::MutantStatus::NotReached
        );
        assert_eq!(
            hinted.untestable.map(|(kind, _)| kind),
            Some(crate::coverage_index::Untestable::Clock)
        );
    }

    #[test]
    fn decisions_under_negations_and_coalescing_are_shaped_by_their_and_or_operators() {
        let source = concat!(
            "export function f(a: number, b: number, c: boolean, d: boolean, x?: boolean) {\n",
            "  if ((x ?? c) && d) return 1;\n",
            "  if (c && -a) return 2;\n",
            "  if (!!(c && d) || x) return 3;\n",
            "  if (!-a && c) return 4;\n",
            "  if (!(x ?? c) && d) return 5;\n",
            "  if ((!(c && d) || x) && a > b) return 6;\n",
            "  return 0;\n",
            "}\n",
        );
        let output = instrumented(source, false);
        let linked = |line: u32, original: &str, mutator: &str, replacement: &str| {
            output
                .mutants
                .iter()
                .find(|m| {
                    m.start.0 == line
                        && m.original == original
                        && m.mutator == mutator
                        && m.replacement == replacement
                })
                .unwrap_or_else(|| panic!("{mutator} on `{original}` at line {line}"))
                .decision
                .clone()
        };
        let condition = |source: &str, index| {
            Some(DecisionLink {
                id: decision(&output, source).into(),
                condition: Some(index),
                prefix: None,
            })
        };
        let prefix = |source: &str, width| {
            Some(DecisionLink {
                id: decision(&output, source).into(),
                condition: None,
                prefix: Some(width),
            })
        };
        // `??` is no `&&`/`||`: a coalescing is one condition, also under a
        // `!`.
        assert_eq!(
            linked(2, "x ?? c", "LogicalOperator", "&&"),
            condition("(x ?? c) && d", 0)
        );
        assert_eq!(
            linked(6, "!(x ?? c)", "BooleanLiteral", "removed"),
            condition("!(x ?? c) && d", 0)
        );
        // A negated number is a condition, and so is the `!` of one, whose
        // operand then is no condition of its own.
        assert_eq!(
            linked(3, "-a", "UnaryOperator", "+"),
            condition("c && -a", 1)
        );
        assert_eq!(
            linked(5, "!-a", "BooleanLiteral", "removed"),
            condition("!-a && c", 0)
        );
        assert_eq!(linked(5, "-a", "UnaryOperator", "+"), None);
        // Through two `!`s the `&&` is still the decision's first two
        // conditions.
        assert_eq!(
            linked(4, "c && d", "LogicalOperator", "||"),
            prefix("!!(c && d) || x", 2)
        );
        // An `||` over a negated `&&` starts the decision but is no prefix
        // made of `&&` and `||` alone.
        assert_eq!(
            linked(7, "c && d", "LogicalOperator", "||"),
            prefix("(!(c && d) || x) && a > b", 2)
        );
        assert_eq!(linked(7, "!(c && d) || x", "LogicalOperator", "&&"), None);
    }

    #[test]
    fn a_conditional_of_booleans_makes_its_logical_expression_boolean() {
        let source = "export const g = (a: number, b: number, c: boolean, x: unknown) => [(c ? a > b : true) || a < b, (c ? a > b : x) || a < b];\n";
        let output = instrumented(source, false);
        // Both arms boolean: the `||` is a boolean, whose value is its
        // truthiness.
        assert!(
            !mutant(
                &output,
                "(c ? a > b : true) || a < b",
                "LogicalOperator",
                "&&"
            )
            .value_used
        );
        assert!(mutant(&output, "(c ? a > b : x) || a < b", "LogicalOperator", "&&").value_used);
    }

    #[test]
    fn a_concise_arrow_is_never_undefined_through_type_syntax_sequences_and_assignments() {
        let source = concat!(
            "export const k = (x: any, y: any) => [\n",
            "  () => 1 as number,\n",
            "  () => 1 satisfies number,\n",
            "  () => x!,\n",
            "  () => <number>1,\n",
            "  () => (x, 1),\n",
            "  () => (x = 1),\n",
            "  () => (x = y),\n",
            "];\n",
        );
        let output = instrumented(source, false);
        let never_undefined = |original: &str| {
            mutant(&output, original, "ArrowFunction", "undefined")
                .arrow
                .expect("a concise arrow's body")
                .never_undefined
        };
        assert!(never_undefined("() => 1 as number"));
        assert!(never_undefined("() => 1 satisfies number"));
        assert!(!never_undefined("() => x!"));
        assert!(never_undefined("() => <number>1"));
        assert!(never_undefined("() => (x, 1)"));
        assert!(never_undefined("() => (x = 1)"));
        assert!(!never_undefined("() => (x = y)"));
    }

    #[test]
    fn an_operator_is_found_past_comments_and_spaces_only() {
        let source = "export function m(a: number, b: number) {\n  let c = a;\n  (c) += b;\n  return [c, a /* plus */ + b, a // minus\n    - b, a \u{feff}* b, a // times\r- b];\n}\n";
        let output = instrumented(source, false);
        let operator = |original: &str, replacement: &str| {
            mutant(&output, original, "ArithmeticOperator", replacement)
                .operator
                .clone()
        };
        assert_eq!(
            operator("a /* plus */ + b", "-"),
            Some(MutantOperator {
                text: "+".into(),
                at: 13
            })
        );
        assert_eq!(
            operator("a // minus\n    - b", "+"),
            Some(MutantOperator {
                text: "-".into(),
                at: 15
            })
        );
        // The parentheses around an assignment's target are not the target's.
        assert_eq!(
            mutant(&output, "(c) += b", "AssignmentOperator", "-=").operator,
            Some(MutantOperator {
                text: "+=".into(),
                at: 4
            })
        );
        // A byte order mark is JavaScript whitespace, and a carriage return
        // ends a line comment as a line feed does.
        assert_eq!(
            operator("a \u{feff}* b", "/"),
            Some(MutantOperator {
                text: "*".into(),
                at: 5
            })
        );
        assert_eq!(
            operator("a // times\r- b", "+"),
            Some(MutantOperator {
                text: "-".into(),
                at: 11
            })
        );
        // A script's HTML-like comment is not read: no operator is shown.
        MUTANTS_FOR_TEST.with(|flag| flag.set(Some(true)));
        let script = instrument_candidate(
            "exports.m = function (a, b) {\n  return a <!-- plus\n    + b;\n};\n",
            "src/legacy.cjs",
        )
        .unwrap();
        MUTANTS_FOR_TEST.with(|flag| flag.set(None));
        assert_eq!(
            mutant(&script, "a <!-- plus\n    + b", "ArithmeticOperator", "-").operator,
            None
        );
    }

    #[test]
    fn private_fields_and_non_null_assertions_are_links_of_a_chain() {
        let source = concat!(
            "export class C {\n",
            "  #x: { y: number } | undefined;\n",
            "  f(o?: C) {\n",
            "    return [o?.#x?.y, this.#x?.y, o!?.#x];\n",
            "  }\n",
            "}\n",
        );
        let output = instrumented(source, false);
        let id = |source: &str| branch(&output, "optional-chain", source).to_owned();
        let chain = |original: &str, link: &str| {
            mutant(&output, original, "OptionalChaining", link)
                .chain
                .clone()
        };
        assert_eq!(
            chain("o?.#x?.y", ".y"),
            Some(ChainLink {
                id: id("o?.#x?.y"),
                earlier: vec![id("o?.#x")]
            })
        );
        assert_eq!(
            chain("this.#x?.y", ".y"),
            Some(ChainLink {
                id: id("this.#x?.y"),
                earlier: vec![]
            })
        );
        assert_eq!(
            chain("o!?.#x", ".#x"),
            Some(ChainLink {
                id: id("o!?.#x"),
                earlier: vec![]
            })
        );
    }

    #[test]
    fn a_span_is_found_only_at_a_line_and_column_that_hold_its_text() {
        let lines = Lines::new("ab\n");
        assert_eq!(lines.span(1, 2, "b"), Some(Span::new(1, 2)));
        assert_eq!(lines.span(2, 1, ""), Some(Span::new(3, 3)));
        assert_eq!(lines.span(0, 1, "a"), None);
        assert_eq!(lines.span(3, 1, ""), None);
        assert_eq!(lines.span(1, 0, "a"), None);
        assert_eq!(lines.span(1, 5, "a"), None);
    }

    #[test]
    fn evidence_that_names_no_known_code_links_nothing() {
        let source = "export function f(a: number, b: number, x: unknown) {\n  if (a > b && b > 0) return x || a;\n  return 0;\n}\n";
        let output = instrumented(source, false);
        let allocator = oxc_allocator::Allocator::default();
        let parsed =
            oxc_parser::Parser::new(&allocator, source, oxc_span::SourceType::ts()).parse();
        let ledger = crate::js_mutants::enumerate(&parsed.program, source);
        let whole = output
            .decisions
            .iter()
            .find(|decision| decision.source == "a > b && b > 0")
            .expect("the decision");
        let span = Lines::new(source)
            .span(whole.line, whole.column, &whole.source)
            .expect("the decision's span");
        let mut elsewhere = whole.clone();
        elsewhere.line = 9;
        let decisions = [elsewhere, whole.clone()];
        let evidence = Evidence {
            points: &output.points,
            decisions: &decisions,
            branches: &output.branches,
            comparisons: &[],
            probes: &[],
            reads: &HashMap::default(),
        };
        let decision_of = |linked: &[CandidateMutant], original: &str| {
            linked
                .iter()
                .find(|m| m.original == original && m.mutator == "ConditionalExpression")
                .unwrap_or_else(|| panic!("a mutant of `{original}`"))
                .decision
                .clone()
        };
        // Facts that know the decision as a whole and none of its parts, and
        // no logical expression's operands.
        let mut facts = MutantFacts::default();
        facts.decisions.insert(key(span), DecisionShape::default());
        let linked = link("src/links.ts", source, ledger.clone(), &facts, &evidence);
        assert_eq!(
            decision_of(&linked, "a > b && b > 0"),
            Some(DecisionLink {
                id: whole.id.clone(),
                condition: None,
                prefix: None
            })
        );
        assert_eq!(decision_of(&linked, "a > b"), None);
        assert!(linked.iter().all(|m| m.branch.is_none()));
        // Facts that do not know the decision link nothing to it.
        let linked = link(
            "src/links.ts",
            source,
            ledger,
            &MutantFacts::default(),
            &evidence,
        );
        assert!(linked.iter().all(|m| m.decision.is_none()));
    }

    #[test]
    fn a_stored_file_keeps_the_ids_and_sites_it_cannot_work_out_again() {
        let output = instrumented("export const h = (a: number, b: number) => a < b;\n", false);
        let mut moved = mutant(&output, "a < b", "EqualityOperator", "<=").clone();
        moved.id = "src/old.ts:1:1-1:6:EqualityOperator:<=".into();
        moved.compare = Some("src/other.ts:3:7".into());
        let file = MutantsFile {
            schema_version: crate::mutant_list::MUTANTS_SCHEMA_VERSION,
            mutant_set: Some(crate::js_mutants::MUTANT_SET.into()),
            mutant_sets: Default::default(),
            languages: Default::default(),
            files: BTreeMap::from([
                ("src/empty.ts".to_owned(), Vec::new()),
                ("src/links.ts".to_owned(), vec![moved.clone()]),
            ]),
            tests_without_values: Vec::new(),
        };
        let encoded = serde_json::to_string(&file).unwrap();
        let stored: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        let written = &stored["files"]["src/links.ts"][0];
        assert_eq!(written["id"], moved.id.as_str());
        assert_eq!(written["compare"], "src/other.ts:3:7");
        assert_eq!(stored["files"]["src/empty.ts"], serde_json::json!([]));
        let decoded: MutantsFile = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, file);
        // A run that enumerated no file's mutants is read back as one.
        let none = MutantsFile {
            files: BTreeMap::new(),
            ..file
        };
        let encoded = serde_json::to_string(&none).unwrap();
        assert_eq!(serde_json::from_str::<MutantsFile>(&encoded).unwrap(), none);
    }

    #[test]
    fn listing_a_runs_mutants_keeps_unchanged_files_and_names_the_changed_ones() {
        let root =
            std::env::temp_dir().join(format!("supercov-list-mutants-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        let kept = "export const f = (a: number) => a + 1;\n";
        let plain = "export {};\n";
        for (file, source) in [
            ("src/kept.ts", kept),
            ("src/plain.ts", plain),
            ("src/edited.ts", kept),
            ("src/broken.ts", "export const = ;\n"),
        ] {
            std::fs::write(root.join(file), source).unwrap();
        }
        let points = |file: &str, source: &str| {
            crate::js_instrumenter::listing_mutants(|| {
                crate::js_instrumenter::instrument_with_import_policy(
                    source,
                    file,
                    "capability.mjs",
                    false,
                )
            })
            .unwrap()
            .points
            .into_iter()
            .map(|point| point.id)
            .collect::<std::collections::BTreeSet<_>>()
        };
        let recorded = BTreeMap::from([
            ("src/kept.ts".to_owned(), points("src/kept.ts", kept)),
            ("src/plain.ts".to_owned(), points("src/plain.ts", plain)),
            (
                "src/edited.ts".to_owned(),
                std::collections::BTreeSet::from(["src/edited.ts:9:1".to_owned()]),
            ),
            ("src/broken.ts".to_owned(), Default::default()),
            ("src/missing.ts".to_owned(), Default::default()),
            ("styles/site.css".to_owned(), Default::default()),
        ]);
        let listed = list_mutants(&root, &recorded);
        let nothing = list_mutants(&root, &BTreeMap::new());
        std::fs::remove_dir_all(&root).unwrap();
        // A file without mutants is listed with none; a stylesheet is no
        // JavaScript file, changed or not.
        assert_eq!(
            listed.mutants.files.keys().collect::<Vec<_>>(),
            ["src/kept.ts"]
        );
        assert!(
            listed.mutants.files["src/kept.ts"]
                .iter()
                .any(|m| m.mutator == "ArithmeticOperator")
        );
        assert_eq!(
            listed.changed,
            ["src/broken.ts", "src/edited.ts", "src/missing.ts"]
        );
        assert_eq!(
            listed.mutants.mutant_set.as_deref(),
            Some(crate::js_mutants::MUTANT_SET)
        );
        assert!(nothing.mutants.files.is_empty() && nothing.changed.is_empty());
    }

    /// The JavaScript of every source a file of mutants could be made of:
    /// the StrykerJS parity fixtures and the runtime's own modules.
    fn corpus() -> Vec<(String, String)> {
        let crate_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut sources = Vec::new();
        for directory in ["tests/fixtures/stryker-parity", "runtime-assets/javascript"] {
            let mut entries = std::fs::read_dir(crate_root.join(directory))
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .filter(|path| {
                    path.extension().is_some_and(|extension| {
                        ["js", "mjs", "cjs", "ts", "tsx"].contains(&extension.to_str().unwrap())
                    })
                })
                .collect::<Vec<_>>();
            entries.sort();
            for path in entries {
                let name = format!(
                    "{directory}/{}",
                    path.file_name().unwrap().to_str().unwrap()
                );
                sources.push((name, std::fs::read_to_string(path).unwrap()));
            }
        }
        sources
    }

    /// A stored file of mutants is read back as one that shows each mutant
    /// exactly as the whole one did, in less: every verdict and every text
    /// shown is the same, from no evidence and from every point run and
    /// every probe seen.
    #[test]
    fn a_stored_file_of_mutants_shows_what_the_whole_one_did_in_less() {
        use crate::js_weak_mutation::{Observations, verdict};
        MUTANTS_FOR_TEST.with(|flag| flag.set(Some(true)));
        let mut files = BTreeMap::new();
        for (name, source) in corpus() {
            let output = instrument_candidate(&source, &name).unwrap();
            files.insert(name, output.mutants);
        }
        MUTANTS_FOR_TEST.with(|flag| flag.set(None));
        let file = MutantsFile {
            schema_version: crate::mutant_list::MUTANTS_SCHEMA_VERSION,
            mutant_set: Some(crate::js_mutants::MUTANT_SET.into()),
            mutant_sets: Default::default(),
            languages: Default::default(),
            files,
            tests_without_values: Vec::new(),
        };
        let whole = file.files.values().flatten().collect::<Vec<_>>();
        let count = whole.len();
        assert!(count > 3_000, "{count} mutants");
        // Every mutant with its id and its whole original.
        let whole_list = serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 1,
            "mutantSet": file.mutant_set,
            "files": file.files,
        }))
        .unwrap();
        let stored = serde_json::to_vec(&file).unwrap();
        let read: MutantsFile = serde_json::from_slice(&stored).unwrap();
        let kept = read.files.values().flatten().collect::<Vec<_>>();
        assert_eq!(kept.len(), count);
        // What a run saw: nothing, and then everything.
        let mut ran = Observations::default();
        for mutant in &whole {
            for point in [&mutant.statement, &mutant.function].into_iter().flatten() {
                ran.hits.insert(point.clone());
            }
            if let Some(chain) = &mutant.chain {
                ran.hits.insert(format!("{}:continued", chain.id));
            }
            for site in [&mutant.compare, &mutant.probe].into_iter().flatten() {
                ran.comparisons.insert(
                    site.clone(),
                    ["lt", "same"].map(str::to_owned).into_iter().collect(),
                );
            }
            if let Some(branch) = &mutant.branch {
                ran.logicals
                    .insert(branch.id.clone(), [(true, false)].into_iter().collect());
            }
        }
        let mut abbreviated = 0;
        for (whole, kept) in whole.iter().zip(&kept) {
            assert_eq!(whole.id, kept.id);
            if whole.original != kept.original {
                abbreviated += 1;
            }
            for observations in [&Observations::default(), &ran] {
                assert_eq!(
                    verdict(whole, observations),
                    verdict(kept, observations),
                    "{}",
                    whole.id
                );
            }
        }
        let listed = |file: MutantsFile| {
            crate::js_weak_mutation::results(&[], Some(file))
                .unwrap()
                .mutants
        };
        assert_eq!(listed(read), listed(file));
        assert!(abbreviated > 100, "{abbreviated} of {count} originals cut");
        let gzip = |bytes: &[u8]| {
            use std::io::Write as _;
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(bytes).unwrap();
            encoder.finish().unwrap().len()
        };
        let sizes = format!(
            "{count} mutants, {abbreviated} originals cut: whole {} bytes ({} gzipped), stored {} bytes ({} gzipped)",
            whole_list.len(),
            gzip(&whole_list),
            stored.len(),
            gzip(&stored)
        );
        assert!(stored.len() * 10 < whole_list.len() * 7, "{sizes}");
    }
}
