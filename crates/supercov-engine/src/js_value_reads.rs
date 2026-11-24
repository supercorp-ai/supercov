//! Who reads a module-level value (`docs/design/strong-mutation.md`,
//! "Module-level values").
//!
//! A mutant in code that runs as its module loads changes a value once, and
//! the module keeps that value for every later use: with nothing more known,
//! every test that loaded the module may observe it. But where the mutant is
//! inside an object or array literal that a top-level declaration makes, and
//! nothing in the literal's evaluation can carry the changed value anywhere
//! else, the value is reachable only through that literal. Such a literal is
//! wrapped (`trackV2`) so that each read of it is recorded: a hit of a point
//! for any read, and of one for each key a mutant is under, credited to the
//! test whose work read it. A read made while a module loads hits the
//! literal's escape points instead: what it took outlives it.
//!
//! This module finds the literals, decides which mutants are linked to which
//! of their points, and wraps the literals in the instrumented copy. It runs
//! only where the mutants' probes are placed: a run measured without them
//! recorded no read, and its mutants have no link.

use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};

use oxc_allocator::TakeIn;
use oxc_ast::{
    AstBuilder, NONE,
    ast::{
        Argument, ArrayExpressionElement, AwaitExpression, Declaration, Expression, ForOfStatement,
        Function, ObjectPropertyKind, Program, PropertyKey, PropertyKind, Statement,
        VariableDeclarationKind,
    },
};
use oxc_ast_visit::{Visit, VisitMut, walk, walk_mut};
use oxc_span::{GetSpan, Span};
use oxc_syntax::{
    number::NumberBase,
    operator::{BinaryOperator, UnaryOperator},
    scope::ScopeFlags,
};

use crate::js_mutants::LedgerMutant;
use crate::mutant_list::ReadLink;

type SpanKey = (u32, u32);

fn key(span: Span) -> SpanKey {
    (span.start, span.end)
}

fn contains(outer: Span, inner: Span) -> bool {
    outer.start <= inner.start && inner.end <= outer.end
}

/// The prefix of a read point's id: no statement, function or branch id has
/// it, so the report can tell these hits from coverage's.
pub(crate) const READ_POINT_PREFIX: &str = "read:";

/// Whether a hit's id is a read point's.
pub(crate) fn is_read_point(id: &str) -> bool {
    id.starts_with(READ_POINT_PREFIX)
}

/// The literals a file's module-level mutants are read through, and the
/// mutants' links to their points.
#[derive(Debug, Default)]
pub(crate) struct ValueReads {
    /// Each literal to wrap, by its span: the place of its first point among
    /// the reads' own, and the keys it has points for.
    pub nodes: HashMap<SpanKey, (usize, Vec<String>)>,
    /// Each linked mutant's points, by the mutated span.
    pub links: HashMap<SpanKey, ReadLink>,
    /// The reads' points, in place order.
    pub ids: Vec<String>,
}

/// A literal of a module's top level, and its slots.
struct Node<'p, 'a> {
    /// The literal itself.
    expression: &'p Expression<'a>,
    span: Span,
    slots: Vec<Slot<'p, 'a>>,
}

/// One property or element of a literal.
struct Slot<'p, 'a> {
    /// The key's span, where it is written (a mutant there is not linked).
    key_span: Option<Span>,
    /// The key a read names it by, where the source says it alone.
    key: Option<String>,
    value: &'p Expression<'a>,
    /// The literal it holds, as a node of its own.
    child: Option<usize>,
}

/// The literal `expression` is, under parentheses and type syntax.
fn peeled<'p, 'a>(expression: &'p Expression<'a>) -> &'p Expression<'a> {
    match expression {
        Expression::ParenthesizedExpression(inner) => peeled(&inner.expression),
        Expression::TSAsExpression(inner) => peeled(&inner.expression),
        Expression::TSSatisfiesExpression(inner) => peeled(&inner.expression),
        Expression::TSNonNullExpression(inner) => peeled(&inner.expression),
        Expression::TSTypeAssertion(inner) => peeled(&inner.expression),
        _ => expression,
    }
}

/// An object or array literal, as `add` takes one.
enum Literal<'p, 'a> {
    Object(&'p oxc_ast::ast::ObjectExpression<'a>),
    Array(&'p oxc_ast::ast::ArrayExpression<'a>),
}

/// The literal an expression is, through parentheses and type assertions,
/// with the expression it is; `None` where it is no object or array literal.
fn literal_of<'p, 'a>(
    expression: &'p Expression<'a>,
) -> Option<(&'p Expression<'a>, Literal<'p, 'a>)> {
    let peeled = peeled(expression);
    match peeled {
        Expression::ObjectExpression(object) => Some((peeled, Literal::Object(object))),
        Expression::ArrayExpression(array) => Some((peeled, Literal::Array(array))),
        _ => None,
    }
}

/// A number key as JavaScript names it, where that is plain: a whole number
/// below 10^21, written in digits alone.
fn number_key(value: f64) -> Option<String> {
    (value.fract() == 0.0 && value < 1e21).then(|| format!("{value:.0}"))
}

/// The key an object property is read by, where the source says it alone.
fn property_key(key: &PropertyKey<'_>) -> Option<String> {
    match key {
        PropertyKey::StaticIdentifier(name) => Some(name.name.to_string()),
        PropertyKey::StringLiteral(literal) => Some(literal.value.to_string()),
        PropertyKey::NumericLiteral(literal) => number_key(literal.value),
        _ => None,
    }
}

/// What an expression's evaluation can do, as far as this module allows.
#[derive(Default)]
struct Inert {
    /// It reads a name.
    names: bool,
    /// It applies an operator, or converts a value to a string.
    operators: bool,
    /// It does something else: a call, a member read, an assignment, a
    /// spread...
    other: bool,
}

impl Inert {
    /// Whether a literal can be left unevaluated (replaced whole by `{}` or
    /// `[]`) changing nothing but its value: what it holds is literals of
    /// the same kind, or values each `of` allows. Nothing new is evaluated,
    /// so a name and an operator in different values do no harm.
    fn droppable(expression: &Expression<'_>) -> bool {
        match peeled(expression) {
            Expression::ObjectExpression(object) => {
                object.properties.iter().all(|property| match property {
                    ObjectPropertyKind::ObjectProperty(property) => {
                        (!property.computed || property_key(&property.key).is_some())
                            && Self::droppable(&property.value)
                    }
                    ObjectPropertyKind::SpreadProperty(_) => false,
                })
            }
            Expression::ArrayExpression(array) => {
                array.elements.iter().all(|element| match element {
                    ArrayExpressionElement::SpreadElement(_) => false,
                    ArrayExpressionElement::Elision(_) => true,
                    element => Self::droppable(element.to_expression()),
                })
            }
            expression => Self::of(expression),
        }
    }

    /// Whether evaluating the expression, or skipping it, or evaluating
    /// another part of it than before, can do nothing but make its value:
    /// it runs no code of the program's, writes nothing, and cannot throw.
    /// Literals, literals of literals, functions (made, not run), and
    /// operators over literals. A name is read alone, or with no operator:
    /// an operator over a name's value can run its `valueOf` or throw.
    fn of(expression: &Expression<'_>) -> bool {
        let mut inert = Inert::default();
        inert.expression(expression);
        !(inert.other || inert.names && inert.operators)
    }

    fn expression(&mut self, expression: &Expression<'_>) {
        match expression {
            Expression::StringLiteral(_)
            | Expression::NumericLiteral(_)
            | Expression::BigIntLiteral(_)
            | Expression::BooleanLiteral(_)
            | Expression::NullLiteral(_)
            | Expression::RegExpLiteral(_)
            | Expression::FunctionExpression(_)
            | Expression::ArrowFunctionExpression(_) => {}
            Expression::Identifier(_) => self.names = true,
            Expression::TemplateLiteral(template) => {
                if !template.expressions.is_empty() {
                    self.operators = true;
                }
                template
                    .expressions
                    .iter()
                    .for_each(|part| self.expression(part));
            }
            Expression::ObjectExpression(object) => {
                for property in &object.properties {
                    match property {
                        ObjectPropertyKind::ObjectProperty(property) => {
                            if property.computed && property_key(&property.key).is_none() {
                                self.other = true;
                            }
                            self.expression(&property.value);
                        }
                        ObjectPropertyKind::SpreadProperty(_) => self.other = true,
                    }
                }
            }
            Expression::ArrayExpression(array) => {
                for element in &array.elements {
                    match element {
                        ArrayExpressionElement::SpreadElement(_) => self.other = true,
                        ArrayExpressionElement::Elision(_) => {}
                        element => self.expression(element.to_expression()),
                    }
                }
            }
            Expression::UnaryExpression(unary) if unary.operator != UnaryOperator::Delete => {
                self.operators = true;
                self.expression(&unary.argument);
            }
            Expression::BinaryExpression(binary)
                if !matches!(
                    binary.operator,
                    BinaryOperator::In | BinaryOperator::Instanceof
                ) =>
            {
                self.operators = true;
                self.expression(&binary.left);
                self.expression(&binary.right);
            }
            Expression::LogicalExpression(logical) => {
                self.operators = true;
                self.expression(&logical.left);
                self.expression(&logical.right);
            }
            Expression::ConditionalExpression(conditional) => {
                self.operators = true;
                self.expression(&conditional.test);
                self.expression(&conditional.consequent);
                self.expression(&conditional.alternate);
            }
            Expression::SequenceExpression(sequence) => {
                self.operators = true;
                sequence
                    .expressions
                    .iter()
                    .for_each(|part| self.expression(part));
            }
            Expression::ParenthesizedExpression(inner) => self.expression(&inner.expression),
            Expression::TSAsExpression(inner) => self.expression(&inner.expression),
            Expression::TSSatisfiesExpression(inner) => self.expression(&inner.expression),
            Expression::TSNonNullExpression(inner) => self.expression(&inner.expression),
            Expression::TSTypeAssertion(inner) => self.expression(&inner.expression),
            Expression::TSInstantiationExpression(inner) => self.expression(&inner.expression),
            _ => self.other = true,
        }
    }
}

/// The spans of the functions in an expression: code made there, run later.
#[derive(Default)]
struct Functions {
    spans: Vec<Span>,
}

impl<'a> Visit<'a> for Functions {
    fn visit_function(&mut self, function: &Function<'a>, _: ScopeFlags) {
        self.spans.push(function.span);
    }

    fn visit_arrow_function_expression(
        &mut self,
        arrow: &oxc_ast::ast::ArrowFunctionExpression<'a>,
    ) {
        self.spans.push(arrow.span);
    }

    fn visit_class(&mut self, class: &oxc_ast::ast::Class<'a>) {
        self.spans.push(class.span);
    }
}

fn in_function(expression: &Expression<'_>, span: Span) -> bool {
    let mut functions = Functions::default();
    functions.visit_expression(expression);
    functions
        .spans
        .iter()
        .any(|function| contains(*function, span))
}

/// The literals of `program`'s top-level declarations, outermost first, and
/// the places of the declarations' own.
fn literals<'p, 'a>(program: &'p Program<'a>, file: &str) -> (Vec<Node<'p, 'a>>, Vec<usize>) {
    let static_exports = crate::js_schemata::next_static_exports(file);
    let mut nodes = Vec::new();
    let mut roots = Vec::new();
    for statement in &program.body {
        let (declaration, exported) = match statement {
            Statement::VariableDeclaration(declaration) => (declaration, false),
            Statement::ExportNamedDeclaration(export) => match &export.declaration {
                Some(Declaration::VariableDeclaration(declaration)) => (declaration, true),
                _ => continue,
            },
            _ => continue,
        };
        // `using` disposes of its value as the module ends.
        if !matches!(
            declaration.kind,
            VariableDeclarationKind::Const
                | VariableDeclarationKind::Let
                | VariableDeclarationKind::Var
        ) {
            continue;
        }
        for declarator in &declaration.declarations {
            // A value a framework reads from the source stays as written.
            if exported
                && declarator
                    .id
                    .get_binding_identifier()
                    .is_some_and(|name| static_exports.contains(&name.name.as_str()))
            {
                continue;
            }
            if let Some((expression, literal)) = declarator.init.as_ref().and_then(literal_of) {
                roots.push(add(&mut nodes, expression, literal));
            }
        }
    }
    (nodes, roots)
}

/// Adds `literal` and the literals it holds, and returns its place.
fn add<'p, 'a>(
    nodes: &mut Vec<Node<'p, 'a>>,
    expression: &'p Expression<'a>,
    literal: Literal<'p, 'a>,
) -> usize {
    let at = nodes.len();
    nodes.push(Node {
        expression,
        span: expression.span(),
        slots: Vec::new(),
    });
    let mut slots = Vec::new();
    match literal {
        Literal::Object(object) => {
            for property in &object.properties {
                let ObjectPropertyKind::ObjectProperty(property) = property else {
                    continue;
                };
                // `__proto__: x` sets the prototype: what it holds is read by
                // keys of its own, through the literal.
                let prototype = !property.computed
                    && !property.shorthand
                    && !property.method
                    && property.kind == PropertyKind::Init
                    && property_key(&property.key).as_deref() == Some("__proto__");
                slots.push(Slot {
                    key_span: Some(property.key.span()),
                    key: (!prototype).then(|| property_key(&property.key)).flatten(),
                    value: &property.value,
                    child: None,
                });
            }
        }
        Literal::Array(array) => {
            let mut index = Some(0usize);
            for element in &array.elements {
                match element {
                    ArrayExpressionElement::SpreadElement(_) => index = None,
                    ArrayExpressionElement::Elision(_) => index = index.map(|at| at + 1),
                    element => {
                        slots.push(Slot {
                            key_span: None,
                            key: index.map(|at| at.to_string()),
                            value: element.to_expression(),
                            child: None,
                        });
                        index = index.map(|at| at + 1);
                    }
                }
            }
        }
    }
    for slot in &mut slots {
        if let Some((expression, literal)) = literal_of(slot.value) {
            slot.child = Some(add(nodes, expression, literal));
        }
    }
    nodes[at].slots = slots;
    at
}

/// Where a mutant at `span` is read: the node, and its key where the mutant
/// is under one; `None` where it is not linked.
fn place(nodes: &[Node<'_, '_>], at: usize, span: Span) -> Option<(usize, Option<String>)> {
    let node = &nodes[at];
    if node.span == span {
        // The literal itself replaced (`{}`, `[]`): what it held is no longer
        // evaluated, which must change nothing but the value.
        return Inert::droppable(node.expression).then_some((at, None));
    }
    for slot in &node.slots {
        if slot.key_span.is_some_and(|key| contains(key, span)) {
            return None;
        }
        if !contains(slot.value.span(), span) {
            continue;
        }
        // The literal a slot holds is its value, under type syntax alone.
        if let Some(child) = slot.child {
            return place(nodes, child, span);
        }
        if in_function(slot.value, span) || !Inert::of(slot.value) {
            return None;
        }
        return Some((at, slot.key.clone()));
    }
    None
}

/// The literals `mutants` are read through, in `program` as written.
pub(crate) fn analyze(
    program: &Program<'_>,
    source: &str,
    file: &str,
    mutants: &[LedgerMutant],
) -> ValueReads {
    let (nodes, roots) = literals(program, file);
    // Each mutant's node and key.
    let mut placed = Vec::new();
    for mutant in mutants {
        let Some(root) = roots
            .iter()
            .find(|root| contains(nodes[**root].span, mutant.span))
        else {
            continue;
        };
        if let Some((node, read_key)) = place(&nodes, *root, mutant.span) {
            placed.push((mutant, node, read_key));
        }
    }
    // Each node's keys, in the order of its slots.
    let mut keys = HashMap::<usize, Vec<String>>::default();
    for (_, node, read_key) in &placed {
        let used = keys.entry(*node).or_default();
        if let Some(read_key) = read_key
            && !used.contains(read_key)
        {
            used.push(read_key.clone());
        }
    }
    let mut tracked = keys.keys().copied().collect::<Vec<_>>();
    tracked.sort_by_key(|at| (nodes[*at].span.start, nodes[*at].span.end));
    let mut reads = ValueReads::default();
    let mut points = HashMap::<(usize, Option<String>), (String, String)>::default();
    let id = |span: Span, suffix: &str| {
        format!(
            "{READ_POINT_PREFIX}{}",
            crate::js_instrumenter::stable_id(source, file, "value-read", span, suffix)
        )
    };
    for at in &tracked {
        let span = nodes[*at].span;
        let mut node_keys = keys[at].clone();
        let order = nodes[*at]
            .slots
            .iter()
            .filter_map(|slot| slot.key.clone())
            .collect::<Vec<_>>();
        node_keys.sort_by_key(|read_key| order.iter().position(|known| known == read_key));
        let base = reads.ids.len();
        let any = (id(span, "any"), id(span, "any-loading"));
        reads.ids.push(any.0.clone());
        reads.ids.push(any.1.clone());
        points.insert((*at, None), any);
        for read_key in &node_keys {
            let pair = (
                id(span, &format!("key:{read_key}")),
                id(span, &format!("key-loading:{read_key}")),
            );
            reads.ids.push(pair.0.clone());
            reads.ids.push(pair.1.clone());
            points.insert((*at, Some(read_key.clone())), pair);
        }
        reads.nodes.insert(key(span), (base, node_keys));
    }
    for (mutant, node, read_key) in placed {
        let (read, escape) = points[&(node, read_key)].clone();
        reads
            .links
            .insert(key(mutant.span), ReadLink { read, escape });
    }
    reads
}

/// Wraps each tracked literal in `trackV2(file, base, keys, literal)`, after
/// every other pass: whatever they placed inside it stays inside it.
pub(crate) struct ReadTransformer<'a, 'n> {
    pub ast: AstBuilder<'a>,
    /// The file's probe binding (`__supercovFile`).
    pub file: String,
    /// The helper's local name, named where a literal or an await is.
    pub helper: &'n mut dyn FnMut(&'static str) -> String,
    /// Where the reads' points start among the file's.
    pub base: usize,
    pub nodes: &'n HashMap<SpanKey, (usize, Vec<String>)>,
    pub done: HashSet<SpanKey>,
}

impl<'a> ReadTransformer<'a, '_> {
    fn number(&self, value: usize) -> Argument<'a> {
        Argument::from(self.ast.expression_numeric_literal(
            Span::default(),
            value as f64,
            None,
            NumberBase::Decimal,
        ))
    }
}

impl<'a> VisitMut<'a> for ReadTransformer<'a, '_> {
    fn visit_expression(&mut self, expression: &mut Expression<'a>) {
        let literal = peeled(expression);
        let span = key(literal.span());
        let tracked = matches!(
            literal,
            Expression::ObjectExpression(_) | Expression::ArrayExpression(_)
        ) && self.nodes.contains_key(&span)
            && self.done.insert(span);
        walk_mut::walk_expression(self, expression);
        if !tracked {
            return;
        }
        let (first, keys) = &self.nodes[&span];
        let keys = self.ast.expression_array(
            Span::default(),
            self.ast.vec_from_iter(keys.iter().map(|read_key| {
                ArrayExpressionElement::from(self.ast.expression_string_literal(
                    Span::default(),
                    self.ast.str(read_key),
                    None,
                ))
            })),
        );
        let base = self.number(self.base + first);
        let value = expression.take_in(self.ast.allocator);
        let helper = (self.helper)("trackV2");
        *expression = self.ast.expression_call(
            Span::default(),
            self.ast
                .expression_identifier(Span::default(), self.ast.ident(&helper)),
            NONE,
            self.ast.vec_from_array([
                Argument::from(
                    self.ast
                        .expression_identifier(Span::default(), self.ast.ident(&self.file)),
                ),
                base,
                Argument::from(keys),
                Argument::from(value),
            ]),
            false,
        );
    }
}

/// Whether `program`'s top level awaits: there it gives up the thread, and
/// code after the await runs once a module that was loading may be done.
#[derive(Default)]
struct TopLevelAwaits {
    found: bool,
}

impl<'a> Visit<'a> for TopLevelAwaits {
    fn visit_function(&mut self, _: &Function<'a>, _: ScopeFlags) {}
    fn visit_arrow_function_expression(&mut self, _: &oxc_ast::ast::ArrowFunctionExpression<'a>) {}
    fn visit_class(&mut self, _: &oxc_ast::ast::Class<'a>) {}
    fn visit_await_expression(&mut self, _: &AwaitExpression<'a>) {
        self.found = true;
    }
    fn visit_for_of_statement(&mut self, statement: &ForOfStatement<'a>) {
        self.found |= statement.r#await;
        walk::walk_for_of_statement(self, statement);
    }
}

pub(crate) fn awaits_at_top_level(program: &Program<'_>) -> bool {
    let mut awaits = TopLevelAwaits::default();
    awaits.visit_program(program);
    awaits.found
}

/// Marks the module loading again where its top level resumes after an
/// await: `resumeV2(file, await x)`, and `resumeV2(file)` first in a
/// `for await` body.
pub(crate) struct ResumeTransformer<'a, 'n> {
    pub ast: AstBuilder<'a>,
    /// The file's probe binding (`__supercovFile`).
    pub file: String,
    pub helper: &'n mut dyn FnMut(&'static str) -> String,
}

impl<'a> ResumeTransformer<'a, '_> {
    fn call(&mut self, value: Option<Expression<'a>>) -> Expression<'a> {
        let helper = (self.helper)("resumeV2");
        let file = Argument::from(
            self.ast
                .expression_identifier(Span::default(), self.ast.ident(&self.file)),
        );
        self.ast.expression_call(
            Span::default(),
            self.ast
                .expression_identifier(Span::default(), self.ast.ident(&helper)),
            NONE,
            self.ast
                .vec_from_iter(std::iter::once(file).chain(value.map(Argument::from))),
            false,
        )
    }
}

impl<'a> VisitMut<'a> for ResumeTransformer<'a, '_> {
    fn visit_function(&mut self, _: &mut Function<'a>, _: ScopeFlags) {}
    fn visit_arrow_function_expression(
        &mut self,
        _: &mut oxc_ast::ast::ArrowFunctionExpression<'a>,
    ) {
    }
    fn visit_class(&mut self, _: &mut oxc_ast::ast::Class<'a>) {}

    fn visit_expression(&mut self, expression: &mut Expression<'a>) {
        walk_mut::walk_expression(self, expression);
        if matches!(expression, Expression::AwaitExpression(_)) {
            let awaited = expression.take_in(self.ast.allocator);
            *expression = self.call(Some(awaited));
        }
    }

    fn visit_for_of_statement(&mut self, statement: &mut oxc_ast::ast::ForOfStatement<'a>) {
        walk_mut::walk_for_of_statement(self, statement);
        if !statement.r#await {
            return;
        }
        let mark = self
            .ast
            .statement_expression(Span::default(), self.call(None));
        let body = statement.body.take_in(self.ast.allocator);
        statement.body = Statement::BlockStatement(
            self.ast
                .alloc_block_statement(Span::default(), self.ast.vec_from_array([mark, body])),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::js_instrumenter::{
        CandidateOutput, MUTANTS_FOR_TEST, PROBES_FOR_TEST, instrument_candidate,
        project_source_type,
    };
    use crate::mutant_list::CandidateMutant;
    use oxc_allocator::Allocator;
    use oxc_parser::Parser;

    /// `source` instrumented as `file` with the mutants listed, with or
    /// without their probes; its output parses.
    fn instrumented(source: &str, file: &str, probes: bool) -> CandidateOutput {
        MUTANTS_FOR_TEST.with(|flag| flag.set(Some(true)));
        PROBES_FOR_TEST.with(|flag| flag.set(probes));
        let output = instrument_candidate(source, file);
        MUTANTS_FOR_TEST.with(|flag| flag.set(None));
        PROBES_FOR_TEST.with(|flag| flag.set(true));
        let output = output.unwrap();
        let allocator = Allocator::default();
        let source_type = project_source_type(std::path::Path::new(file)).unwrap();
        let parsed = Parser::new(&allocator, &output.code, source_type).parse();
        assert!(
            parsed.errors.is_empty(),
            "{:?}\n{}",
            parsed.errors,
            output.code
        );
        output
    }

    fn mutant<'o>(
        output: &'o CandidateOutput,
        original: &str,
        mutator: &str,
    ) -> &'o CandidateMutant {
        output
            .mutants
            .iter()
            .find(|mutant| mutant.original == original && mutant.mutator == mutator)
            .unwrap_or_else(|| panic!("{mutator} `{original}`: {:#?}", output.mutants))
    }

    /// Each `trackV2(file, base, [keys], ...)` the code makes: the point ids
    /// from its base, any read's first, then each key's read.
    fn tracked(output: &CandidateOutput) -> Vec<(String, Vec<String>)> {
        let points = &output.probe_table.as_ref().unwrap().points;
        output
            .code
            .split("__supercovTrackV2")
            .skip(1)
            .filter_map(|call| {
                call.trim_start_matches(|c: char| c.is_ascii_digit())
                    .strip_prefix('(')
            })
            .map(|arguments| {
                let mut parts = arguments.splitn(3, ", ");
                parts.next();
                let base = parts.next().unwrap().parse::<usize>().unwrap();
                let keys = parts.next().unwrap();
                let keys = keys[1..keys.find(']').unwrap()]
                    .split(',')
                    .map(|key| key.trim().trim_matches('"').to_owned())
                    .filter(|key| !key.is_empty())
                    .collect::<Vec<_>>();
                assert!(
                    points[base + 1..base + 2 + 2 * keys.len()]
                        .iter()
                        .all(|id| is_read_point(id))
                );
                (points[base].clone(), keys)
            })
            .collect()
    }

    /// The first declaration's value in `source` (TypeScript), given to
    /// `check`.
    fn with_value<R>(source: &str, check: impl FnOnce(&Expression<'_>) -> R) -> R {
        let allocator = Allocator::default();
        let parsed = Parser::new(&allocator, source, oxc_span::SourceType::ts()).parse();
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let Statement::VariableDeclaration(declaration) = &parsed.program.body[0] else {
            panic!("{source} declares nothing first");
        };
        check(declaration.declarations[0].init.as_ref().unwrap())
    }

    /// What a value may do when evaluated, or left out: literals, a key
    /// known before it is evaluated, holes, parentheses and type assertions
    /// do nothing but make it; a key worked out from a name, or a spread,
    /// runs the program's code. A literal is dropped whole only where it
    /// holds no such part.
    #[test]
    fn a_value_s_parts_say_whether_it_runs_code() {
        for (source, inert, droppable) in [
            ("const a = {};", true, true),
            ("const a = [];", true, true),
            ("const a = { [\"key\"]: 1 };", true, true),
            ("const a = { [key]: 1 };", false, false),
            ("const a = { ...rest };", false, false),
            ("const a = [...rest];", false, false),
            ("const a = [, 1];", true, true),
            ("const a = (1);", true, true),
            ("const a = 1 as number;", true, true),
            ("const a = 1 satisfies number;", true, true),
        ] {
            with_value(source, |value| {
                assert_eq!(Inert::of(value), inert, "{source}");
                assert_eq!(Inert::droppable(value), droppable, "{source}");
            });
        }
    }

    /// The literals of a module's top-level declarations, each with its
    /// slots: an empty literal holds none, and a declaration with no
    /// declarator (none the parser makes, but the walk does not assume
    /// one) adds none.
    #[test]
    fn the_top_level_literals_are_each_declaration_s() {
        let allocator = Allocator::default();
        let source = "const empty = {}, list = [1, { inner: 2 }];\nlet none = 1;";
        let mut parsed = Parser::new(&allocator, source, oxc_span::SourceType::mjs()).parse();
        assert!(parsed.errors.is_empty());
        let (nodes, roots) = literals(&parsed.program, "src/a.js");
        assert_eq!(roots.len(), 2);
        assert!(nodes[roots[0]].slots.is_empty());
        let list = &nodes[roots[1]];
        assert_eq!(
            list.slots
                .iter()
                .map(|slot| slot.key.clone())
                .collect::<Vec<_>>(),
            [Some("0".to_owned()), Some("1".to_owned())]
        );
        assert!(list.slots[1].child.is_some());
        let Statement::VariableDeclaration(declaration) = &mut parsed.program.body[0] else {
            unreachable!("the source's first statement is a declaration");
        };
        declaration.declarations.clear();
        let (nodes, roots) = literals(&parsed.program, "src/a.js");
        assert!(nodes.is_empty() && roots.is_empty());
    }

    /// A table's mutants are read through its literals: each string under
    /// the key it is read by, each literal replaced whole as any read of it.
    /// The literals are wrapped with the points the mutants are linked to,
    /// after the coverage points, which keep their places.
    #[test]
    fn a_table_s_mutants_are_linked_to_the_reads_of_their_places() {
        let source = "export const WORDS: Record<string, { hi: string; bye: string }> = {\n  en: { hi: 'Hello', bye: 'Bye' },\n  'es': { hi: 'Hola', bye: `Adios` },\n};\nexport function words(lang: string) {\n  return WORDS[lang] ?? WORDS['en'];\n}\n";
        let output = instrumented(source, "src/words.ts", true);
        let without = instrumented(source, "src/words.ts", false);
        assert!(!without.code.contains("TrackV2"), "{}", without.code);
        assert!(without.mutants.iter().all(|mutant| mutant.reads.is_none()));
        let table = output.probe_table.as_ref().unwrap();
        let plain = without.probe_table.as_ref().unwrap();
        assert_eq!(table.points[..plain.points.len()], plain.points[..]);
        let calls = tracked(&output);
        assert_eq!(calls.len(), 3, "{}", output.code);
        let read = |original: &str, mutator: &str| {
            mutant(&output, original, mutator)
                .reads
                .clone()
                .unwrap_or_else(|| panic!("`{original}` has no reads"))
        };
        // The root replaced whole: any read of it.
        let root = read(
            "{\n  en: { hi: 'Hello', bye: 'Bye' },\n  'es': { hi: 'Hola', bye: `Adios` },\n}",
            "ObjectLiteral",
        );
        assert_eq!(calls[0], (root.read.clone(), Vec::<String>::new()));
        assert!(is_read_point(&root.escape) && root.escape != root.read);
        // A string, under its key; a template too.
        let hello = read("'Hello'", "StringLiteral");
        let adios = read("`Adios`", "StringLiteral");
        assert_eq!(calls[1].1, ["hi", "bye"]);
        assert_eq!(calls[2].1, ["hi", "bye"]);
        let points = &table.points;
        let place = |id: &str| points.iter().position(|point| point == id).unwrap();
        // A node's points: any, any while loading, then each key's two.
        assert_eq!(place(&hello.read), place(&calls[1].0) + 2);
        assert_eq!(place(&hello.escape), place(&calls[1].0) + 3);
        assert_eq!(place(&adios.read), place(&calls[2].0) + 4);
        assert_eq!(place(&adios.escape), place(&calls[2].0) + 5);
        // A nested literal replaced whole: any read of it.
        assert_eq!(
            read("{ hi: 'Hola', bye: `Adios` }", "ObjectLiteral").read,
            calls[2].0
        );
        // Code in a function is not module-level.
        assert!(mutant(&output, "'en'", "StringLiteral").reads.is_none());
        // Everything else instruments as it did.
        assert!(output.code.contains("export function words(lang: string)"));
    }

    /// Only what the literal alone carries is linked: a value a call, a
    /// member read, a spread or an operator over a name makes is not, nor a
    /// literal replaced whole whose evaluation does more than make it; but
    /// their siblings are. A key the source does not say is read as any
    /// read of its literal.
    #[test]
    fn only_values_the_literal_alone_carries_are_linked() {
        let source = concat!(
            "import { Icon, name, registry } from './lib';\n",
            "const ITEMS = [\n",
            "  { label: 'Home', icon: Icon, size: 2 * 3, title: `a${'b'}` },\n",
            "  { label: 'Made', made: register('x'), named: name + 'y', member: registry.z },\n",
            "  ...registry.more,\n",
            "  { label: 'Late' },\n",
            "];\n",
            "const AFTER = ['before', ...registry.more, 'after'];\n",
            "const KEYED = { [name]: 'dynamic', ['fixed']: 'known', 1: 'one', __proto__: { p: 'proto' } };\n",
            "let reassigned = { a: true }, plain = 'p';\n",
            "export const config = { matcher: ['/a'] };\n",
            "const later = (() => ({ inner: 'fn' }))();\n",
        );
        let output = instrumented(source, "src/items.ts", true);
        let reads =
            |original: &str, mutator: &str| mutant(&output, original, mutator).reads.clone();
        let calls = tracked(&output);
        let keys_of = |id: &str| {
            calls
                .iter()
                .find(|(any, _)| any == id)
                .map(|(_, keys)| keys.clone())
                .unwrap_or_else(|| panic!("{id} is not tracked: {}", output.code))
        };
        // The first item: every value is the literal's alone, a name read
        // alone among them; the item replaced whole evaluates none of them.
        let home = reads("'Home'", "StringLiteral").unwrap();
        assert!(reads("2 * 3", "ArithmeticOperator").is_some());
        assert!(reads("`a${'b'}`", "StringLiteral").is_some());
        let first = reads(
            "{ label: 'Home', icon: Icon, size: 2 * 3, title: `a${'b'}` }",
            "ObjectLiteral",
        )
        .unwrap();
        assert_eq!(keys_of(&first.read), ["label", "size", "title"]);
        assert_ne!(home.read, first.read);
        // The second: a call's argument and an operator over a name are not
        // linked, and the item replaced whole would skip the call.
        assert!(reads("'x'", "StringLiteral").is_none());
        assert!(reads("'y'", "StringLiteral").is_none());
        assert!(
            reads(
                "{ label: 'Made', made: register('x'), named: name + 'y', member: registry.z }",
                "ObjectLiteral"
            )
            .is_none()
        );
        assert!(reads("'Made'", "StringLiteral").is_some());
        // An item after a spread is a literal of its own; the array
        // replaced whole would skip the spread.
        assert!(reads("{ label: 'Late' }", "ObjectLiteral").is_some());
        assert!(reads("'Late'", "StringLiteral").is_some());
        assert!(
            mutant(&output, "[\n  { label: 'Home', icon: Icon, size: 2 * 3, title: `a${'b'}` },\n  { label: 'Made', made: register('x'), named: name + 'y', member: registry.z },\n  ...registry.more,\n  { label: 'Late' },\n]", "ArrayDeclaration")
                .reads
                .is_none()
        );
        // After a spread an element's index is not known: any read of the
        // array.
        let before = reads("'before'", "StringLiteral").unwrap();
        let after = reads("'after'", "StringLiteral").unwrap();
        assert_eq!(keys_of(&after.read), ["0"]);
        assert_ne!(before.read, after.read);
        // A computed key the source does not say is any read; one it says
        // is its own; a number names its key; `__proto__` sets the
        // prototype, read through the literal by keys of its own.
        let dynamic = reads("'dynamic'", "StringLiteral").unwrap();
        let known = reads("'known'", "StringLiteral").unwrap();
        let one = reads("'one'", "StringLiteral").unwrap();
        assert!(reads("'proto'", "StringLiteral").is_some());
        let keyed = calls
            .iter()
            .find(|(_, keys)| keys.contains(&"fixed".to_owned()))
            .unwrap();
        assert_eq!(dynamic.read, keyed.0);
        assert_eq!(keyed.1, ["fixed", "1"]);
        assert_ne!(known.read, one.read);
        assert!(
            reads(
                "{ [name]: 'dynamic', ['fixed']: 'known', 1: 'one', __proto__: { p: 'proto' } }",
                "ObjectLiteral"
            )
            .is_none()
        );
        // Every declaration kind; not a literal Next.js reads from the source
        // where it does (middleware's `config`), nor one a call makes.
        assert!(reads("true", "BooleanLiteral").is_some());
        assert!(reads("'/a'", "StringLiteral").is_some());
        let middleware = instrumented(source, "src/middleware.ts", true);
        assert!(
            mutant(&middleware, "'/a'", "StringLiteral").reads.is_none(),
            "{}",
            middleware.code
        );
        assert!(reads("'fn'", "StringLiteral").is_none());
    }

    /// Every form a literal's values may take, and what each makes of its
    /// mutants: a literal under type syntax is tracked once, whatever wraps
    /// it; a literal of plain values may be replaced whole; one holding a
    /// computed key, a spread or an `in` may not; a mutant in a key, a
    /// spread or a function is not linked; a `__proto__` that does not set
    /// the prototype is a key like another.
    #[test]
    fn every_form_of_a_value_is_judged_as_its_evaluation_allows() {
        let source = concat!(
            "const LEAVES = { a: 1n, b: null, c: /x/, d: function () {}, e: () => 1, f: true, g: `t${1}`, h: -1, i: !0, j: 1 + 2, k: 1 || 2, l: true ? { m: 1 } : [2], n: (1, 2), o: f<string>, p: [, 'q'], r: 'r' as string, s: 's'!, t: <string>'t' };\n",
            "const NOT_DROPPED = [{ [k]: 'u' }, { ...{ v: 'v' } }, [...w, 'x'], { y: 'y' in w }, { z: delete w.z }, { aa: k + 1 }];\n",
            "const WRAPPED = ({ ab: 'ab' }) satisfies object;\n",
            "const ASSERTED = { ac: 'ac' } as Record<string, string>;\n",
            "const NON_NULL = { ad: 'ad' }!;\n",
            "const ANGLED = <object>{ ae: 'ae' };\n",
            "const KEYS = { 1.5: 'half', 1e21: 'huge', [2 * 3]: 'six', ['__proto__']: 'computed', __proto__() { return 'method'; }, get __proto__() { return 'getter'; } };\n",
            "const SHORT = { __proto__ };\n",
            "const CODE = { f() { return 'in method'; }, g: () => 'in arrow', C: class { m() { return 'in class'; } } };\n",
            "let NONE, NUMBER = 1 + 1;\n",
        );
        let output = instrumented(source, "src/forms.ts", true);
        let linked =
            |original: &str, mutator: &str| mutant(&output, original, mutator).reads.is_some();
        // Each leaf of a literal of plain values: replaced whole.
        assert!(linked(
            "{ a: 1n, b: null, c: /x/, d: function () {}, e: () => 1, f: true, g: `t${1}`, h: -1, i: !0, j: 1 + 2, k: 1 || 2, l: true ? { m: 1 } : [2], n: (1, 2), o: f<string>, p: [, 'q'], r: 'r' as string, s: 's'!, t: <string>'t' }",
            "ObjectLiteral"
        ));
        assert!(linked("'q'", "StringLiteral"));
        assert!(linked("1 + 2", "ArithmeticOperator"));
        assert!(linked("true", "BooleanLiteral"));
        // A computed key, a spread, `in`, `delete`, an operator over a name.
        for (original, mutator) in [
            ("{ [k]: 'u' }", "ObjectLiteral"),
            ("{ ...{ v: 'v' } }", "ObjectLiteral"),
            ("[...w, 'x']", "ArrayDeclaration"),
            ("{ y: 'y' in w }", "ObjectLiteral"),
            ("{ z: delete w.z }", "ObjectLiteral"),
            ("{ aa: k + 1 }", "ObjectLiteral"),
            ("'v'", "StringLiteral"),
        ] {
            assert!(!linked(original, mutator), "{original}\n{}", output.code);
        }
        assert!(linked("'u'", "StringLiteral"));
        assert!(linked("'x'", "StringLiteral"));
        // Under type syntax, tracked once; StrykerJS mutates nothing under
        // `as`, so nothing is.
        assert!(
            !output.code.contains("const ASSERTED = __supercov"),
            "{}",
            output.code
        );
        for name in ["WRAPPED", "NON_NULL", "ANGLED"] {
            let line = output
                .code
                .lines()
                .find(|line| line.contains(&format!("const {name} =")))
                .unwrap();
            assert_eq!(line.matches("TrackV2").count(), 1, "{line}");
        }
        for original in ["'ab'", "'ad'", "'ae'"] {
            assert!(linked(original, "StringLiteral"), "{original}");
        }
        // Keys: a fraction or a huge number is not said plainly, a mutated
        // computed key is not linked, and only `__proto__: x` sets the
        // prototype.
        let keys = tracked(&output)
            .into_iter()
            .find(|(_, keys)| keys.contains(&"__proto__".to_owned()))
            .unwrap_or_else(|| panic!("{}", output.code));
        assert_eq!(keys.1, ["__proto__"]);
        assert_eq!(
            mutant(&output, "'half'", "StringLiteral")
                .reads
                .as_ref()
                .unwrap()
                .read,
            keys.0
        );
        assert_eq!(
            mutant(&output, "'huge'", "StringLiteral")
                .reads
                .as_ref()
                .unwrap()
                .read,
            keys.0
        );
        assert!(!linked("2 * 3", "ArithmeticOperator"));
        assert!(linked("'six'", "StringLiteral"));
        assert!(linked("'computed'", "StringLiteral"));
        // Code in a method, an arrow or a class is not module-level.
        for original in [
            "'in method'",
            "'in arrow'",
            "'in class'",
            "'method'",
            "'getter'",
        ] {
            assert!(!linked(original, "StringLiteral"), "{original}");
        }
        // Not a literal: nothing to track.
        assert!(!linked("1 + 1", "ArithmeticOperator"));
        // A shorthand `__proto__` is an own property, and a name read alone.
        assert!(linked("{ __proto__ }", "ObjectLiteral"));
    }

    /// Which declarations: every exported one but those Next.js reads from
    /// a middleware's source, whatever their pattern, and no `using`.
    #[test]
    fn declarations_a_framework_reads_or_disposes_of_are_not_tracked() {
        let source = concat!(
            "export const config = { matcher: ['/a'] };\n",
            "export const other = { b: 'b' };\n",
            "const local = { c: 'c' };\n",
            "export const { d } = { d: 'd' };\n",
            "using e = { f: 'f', [Symbol.dispose]() {} };\n",
        );
        let output = instrumented(source, "middleware.ts", true);
        let linked = |original: &str| mutant(&output, original, "StringLiteral").reads.is_some();
        assert!(!linked("'/a'"));
        assert!(linked("'b'"));
        assert!(linked("'c'"));
        assert!(linked("'d'"));
        assert!(!linked("'f'"));
    }

    /// Where a module's top level awaits, the code after resumes marked as
    /// loading; a function's await is left alone.
    #[test]
    fn a_top_level_await_marks_the_module_loading_where_it_resumes() {
        let source = "const config = await load();\nfor await (const part of parts()) use(part);\nfor (const part of parts()) use(part);\nexport async function later() {\n  return await load();\n}\nconst arrow = async () => await load();\nclass Later {\n  async m() {\n    await load();\n  }\n}\n";
        let output = instrumented(source, "src/top.ts", true);
        assert!(
            output.code.contains("ResumeV2")
                && output.code.contains("(__supercovFile7, await load())"),
            "{}",
            output.code
        );
        // Imported once, called twice.
        assert_eq!(
            output.code.matches("ResumeV2").count(),
            3,
            "{}",
            output.code
        );
        assert!(
            output.code.contains("return await load();"),
            "{}",
            output.code
        );
        let without = instrumented(source, "src/top.ts", false);
        assert!(!without.code.contains("ResumeV2"), "{}", without.code);
        // Awaits in functions alone: nothing to mark.
        let inner = "export async function later() {\n  return await load();\n}\nconst arrow = async () => await load();\nclass Later {\n  async m() {\n    await load();\n  }\n}\nfor (const part of parts()) use(part);\n";
        assert!(
            !instrumented(inner, "src/inner.ts", true)
                .code
                .contains("ResumeV2")
        );
    }
}
