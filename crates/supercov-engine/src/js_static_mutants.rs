//! What the program as written says of a mutant before any test runs: that
//! no input can make it differ from the original (an equivalent mutant), or
//! that the input which would is practically out of a test's reach.
//!
//! A mutant is called equivalent only by a rule that holds for every input
//! under JavaScript's semantics (Offutt and Pan 1997; Papadakis et al.,
//! "Trivial Compiler Equivalence", ICSE 2015). A rule that holds only for
//! the inputs a program usually sees is a hint at most, and changes no
//! status. `docs/design/mutants.md` lists each rule and why it is sound, and
//! the rules considered and left out.

use std::collections::HashMap;

use oxc_ast::AstKind;
use oxc_ast::ast::{
    ArrowFunctionExpression, BinaryExpression, CallExpression, Expression, Function,
    LogicalExpression, Program, ReturnStatement, Statement, VariableDeclaration,
    VariableDeclarationKind, WithStatement,
};
use oxc_ast_visit::{Visit, walk};
use oxc_span::{GetSpan, Span};
use oxc_syntax::operator::{AssignmentOperator, BinaryOperator, LogicalOperator, UnaryOperator};
use oxc_syntax::scope::{ScopeFlags, ScopeId};

use crate::js_mutant_links::StaticNote;
use crate::js_mutants::LedgerMutant;

/// `x > 0` and `x >= 0` (or `<` and `<=`) differ only where `x` is 0, and
/// an earlier `x &&` (or `!x ||`) in the same chain already left 0 out. Only
/// for an `x` that holds a number or a bigint: see `numeric`.
pub(crate) const ZERO_GUARDED: &str = "zero-guarded";
/// A boundary comparison that reads the clock.
pub(crate) const CLOCK: &str = "clock";
/// A boundary comparison that reads `Math.random()`.
pub(crate) const RANDOM: &str = "random";

type SpanKey = (u32, u32);

fn key(span: Span) -> SpanKey {
    (span.start, span.end)
}

fn within(inner: Span, outer: Span) -> bool {
    outer.start <= inner.start && inner.end <= outer.end
}

/// A relational comparison: its operator, and whether an operand is a
/// clock or random reading (`CLOCK`, `RANDOM`).
#[derive(Debug)]
struct Comparison {
    operator: &'static str,
    untestable: Option<&'static str>,
}

/// A comparison of an identifier with the literal 0 inside a chain whose
/// earlier operands found that identifier truthy.
#[derive(Debug)]
struct GuardedZero {
    name: String,
    /// `&&` for `x && … x > 0`, `||` for `!x || … x > 0`.
    guard: &'static str,
}

/// Where a value comes from that one exact reading of decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Clock,
    Random,
}

#[derive(Debug, Default)]
pub(crate) struct StaticFacts {
    comparisons: HashMap<SpanKey, Comparison>,
    guarded_zero: HashMap<SpanKey, GuardedZero>,
    /// How many bindings of each name the file declares, of any kind.
    bindings: HashMap<String, usize>,
    /// The `const`s whose initializer is always a number or a bigint, and
    /// the span their binding is visible in.
    numeric: HashMap<String, Span>,
    /// A `with` statement or a reference to `eval`: a name may then be bound
    /// where no declaration says.
    dynamic_scope: bool,
}

/// What `untestable` reads of the program, kept while it is visited.
#[derive(Default)]
struct Values<'a> {
    /// Relational comparisons and their innermost function.
    comparisons: Vec<(&'a BinaryExpression<'a>, Option<Span>)>,
    /// `const`s with an identifier binding and an initializer, and their
    /// innermost function.
    constants: Vec<(String, &'a Expression<'a>, Option<Span>)>,
    /// Functions a name declares (a function declaration's, or a `const`'s
    /// arrow or function expression), by the function's span.
    local_functions: HashMap<String, Span>,
    /// What each function returns: its `return`s' arguments, or a concise
    /// arrow's body.
    returns: HashMap<SpanKey, Vec<&'a Expression<'a>>>,
}

struct Collector<'a> {
    facts: StaticFacts,
    values: Values<'a>,
    /// Each node entered: the span a scope it opens covers (a function's
    /// body, not its parameters, which do not see the body's declarations).
    nodes: Vec<Span>,
    scopes: Vec<Span>,
    functions: Vec<Span>,
    /// Names the operands evaluated before this point of a `&&`/`||` chain
    /// found truthy: bare `x` before `&&`, `!x` before `||`.
    truthy: Vec<(String, &'static str)>,
}

impl StaticFacts {
    pub(crate) fn collect(program: &Program<'_>) -> Self {
        let mut collector = Collector {
            facts: StaticFacts::default(),
            values: Values::default(),
            nodes: Vec::new(),
            scopes: Vec::new(),
            functions: Vec::new(),
            truthy: Vec::new(),
        };
        collector.visit_program(program);
        let Collector {
            mut facts, values, ..
        } = collector;
        for (binary, function) in &values.comparisons {
            let untestable = [Source::Clock, Source::Random].into_iter().find(|source| {
                [&binary.left, &binary.right]
                    .into_iter()
                    .any(|operand| values.valued(operand, *source, *function, 2))
            });
            if let Some(comparison) = facts.comparisons.get_mut(&key(binary.span)) {
                comparison.untestable = untestable.map(|source| match source {
                    Source::Clock => CLOCK,
                    Source::Random => RANDOM,
                });
            }
        }
        facts
    }

    /// The rule that shows `mutant` equivalent, with why, if one does.
    pub(crate) fn equivalent(&self, mutant: &LedgerMutant) -> Option<StaticNote> {
        if mutant.mutator != "EqualityOperator" || self.dynamic_scope {
            return None;
        }
        let comparison = self.comparisons.get(&key(mutant.span))?;
        if boundary_twin(comparison.operator) != Some(mutant.replacement.as_str()) {
            return None;
        }
        let guarded = self.guarded_zero.get(&key(mutant.span))?;
        let name = guarded.name.as_str();
        let scope = self.numeric.get(name)?;
        if self.bindings.get(name) != Some(&1) || !within(mutant.span, *scope) {
            return None;
        }
        let guard = match guarded.guard {
            "&&" => format!("`{name} &&`"),
            _ => format!("`!{name} ||`"),
        };
        Some(StaticNote {
            rule: ZERO_GUARDED.to_owned(),
            reason: format!(
                "`{}` and `{}` differ only where `{name}` is 0, and `{name}` is a const number the {guard} before it has found not 0",
                comparison.operator, mutant.replacement
            ),
        })
    }

    /// Whether only one exact clock or random reading tells `mutant` from
    /// the original: a boundary comparison (`<` and `<=`, `>` and `>=`) of
    /// what reads the clock or `Math.random()`.
    pub(crate) fn untestable(&self, mutant: &LedgerMutant) -> Option<StaticNote> {
        if mutant.mutator != "EqualityOperator" {
            return None;
        }
        let comparison = self.comparisons.get(&key(mutant.span))?;
        if boundary_twin(comparison.operator) != Some(mutant.replacement.as_str()) {
            return None;
        }
        match comparison.untestable? {
            CLOCK => Some(StaticNote {
                rule: CLOCK.to_owned(),
                reason: "an operand is a clock reading: they differ only on one exact millisecond"
                    .to_owned(),
            }),
            _ => Some(StaticNote {
                rule: RANDOM.to_owned(),
                reason:
                    "an operand is a Math.random() reading: they differ only at one exact random value"
                        .to_owned(),
            }),
        }
    }
}

const DATE_FNS_PREFIXES: [&str; 5] = ["add", "sub", "differenceIn", "startOf", "endOf"];

/// A date-fns function that turns a time into a time or a length of time:
/// `addMinutes`, `subDays`, `differenceInSeconds`, `startOfDay`, `endOfMonth`.
fn date_fns_time(name: &str) -> bool {
    DATE_FNS_PREFIXES.iter().any(|prefix| {
        name.strip_prefix(prefix)
            .and_then(|rest| rest.chars().next())
            .is_some_and(char::is_uppercase)
    })
}

/// An operand arithmetic may combine with a reading without making it any
/// less one: a number literal, a name, or a member path of names.
fn plain(expression: &Expression<'_>) -> bool {
    match bare(expression) {
        Expression::NumericLiteral(_) | Expression::Identifier(_) => true,
        Expression::StaticMemberExpression(member) => plain(&member.object),
        _ => false,
    }
}

impl<'a> Values<'a> {
    /// Whether `expression` is a clock (or random) reading itself, and not
    /// only something worked out from one: the reading; arithmetic of it
    /// with readings, numbers and names; `-x` and `+x`; `.getTime()`,
    /// `.valueOf()`, `Number()` and `new Date()` of it; a date-fns `add*`,
    /// `sub*`, `differenceIn*`, `startOf*` or `endOf*` of it; a `??` or `||`
    /// fallback to it; and, up to `hops` of them, a `const` of an enclosing
    /// function whose initializer is one or a local function whose return
    /// is one. A count, a found element or a property read (`.length`,
    /// `.find(...)`) of something worked out from the clock is not.
    fn valued(
        &self,
        expression: &Expression<'a>,
        source: Source,
        function: Option<Span>,
        hops: usize,
    ) -> bool {
        let valued = |expression: &Expression<'a>| self.valued(expression, source, function, hops);
        match bare(expression) {
            Expression::CallExpression(call) => {
                if let Some(read) = static_call(call) {
                    let reading = match source {
                        Source::Clock => matches!(read, ("Date" | "performance", "now")),
                        Source::Random => read == ("Math", "random"),
                    };
                    if reading {
                        return true;
                    }
                }
                let mut arguments = call
                    .arguments
                    .iter()
                    .filter_map(|argument| argument.as_expression());
                match bare(&call.callee) {
                    Expression::StaticMemberExpression(member)
                        if call.arguments.is_empty()
                            && matches!(member.property.name.as_str(), "getTime" | "valueOf") =>
                    {
                        valued(&member.object)
                    }
                    Expression::Identifier(callee)
                        if callee.name == "Number" || date_fns_time(&callee.name) =>
                    {
                        arguments.any(valued)
                    }
                    Expression::Identifier(callee) if hops > 0 => {
                        let Some(span) = self.local_functions.get(callee.name.as_str()) else {
                            return false;
                        };
                        self.returns.get(&key(*span)).is_some_and(|returns| {
                            returns
                                .iter()
                                .any(|value| self.valued(value, source, Some(*span), hops - 1))
                        })
                    }
                    _ => false,
                }
            }
            Expression::NewExpression(new) => {
                let date = matches!(bare(&new.callee), Expression::Identifier(callee) if callee.name == "Date");
                date && match new.arguments.as_slice() {
                    [] => source == Source::Clock,
                    [argument] => argument.as_expression().is_some_and(valued),
                    _ => false,
                }
            }
            Expression::BinaryExpression(binary) => {
                matches!(
                    binary.operator,
                    BinaryOperator::Addition
                        | BinaryOperator::Subtraction
                        | BinaryOperator::Multiplication
                        | BinaryOperator::Division
                ) && {
                    match (valued(&binary.left), valued(&binary.right)) {
                        (true, true) => true,
                        (true, false) => plain(&binary.right),
                        (false, true) => plain(&binary.left),
                        (false, false) => false,
                    }
                }
            }
            Expression::UnaryExpression(unary) => {
                matches!(
                    unary.operator,
                    UnaryOperator::UnaryNegation | UnaryOperator::UnaryPlus
                ) && valued(&unary.argument)
            }
            Expression::LogicalExpression(logical) => {
                logical.operator != LogicalOperator::And
                    && (valued(&logical.left) || valued(&logical.right))
            }
            Expression::Identifier(identifier) if hops > 0 => self
                .constant(identifier.name.as_str(), identifier.span, function)
                .is_some_and(|(init, scope)| self.valued(init, source, scope, hops - 1)),
            _ => false,
        }
    }

    /// The `const` of `name` a read at `at` in `function` sees, among those
    /// of the functions around it: the innermost one's.
    fn constant(
        &self,
        name: &str,
        at: Span,
        function: Option<Span>,
    ) -> Option<(&'a Expression<'a>, Option<Span>)> {
        let inside = function.unwrap_or(at);
        self.constants
            .iter()
            .filter(|(constant, init, scope)| {
                constant == name
                    && !within(at, init.span())
                    && scope.is_some_and(|scope| within(inside, scope))
            })
            .min_by_key(|(_, _, scope)| scope.map_or(u32::MAX, |scope| scope.end - scope.start))
            .map(|(_, init, scope)| (*init, *scope))
    }
}

/// The relational operator that differs from `operator` only where its
/// operands are equal.
fn boundary_twin(operator: &str) -> Option<&'static str> {
    match operator {
        "<" => Some("<="),
        "<=" => Some("<"),
        ">" => Some(">="),
        ">=" => Some(">"),
        _ => None,
    }
}

fn relational(operator: BinaryOperator) -> bool {
    matches!(
        operator,
        BinaryOperator::LessThan
            | BinaryOperator::LessEqualThan
            | BinaryOperator::GreaterThan
            | BinaryOperator::GreaterEqualThan
    )
}

/// The expression under parentheses and TypeScript's type-only wrappers,
/// which change nothing at run time.
fn bare<'b, 'a>(expression: &'b Expression<'a>) -> &'b Expression<'a> {
    match expression {
        Expression::ParenthesizedExpression(inner) => bare(&inner.expression),
        Expression::TSAsExpression(inner) => bare(&inner.expression),
        Expression::TSSatisfiesExpression(inner) => bare(&inner.expression),
        Expression::TSNonNullExpression(inner) => bare(&inner.expression),
        Expression::TSTypeAssertion(inner) => bare(&inner.expression),
        other => other,
    }
}

fn zero(expression: &Expression<'_>) -> bool {
    match bare(expression) {
        Expression::NumericLiteral(literal) => literal.value == 0.0,
        Expression::BigIntLiteral(literal) => literal.value.as_str() == "0",
        _ => false,
    }
}

/// An expression whose value is always a number or a bigint (or that
/// throws): unary `-`, `+` and `~`, arithmetic and bitwise operators, and
/// `+` of two such. A number or bigint is truthy exactly where it is neither
/// 0 nor NaN, which is what makes `zero-guarded` sound; a string such as
/// `"0"` or `" "`, an empty array or a `Date` at the epoch is truthy and
/// compares as 0, so nothing else qualifies.
fn numeric(expression: &Expression<'_>) -> bool {
    match bare(expression) {
        Expression::NumericLiteral(_)
        | Expression::BigIntLiteral(_)
        | Expression::UpdateExpression(_) => true,
        Expression::UnaryExpression(unary) => matches!(
            unary.operator,
            UnaryOperator::UnaryNegation | UnaryOperator::UnaryPlus | UnaryOperator::BitwiseNot
        ),
        Expression::BinaryExpression(binary) => match binary.operator {
            BinaryOperator::Addition => numeric(&binary.left) && numeric(&binary.right),
            BinaryOperator::Subtraction
            | BinaryOperator::Multiplication
            | BinaryOperator::Division
            | BinaryOperator::Remainder
            | BinaryOperator::Exponential
            | BinaryOperator::ShiftLeft
            | BinaryOperator::ShiftRight
            | BinaryOperator::ShiftRightZeroFill
            | BinaryOperator::BitwiseOR
            | BinaryOperator::BitwiseXOR
            | BinaryOperator::BitwiseAnd => true,
            _ => false,
        },
        Expression::ConditionalExpression(conditional) => {
            numeric(&conditional.consequent) && numeric(&conditional.alternate)
        }
        Expression::SequenceExpression(sequence) => {
            sequence.expressions.last().is_some_and(numeric)
        }
        Expression::AssignmentExpression(assignment) => match assignment.operator {
            AssignmentOperator::Assign => numeric(&assignment.right),
            AssignmentOperator::Addition => false,
            operator => !operator.is_logical(),
        },
        _ => false,
    }
}

/// The names a `&&`'s left side, when truthy, shows truthy: each bare
/// identifier among its `&&`-joined operands.
fn conjuncts(expression: &Expression<'_>, names: &mut Vec<String>) {
    match bare(expression) {
        Expression::LogicalExpression(logical) if logical.operator == LogicalOperator::And => {
            conjuncts(&logical.left, names);
            conjuncts(&logical.right, names);
        }
        Expression::Identifier(identifier) => names.push(identifier.name.to_string()),
        _ => {}
    }
}

/// The names a `||`'s left side, when falsy, shows truthy: each `!x` among
/// its `||`-joined operands.
fn negated_disjuncts(expression: &Expression<'_>, names: &mut Vec<String>) {
    match bare(expression) {
        Expression::LogicalExpression(logical) if logical.operator == LogicalOperator::Or => {
            negated_disjuncts(&logical.left, names);
            negated_disjuncts(&logical.right, names);
        }
        Expression::UnaryExpression(unary) if unary.operator == UnaryOperator::LogicalNot => {
            if let Expression::Identifier(identifier) = bare(&unary.argument) {
                names.push(identifier.name.to_string());
            }
        }
        _ => {}
    }
}

/// `Date.now()`, `performance.now()` or `Math.random()`: the object and
/// method called with no arguments.
fn static_call<'c>(call: &'c CallExpression<'_>) -> Option<(&'c str, &'c str)> {
    if !call.arguments.is_empty() {
        return None;
    }
    let Expression::StaticMemberExpression(member) = bare(&call.callee) else {
        return None;
    };
    let Expression::Identifier(object) = bare(&member.object) else {
        return None;
    };
    Some((object.name.as_str(), member.property.name.as_str()))
}

impl<'a> Collector<'a> {
    fn comparison(&mut self, binary: &BinaryExpression<'a>) {
        if !relational(binary.operator) {
            return;
        }
        self.facts.comparisons.insert(
            key(binary.span),
            Comparison {
                operator: binary.operator.as_str(),
                untestable: None,
            },
        );
        let at = self.alloc(binary);
        self.values
            .comparisons
            .push((at, self.functions.last().copied()));
        let operand = if zero(&binary.right) {
            &binary.left
        } else if zero(&binary.left) {
            &binary.right
        } else {
            return;
        };
        let Expression::Identifier(identifier) = bare(operand) else {
            return;
        };
        if let Some((name, guard)) = self
            .truthy
            .iter()
            .find(|(name, _)| name == identifier.name.as_str())
        {
            self.facts.guarded_zero.insert(
                key(binary.span),
                GuardedZero {
                    name: name.clone(),
                    guard,
                },
            );
        }
    }
}

impl<'a> Visit<'a> for Collector<'a> {
    fn enter_node(&mut self, kind: AstKind<'a>) {
        let span = match kind {
            AstKind::Function(function) => function
                .body
                .as_ref()
                .map_or(function.span, |body| body.span),
            AstKind::ArrowFunctionExpression(arrow) => arrow.body.span,
            other => other.span(),
        };
        self.nodes.push(span);
    }

    fn leave_node(&mut self, _kind: AstKind<'a>) {
        self.nodes.pop();
    }

    fn enter_scope(&mut self, _flags: ScopeFlags, _scope_id: &std::cell::Cell<Option<ScopeId>>) {
        let span = self.nodes.last().copied().unwrap_or_default();
        self.scopes.push(span);
    }

    fn leave_scope(&mut self) {
        self.scopes.pop();
    }

    fn visit_expression(&mut self, expression: &Expression<'a>) {
        match expression {
            // A chain: what its earlier operands showed still holds.
            Expression::LogicalExpression(_) | Expression::ParenthesizedExpression(_) => {
                walk::walk_expression(self, expression);
            }
            other => {
                if let Expression::BinaryExpression(binary) = other {
                    self.comparison(binary);
                }
                let outer = std::mem::take(&mut self.truthy);
                walk::walk_expression(self, other);
                self.truthy = outer;
            }
        }
    }

    fn visit_logical_expression(&mut self, logical: &LogicalExpression<'a>) {
        self.visit_expression(&logical.left);
        let mut names = Vec::new();
        let guard = match logical.operator {
            LogicalOperator::And => {
                conjuncts(&logical.left, &mut names);
                "&&"
            }
            LogicalOperator::Or => {
                negated_disjuncts(&logical.left, &mut names);
                "||"
            }
            LogicalOperator::Coalesce => "??",
        };
        let depth = self.truthy.len();
        self.truthy
            .extend(names.into_iter().map(|name| (name, guard)));
        self.visit_expression(&logical.right);
        self.truthy.truncate(depth);
    }

    fn visit_binding_identifier(&mut self, identifier: &oxc_ast::ast::BindingIdentifier<'a>) {
        *self
            .facts
            .bindings
            .entry(identifier.name.to_string())
            .or_default() += 1;
    }

    fn visit_identifier_reference(&mut self, identifier: &oxc_ast::ast::IdentifierReference<'a>) {
        if identifier.name == "eval" {
            self.facts.dynamic_scope = true;
        }
    }

    fn visit_with_statement(&mut self, statement: &WithStatement<'a>) {
        self.facts.dynamic_scope = true;
        walk::walk_with_statement(self, statement);
    }

    fn visit_variable_declaration(&mut self, declaration: &VariableDeclaration<'a>) {
        if declaration.kind == VariableDeclarationKind::Const {
            for declarator in &declaration.declarations {
                let (Some(identifier), Some(init)) =
                    (declarator.id.get_binding_identifier(), &declarator.init)
                else {
                    continue;
                };
                let name = identifier.name.to_string();
                if numeric(init)
                    && let Some(scope) = self.scopes.last()
                {
                    self.facts.numeric.insert(name.clone(), *scope);
                }
                if matches!(
                    bare(init),
                    Expression::ArrowFunctionExpression(_) | Expression::FunctionExpression(_)
                ) {
                    self.values
                        .local_functions
                        .insert(name.clone(), bare(init).span());
                }
                let init = self.alloc(init);
                self.values
                    .constants
                    .push((name, init, self.functions.last().copied()));
            }
        }
        walk::walk_variable_declaration(self, declaration);
    }

    fn visit_function(&mut self, function: &Function<'a>, flags: ScopeFlags) {
        if let Some(identifier) = &function.id {
            self.values
                .local_functions
                .insert(identifier.name.to_string(), function.span);
        }
        self.functions.push(function.span);
        walk::walk_function(self, function, flags);
        self.functions.pop();
    }

    fn visit_arrow_function_expression(&mut self, arrow: &ArrowFunctionExpression<'a>) {
        if arrow.expression
            && let [Statement::ExpressionStatement(body)] = arrow.body.statements.as_slice()
        {
            let body = self.alloc(&body.expression);
            self.values
                .returns
                .entry(key(arrow.span))
                .or_default()
                .push(body);
        }
        self.functions.push(arrow.span);
        walk::walk_arrow_function_expression(self, arrow);
        self.functions.pop();
    }

    fn visit_return_statement(&mut self, statement: &ReturnStatement<'a>) {
        if let (Some(argument), Some(function)) = (&statement.argument, self.functions.last()) {
            let argument = self.alloc(argument);
            self.values
                .returns
                .entry(key(*function))
                .or_default()
                .push(argument);
        }
        walk::walk_return_statement(self, statement);
    }
}

#[cfg(test)]
mod tests {
    use oxc_allocator::Allocator;
    use oxc_parser::Parser;
    use oxc_span::SourceType;

    use super::*;

    /// Each EqualityOperator mutant of `source` as `original → descriptor`,
    /// with the rule a static fact gives it.
    fn judged(
        source: &str,
        judge: fn(&StaticFacts, &LedgerMutant) -> Option<StaticNote>,
    ) -> Vec<(String, Option<String>)> {
        let allocator = Allocator::default();
        let parsed = Parser::new(&allocator, source, SourceType::tsx()).parse();
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let facts = StaticFacts::collect(&parsed.program);
        crate::js_mutants::enumerate(&parsed.program, source)
            .into_iter()
            .filter(|mutant| mutant.mutator == "EqualityOperator")
            .map(|mutant| {
                let original = &source[mutant.span.start as usize..mutant.span.end as usize];
                (
                    format!("{original} → {}", mutant.replacement),
                    judge(&facts, &mutant).map(|note| note.rule),
                )
            })
            .collect()
    }

    fn ruled(
        source: &str,
        judge: fn(&StaticFacts, &LedgerMutant) -> Option<StaticNote>,
    ) -> Vec<String> {
        judged(source, judge)
            .into_iter()
            .filter_map(|(mutant, rule)| rule.map(|rule| format!("{mutant}: {rule}")))
            .collect()
    }

    #[test]
    fn a_zero_boundary_after_a_truthy_number_const_is_equivalent() {
        let source = "export function f(a: number, b: number) {\n  const n = a - b;\n  const big = 2n * BigInt(a);\n  if (n && n > 0) return 1;\n  if (!n || 0 < n) return 2;\n  return (n && (b > 1 && n <= 0)) || (big && big >= 0n);\n}\n";
        assert_eq!(
            ruled(source, StaticFacts::equivalent),
            [
                "n > 0 → >=: zero-guarded",
                "0 < n → <=: zero-guarded",
                "n <= 0 → <: zero-guarded",
                "big >= 0n → >: zero-guarded",
            ]
        );
        // The other mutants of the same comparisons cross 0 the other way.
        let all = judged(source, StaticFacts::equivalent);
        assert!(
            all.iter()
                .any(|(mutant, rule)| mutant == "n > 0 → <=" && rule.is_none()),
            "{all:?}"
        );
    }

    #[test]
    fn a_zero_boundary_is_not_equivalent_where_a_truthy_value_can_compare_as_zero() {
        // A parameter, a member path, a `let`, a string-valued `+`, a call:
        // each can hold "0", " ", [] or a Date at the epoch, which are
        // truthy and compare as 0.
        let source = "export function f(a: number, o: { n: number }, s: string) {\n  let l = a - 1;\n  const t = s + 1;\n  const c = Number(s);\n  const d = a - 1;\n  return [\n    a && a > 0,\n    o.n && o.n > 0,\n    l && l > 0,\n    t && t > 0,\n    c && c > 0,\n    d || d > 0,\n    d && d > 1,\n    d && g(d) > 0,\n    (d, d > 0),\n    d ?? d > 0,\n  ];\n}\nfunction g(x: number) { return x; }\n";
        assert_eq!(ruled(source, StaticFacts::equivalent), Vec::<String>::new());
    }

    #[test]
    fn a_zero_guard_needs_the_one_binding_of_its_name_in_scope() {
        // A second binding of `n`, a reference outside the block of the
        // const, a parameter default that cannot see the body, and `eval`.
        let shadowed = "export function f(a: number) {\n  const n = a - 1;\n  return [n && n > 0, ((n: unknown) => n && n > 0)(\"0\")];\n}\n";
        assert_eq!(
            ruled(shadowed, StaticFacts::equivalent),
            Vec::<String>::new()
        );
        let outside = "export function f(a: number) {\n  {\n    const n = a - 1;\n  }\n  return n && n > 0;\n}\n";
        assert_eq!(
            ruled(outside, StaticFacts::equivalent),
            Vec::<String>::new()
        );
        let parameter = "export function f(a = n && n > 0) {\n  const n = 1 - 1;\n  return a;\n}\n";
        assert_eq!(
            ruled(parameter, StaticFacts::equivalent),
            Vec::<String>::new()
        );
        let evaluated = "export function f(a: number, code: string) {\n  eval(code);\n  const n = a - 1;\n  return n && n > 0;\n}\n";
        assert_eq!(
            ruled(evaluated, StaticFacts::equivalent),
            Vec::<String>::new()
        );
        let inside = "export function f(a: number) {\n  {\n    const n = a - 1;\n    return n && n > 0;\n  }\n}\n";
        assert_eq!(
            ruled(inside, StaticFacts::equivalent),
            ["n > 0 → >=: zero-guarded"]
        );
    }

    #[test]
    fn a_boundary_comparison_that_reads_the_clock_or_random_is_marked_untestable() {
        let source = "function elapsed(since: number) {\n  return performance.now() - since;\n}\nexport function f(start: number, until: Date, limit: number) {\n  const now = Date.now();\n  const age = now - start;\n  return [\n    Date.now() - start > 1000,\n    age >= limit,\n    until < new Date(),\n    elapsed(start) <= limit,\n    Math.random() < 0.5,\n    start > limit,\n    until < new Date(0),\n  ];\n}\n";
        assert_eq!(
            ruled(source, StaticFacts::untestable),
            [
                "Date.now() - start > 1000 → >=: clock",
                "age >= limit → >: clock",
                "until < new Date() → <=: clock",
                "elapsed(start) <= limit → <: clock",
                "Math.random() < 0.5 → <=: random",
            ]
        );
    }

    /// The shapes real clock boundaries take, and one that only
    /// depends on the clock: a count of what a clock-dependent `find` found.
    #[test]
    fn a_clock_hint_needs_a_time_valued_operand_not_one_worked_out_from_the_clock() {
        let source = "import { addMinutes } from 'date-fns';\nconst CRON_BUDGET_MS = 60000;\nexport function f(sub: { expires: string; trial: number }, deadline: number, expiry: number, params: { now?: Date }, items: { at: string }[]) {\n  const startedAt = Date.now();\n  const msRemaining = () => CRON_BUDGET_MS - (Date.now() - startedAt);\n  const tokenExpiresIn = expiry - Date.now();\n  const refreshBefore = addMinutes(new Date(), 5);\n  const now = params.now ?? new Date();\n  const expiresAt = new Date(sub.expires);\n  const groups = items.map((item) => ({ items: [item], isToday: new Date(item.at).getDate() === new Date().getDate() }));\n  const today = groups.find((g) => g.isToday);\n  const label = String(Date.now());\n  const run = async () => {\n    while (msRemaining() >= 1000) {}\n  };\n  return [\n    run,\n    tokenExpiresIn > 0,\n    expiresAt.getTime() < refreshBefore.getTime(),\n    sub.trial * 1000 <= now.getTime(),\n    expiresAt > now,\n    expiresAt > new Date(),\n    deadline - Date.now() < 0,\n    today && today.items.length > 0,\n    groups.length > 1,\n    label.length > 3,\n    -Date.now() < deadline,\n  ];\n}\n";
        assert_eq!(
            ruled(source, StaticFacts::untestable),
            [
                "msRemaining() >= 1000 → >: clock",
                "tokenExpiresIn > 0 → >=: clock",
                "expiresAt.getTime() < refreshBefore.getTime() → <=: clock",
                "sub.trial * 1000 <= now.getTime() → <: clock",
                "expiresAt > now → >=: clock",
                "expiresAt > new Date() → >=: clock",
                "deadline - Date.now() < 0 → <=: clock",
                "-Date.now() < deadline → <=: clock",
            ]
        );
    }

    #[test]
    fn a_clock_is_followed_through_two_consts_of_the_same_function_only() {
        // `later` names `now`, which reads the clock: two hops. `latest`
        // needs three; `boot` is not of a function around it.
        let source = "const boot = Date.now();\nexport function f(a: number, b: number) {\n  const now = Date.now();\n  const later = now + 1;\n  const latest = later + 1;\n  const beyond = latest + 1;\n  return [later > a, latest > a, beyond > a, boot > a, a > b];\n}\n";
        assert_eq!(
            ruled(source, StaticFacts::untestable),
            ["later > a → >=: clock"]
        );
    }
}
