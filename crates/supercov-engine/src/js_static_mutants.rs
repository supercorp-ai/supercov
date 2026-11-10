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

use std::collections::{HashMap, HashSet};

use oxc_ast::AstKind;
use oxc_ast::ast::{
    ArrowFunctionExpression, BinaryExpression, CallExpression, Expression, Function,
    LogicalExpression, NewExpression, Program, VariableDeclaration, VariableDeclarationKind,
    WithStatement,
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

/// A relational comparison: its operator, operands and innermost function.
#[derive(Debug)]
struct Comparison {
    operator: &'static str,
    left: Span,
    right: Span,
    function: Option<Span>,
}

/// A comparison of an identifier with the literal 0 inside a chain whose
/// earlier operands found that identifier truthy.
#[derive(Debug)]
struct GuardedZero {
    name: String,
    /// `&&` for `x && … x > 0`, `||` for `!x || … x > 0`.
    guard: &'static str,
}

/// A `const` with an identifier binding and an initializer.
#[derive(Debug)]
struct Constant {
    name: String,
    init: Span,
    function: Option<Span>,
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
    constants: Vec<Constant>,
    /// Functions a name declares: a function declaration's, or a `const`'s
    /// arrow or function expression.
    local_functions: HashMap<String, Span>,
    clock_reads: Vec<Span>,
    random_reads: Vec<Span>,
    /// Identifiers read, by position.
    references: Vec<(Span, String)>,
    /// Identifiers called (`f(...)`), by position.
    callees: Vec<(Span, String)>,
}

struct Collector {
    facts: StaticFacts,
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
            nodes: Vec::new(),
            scopes: Vec::new(),
            functions: Vec::new(),
            truthy: Vec::new(),
        };
        collector.visit_program(program);
        let mut facts = collector.facts;
        facts.references.sort_by_key(|(span, _)| span.start);
        facts.callees.sort_by_key(|(span, _)| span.start);
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
        let reads = |reads: &[Span]| {
            [comparison.left, comparison.right]
                .into_iter()
                .any(|operand| self.reads(operand, reads, comparison.function, 2))
        };
        if reads(&self.clock_reads) {
            Some(StaticNote {
                rule: CLOCK.to_owned(),
                reason: "an operand reads the clock: they differ only on one exact millisecond"
                    .to_owned(),
            })
        } else if reads(&self.random_reads) {
            Some(StaticNote {
                rule: RANDOM.to_owned(),
                reason:
                    "an operand reads Math.random(): they differ only at one exact random value"
                        .to_owned(),
            })
        } else {
            None
        }
    }

    /// Whether the code at `span` reads one of `reads`: itself, through the
    /// initializer of a `const` of `function` it names (up to `hops` of
    /// them), or in the body of a local function it calls.
    fn reads(&self, span: Span, reads: &[Span], function: Option<Span>, hops: usize) -> bool {
        if reads.iter().any(|read| within(*read, span)) {
            return true;
        }
        let called = in_span(&self.callees, span).any(|name| {
            self.local_functions
                .get(name)
                .is_some_and(|body| reads.iter().any(|read| within(*read, *body)))
        });
        if called {
            return true;
        }
        if hops == 0 {
            return false;
        }
        let named = in_span(&self.references, span).collect::<HashSet<_>>();
        self.constants.iter().any(|constant| {
            constant.function == function
                && named.contains(constant.name.as_str())
                && !within(constant.init, span)
                && self.reads(constant.init, reads, function, hops - 1)
        })
    }
}

/// The names at positions within `span`.
fn in_span(names: &[(Span, String)], span: Span) -> impl Iterator<Item = &str> {
    let first = names.partition_point(|(at, _)| at.start < span.start);
    names[first..]
        .iter()
        .take_while(move |(at, _)| at.start < span.end)
        .filter(move |(at, _)| at.end <= span.end)
        .map(|(_, name)| name.as_str())
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

impl Collector {
    fn comparison(&mut self, binary: &BinaryExpression<'_>) {
        if !relational(binary.operator) {
            return;
        }
        self.facts.comparisons.insert(
            key(binary.span),
            Comparison {
                operator: binary.operator.as_str(),
                left: binary.left.span(),
                right: binary.right.span(),
                function: self.functions.last().copied(),
            },
        );
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

impl<'a> Visit<'a> for Collector {
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
        self.facts
            .references
            .push((identifier.span, identifier.name.to_string()));
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
                    self.facts.local_functions.insert(name.clone(), init.span());
                }
                self.facts.constants.push(Constant {
                    name,
                    init: init.span(),
                    function: self.functions.last().copied(),
                });
            }
        }
        walk::walk_variable_declaration(self, declaration);
    }

    fn visit_function(&mut self, function: &Function<'a>, flags: ScopeFlags) {
        if let (Some(identifier), Some(body)) = (&function.id, &function.body) {
            self.facts
                .local_functions
                .insert(identifier.name.to_string(), body.span);
        }
        self.functions.push(function.span);
        walk::walk_function(self, function, flags);
        self.functions.pop();
    }

    fn visit_arrow_function_expression(&mut self, arrow: &ArrowFunctionExpression<'a>) {
        self.functions.push(arrow.span);
        walk::walk_arrow_function_expression(self, arrow);
        self.functions.pop();
    }

    fn visit_call_expression(&mut self, call: &CallExpression<'a>) {
        match static_call(call) {
            Some(("Date" | "performance", "now")) => self.facts.clock_reads.push(call.span),
            Some(("Math", "random")) => self.facts.random_reads.push(call.span),
            _ => {}
        }
        if let Expression::Identifier(callee) = bare(&call.callee) {
            self.facts
                .callees
                .push((callee.span, callee.name.to_string()));
        }
        walk::walk_call_expression(self, call);
    }

    fn visit_new_expression(&mut self, new: &NewExpression<'a>) {
        if new.arguments.is_empty()
            && matches!(bare(&new.callee), Expression::Identifier(callee) if callee.name == "Date")
        {
            self.facts.clock_reads.push(new.span);
        }
        walk::walk_new_expression(self, new);
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

    #[test]
    fn a_clock_is_followed_through_two_consts_of_the_same_function_only() {
        // `later` names `now`, which reads the clock: two hops. `latest`
        // needs three; `boot` is not of the function.
        let source = "const boot = Date.now();\nexport function f(a: number, b: number) {\n  const now = Date.now();\n  const later = now + 1;\n  const latest = later + 1;\n  const beyond = latest + 1;\n  return [later > a, latest > a, beyond > a, boot > a, a > b];\n}\n";
        assert_eq!(
            ruled(source, StaticFacts::untestable),
            ["later > a → >=: clock"]
        );
    }
}
