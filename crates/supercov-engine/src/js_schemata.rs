//! Strong mode: every listed mutant written into the program behind a
//! switch (mutant schemata, Untch 1992), as StrykerJS 10.1.1 writes them.
//!
//! A file's mutants are the ones `js_mutants::enumerate` lists, in its
//! order, and a mutant is switched on by its index in that list. The file
//! reads a record from the runtime once (`const __supercovStrong =
//! strongSlot("src/a.ts")`) and each switch reads the record's `a`, the
//! active index, every time it is evaluated, so the active mutant can change
//! while a process runs:
//!
//! - an expression: `(__supercovStrong.a === 17 ? (__supercovStrong.h(),
//!   <replacement>) : __supercovStrong.a === 18 ? ... : <original>)`;
//! - a statement: `if (__supercovStrong.a === 19) { __supercovStrong.h();
//!   <replacement> } else { <original> }`, and the same inside a function
//!   body or a `case` for their statements.
//!
//! `h()` counts the active mutant's evaluations and throws the runtime's
//! timeout error past its limit. With no mutant active every switch takes
//! the original.
//!
//! # Placement
//!
//! StrykerJS's placers decide where a mutant's switch goes: the mutated node
//! itself where an expression placer, a statement placer or a `case` placer
//! can wrap it, or else its nearest ancestor that can be wrapped, with the
//! mutant applied to a copy of that ancestor. Their rules are kept: no
//! switch on an object literal's key, on a link of a call or member chain,
//! on a template's tag, on `delete`'s operand or an assignment's target.
//! Where wrapping would change the program, Supercov does not wrap where
//! StrykerJS would, and goes on up instead: a declaration (`let`, `const`,
//! a function or class declaration, an import or export), the body of a
//! label, a `case` that declares something lexically, a property's key
//! written without brackets, and any place that is not an expression slot
//! (an assignment target, a JSX child). A mutant with no such ancestor, or
//! one in code the instrumenter keeps as written (a function whose source
//! the program reads, a `with` body), is listed as not placeable, with the
//! reason, and gets no switch.
//!
//! A copy of an ancestor holds the switches StrykerJS's copy holds: those
//! placed before the mutated node was reached, which end before it starts.
//! The others are taken out of the copy again.
//!
//! An anonymous function or class wrapped where it would be named by its
//! place (`const f = () => 1`) keeps its name: it is written as
//! `({ "f": <it> })["f"]`, which names it the same.
//!
//! # Static mutants
//!
//! A mutant is static (StrykerJS's notion) when its switch is evaluated
//! while the module loads: no function, and no instance field's
//! initializer, is around it.

use std::collections::{HashMap, HashSet};

use oxc_allocator::{Address, Allocator, CloneIn, GetAddress, TakeIn};
use oxc_ast::{
    AstBuilder, AstKind, NONE,
    ast::{
        Argument, AssignmentOperator, AssignmentTarget, BinaryOperator, BindingPattern,
        ChainElement, Expression, FormalParameterKind, FunctionBody, LogicalOperator, PropertyKey,
        PropertyKind, Statement, SwitchCase, TemplateElementValue, TemplateLiteral, UnaryOperator,
        UpdateOperator, VariableDeclarationKind,
    },
};
use oxc_ast_visit::{Visit, VisitMut, walk, walk_mut};
use oxc_span::{GetSpan, Span};
use oxc_syntax::number::NumberBase;

use crate::js_mutants::LedgerMutant;

/// Why a listed mutant has no switch.
pub(crate) const NO_PLACEABLE_ANCESTOR: &str = "no-placeable-ancestor";
pub(crate) const FUNCTION_SOURCE: &str = "function-source";
pub(crate) const WITH_STATEMENT: &str = "with-statement";
pub(crate) const NODE_NOT_FOUND: &str = "node-not-found";

/// What strong mode says of one listed mutant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StrongMutant {
    /// Evaluated while the module loads.
    pub is_static: bool,
    /// Why it has no switch, when it has none.
    pub unplaceable: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlaceKind {
    Expression,
    Statement,
    FunctionBody,
    SwitchCase,
}

/// The node a switch goes on: an expression or a statement by its address,
/// a function body or a `case` by its span.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum PlaceKey {
    Node(Address),
    Body(u32, u32),
    Case(u32, u32),
}

#[derive(Debug, Clone)]
struct Placement {
    key: PlaceKey,
    /// The mutant's switch is on the mutated node itself.
    itself: bool,
    /// The name an anonymous function or class has from its place.
    name: Option<String>,
}

#[derive(Debug, Clone)]
struct MutantPlan {
    placement: Result<Placement, &'static str>,
    is_static: bool,
}

/// The kind of node a mutant is on, as far as telling nodes with one span
/// apart needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodeKind {
    Binary,
    Logical,
    Assignment,
    Update,
    Unary,
    BooleanLiteral,
    StringLiteral,
    Template,
    Call,
    New,
    StaticMember,
    ComputedMember,
    PrivateField,
    Arrow,
    Object,
    Array,
    OtherExpression,
    ForWithoutTest,
    ExpressionStatement,
    Throw,
    Block,
    FunctionBody,
    SwitchCase,
    Other,
}

impl NodeKind {
    fn is_expression(self) -> bool {
        !matches!(
            self,
            NodeKind::ForWithoutTest
                | NodeKind::ExpressionStatement
                | NodeKind::Throw
                | NodeKind::Block
                | NodeKind::FunctionBody
                | NodeKind::SwitchCase
                | NodeKind::Other
        )
    }
}

/// Whether a mutant of `mutant`'s mutator and descriptor is on a node of
/// `kind`.
fn accepts(mutant: &LedgerMutant, kind: NodeKind) -> bool {
    let descriptor = mutant.replacement.as_str();
    match mutant.mutator {
        "ArithmeticOperator" | "EqualityOperator" => kind == NodeKind::Binary,
        "LogicalOperator" => kind == NodeKind::Logical,
        "AssignmentOperator" => kind == NodeKind::Assignment,
        "UpdateOperator" => kind == NodeKind::Update,
        "UnaryOperator" => kind == NodeKind::Unary,
        "BooleanLiteral" => matches!(kind, NodeKind::BooleanLiteral | NodeKind::Unary),
        "ConditionalExpression" => {
            if descriptor == "empty" {
                kind == NodeKind::SwitchCase
            } else {
                kind == NodeKind::ForWithoutTest || kind.is_expression()
            }
        }
        "StringLiteral" => matches!(kind, NodeKind::StringLiteral | NodeKind::Template),
        "MethodExpression" => kind == NodeKind::Call,
        "OptionalChaining" => match descriptor {
            "()" => kind == NodeKind::Call,
            "[]" => kind == NodeKind::ComputedMember,
            _ if descriptor.starts_with(".#") => kind == NodeKind::PrivateField,
            _ => kind == NodeKind::StaticMember,
        },
        "ArrowFunction" => kind == NodeKind::Arrow,
        "BlockStatement" => matches!(kind, NodeKind::Block | NodeKind::FunctionBody),
        "ObjectLiteral" => kind == NodeKind::Object,
        "ArrayDeclaration" => {
            if descriptor.starts_with('[') {
                kind == NodeKind::Array
            } else if descriptor.starts_with("new") {
                kind == NodeKind::New
            } else {
                kind == NodeKind::Call
            }
        }
        "CallExpression" => matches!(kind, NodeKind::ExpressionStatement | NodeKind::Throw),
        _ => false,
    }
}

fn ast_kind(kind: &AstKind<'_>) -> NodeKind {
    match kind {
        AstKind::BinaryExpression(_) => NodeKind::Binary,
        AstKind::LogicalExpression(_) => NodeKind::Logical,
        AstKind::AssignmentExpression(_) => NodeKind::Assignment,
        AstKind::UpdateExpression(_) => NodeKind::Update,
        AstKind::UnaryExpression(_) => NodeKind::Unary,
        AstKind::BooleanLiteral(_) => NodeKind::BooleanLiteral,
        AstKind::StringLiteral(_) => NodeKind::StringLiteral,
        AstKind::TemplateLiteral(_) => NodeKind::Template,
        AstKind::CallExpression(_) => NodeKind::Call,
        AstKind::NewExpression(_) => NodeKind::New,
        AstKind::StaticMemberExpression(_) => NodeKind::StaticMember,
        AstKind::ComputedMemberExpression(_) => NodeKind::ComputedMember,
        AstKind::PrivateFieldExpression(_) => NodeKind::PrivateField,
        AstKind::ArrowFunctionExpression(_) => NodeKind::Arrow,
        AstKind::ObjectExpression(_) => NodeKind::Object,
        AstKind::ArrayExpression(_) => NodeKind::Array,
        AstKind::NullLiteral(_)
        | AstKind::NumericLiteral(_)
        | AstKind::BigIntLiteral(_)
        | AstKind::RegExpLiteral(_)
        | AstKind::IdentifierReference(_)
        | AstKind::MetaProperty(_)
        | AstKind::ThisExpression(_)
        | AstKind::AwaitExpression(_)
        | AstKind::ConditionalExpression(_)
        | AstKind::ImportExpression(_)
        | AstKind::SequenceExpression(_)
        | AstKind::TaggedTemplateExpression(_)
        | AstKind::YieldExpression(_)
        | AstKind::PrivateInExpression(_)
        | AstKind::JSXElement(_)
        | AstKind::JSXFragment(_)
        | AstKind::TSAsExpression(_)
        | AstKind::TSSatisfiesExpression(_)
        | AstKind::TSTypeAssertion(_)
        | AstKind::TSNonNullExpression(_)
        | AstKind::TSInstantiationExpression(_) => NodeKind::OtherExpression,
        AstKind::ForStatement(statement) if statement.test.is_none() => NodeKind::ForWithoutTest,
        AstKind::ExpressionStatement(_) => NodeKind::ExpressionStatement,
        AstKind::ThrowStatement(_) => NodeKind::Throw,
        AstKind::BlockStatement(_) => NodeKind::Block,
        AstKind::FunctionBody(_) => NodeKind::FunctionBody,
        AstKind::SwitchCase(_) => NodeKind::SwitchCase,
        _ => NodeKind::Other,
    }
}

fn expression_kind(expression: &Expression<'_>) -> NodeKind {
    match expression {
        Expression::BinaryExpression(_) => NodeKind::Binary,
        Expression::LogicalExpression(_) => NodeKind::Logical,
        Expression::AssignmentExpression(_) => NodeKind::Assignment,
        Expression::UpdateExpression(_) => NodeKind::Update,
        Expression::UnaryExpression(_) => NodeKind::Unary,
        Expression::BooleanLiteral(_) => NodeKind::BooleanLiteral,
        Expression::StringLiteral(_) => NodeKind::StringLiteral,
        Expression::TemplateLiteral(_) => NodeKind::Template,
        Expression::CallExpression(_) => NodeKind::Call,
        Expression::NewExpression(_) => NodeKind::New,
        Expression::StaticMemberExpression(_) => NodeKind::StaticMember,
        Expression::ComputedMemberExpression(_) => NodeKind::ComputedMember,
        Expression::PrivateFieldExpression(_) => NodeKind::PrivateField,
        Expression::ArrowFunctionExpression(_) => NodeKind::Arrow,
        Expression::ObjectExpression(_) => NodeKind::Object,
        Expression::ArrayExpression(_) => NodeKind::Array,
        Expression::ParenthesizedExpression(_)
        | Expression::ChainExpression(_)
        | Expression::FunctionExpression(_)
        | Expression::ClassExpression(_)
        | Expression::Super(_) => NodeKind::Other,
        _ => NodeKind::OtherExpression,
    }
}

/// The node kind of a chain's last link.
fn chain_kind(element: &ChainElement<'_>) -> NodeKind {
    match element {
        ChainElement::CallExpression(_) => NodeKind::Call,
        ChainElement::TSNonNullExpression(_) => NodeKind::OtherExpression,
        ChainElement::StaticMemberExpression(_) => NodeKind::StaticMember,
        ChainElement::ComputedMemberExpression(_) => NodeKind::ComputedMember,
        ChainElement::PrivateFieldExpression(_) => NodeKind::PrivateField,
    }
}

fn statement_kind(statement: &Statement<'_>) -> NodeKind {
    match statement {
        Statement::ForStatement(statement) if statement.test.is_none() => NodeKind::ForWithoutTest,
        Statement::ExpressionStatement(_) => NodeKind::ExpressionStatement,
        Statement::ThrowStatement(_) => NodeKind::Throw,
        Statement::BlockStatement(_) => NodeKind::Block,
        _ => NodeKind::Other,
    }
}

// ---- planning ---------------------------------------------------------

/// Every expression and statement slot of the program, by the address of
/// the node in it: only a node in a slot can be swapped for a switch.
#[derive(Default)]
struct Slots {
    expressions: HashSet<Address>,
    statements: HashSet<Address>,
}

impl<'a> Visit<'a> for Slots {
    fn visit_expression(&mut self, expression: &Expression<'a>) {
        self.expressions.insert(expression.address());
        walk::walk_expression(self, expression);
    }

    fn visit_statement(&mut self, statement: &Statement<'a>) {
        self.statements.insert(statement.address());
        walk::walk_statement(self, statement);
    }
}

struct Planner<'p, 'a> {
    mutants: &'p [LedgerMutant],
    pending: HashMap<(u32, u32), Vec<usize>>,
    slots: Slots,
    /// Functions the instrumenter keeps as written, by span.
    kept: &'p HashSet<(u32, u32)>,
    stack: Vec<AstKind<'a>>,
    plans: Vec<Option<MutantPlan>>,
}

impl<'a> Planner<'_, 'a> {
    fn concise_body(&self, index: usize) -> bool {
        index > 0
            && matches!(self.stack[index - 1], AstKind::ArrowFunctionExpression(arrow) if arrow.expression)
    }

    /// Nodes Babel has none for: parentheses, a chain wrapper, and an
    /// arrow's concise body.
    fn transparent(&self, index: usize) -> bool {
        match self.stack[index] {
            AstKind::ParenthesizedExpression(_)
            | AstKind::ChainExpression(_)
            | AstKind::FormalParameters(_) => true,
            AstKind::FunctionBody(_) => self.concise_body(index),
            AstKind::ExpressionStatement(_) => index > 0 && self.concise_body(index - 1),
            _ => false,
        }
    }

    fn babel_parent(&self, index: usize) -> Option<usize> {
        (0..index)
            .rev()
            .find(|candidate| !self.transparent(*candidate))
    }

    /// The node at `index`, as an expression's slot sees it: a chain's last
    /// link is in the slot of the chain.
    fn slot_address(&self, index: usize) -> Address {
        if index > 0 && matches!(self.stack[index - 1], AstKind::ChainExpression(_)) {
            self.stack[index - 1].address()
        } else {
            self.stack[index].address()
        }
    }

    fn placeable(&self, index: usize) -> Option<PlaceKind> {
        let kind = self.stack[index];
        match kind {
            AstKind::ParenthesizedExpression(_) | AstKind::ChainExpression(_) => return None,
            AstKind::FunctionBody(_) => {
                return (!self.concise_body(index)).then_some(PlaceKind::FunctionBody);
            }
            AstKind::SwitchCase(case) => {
                let lexical = case.consequent.iter().any(|statement| {
                    matches!(
                        statement,
                        Statement::VariableDeclaration(declaration)
                            if declaration.kind != VariableDeclarationKind::Var
                    ) || matches!(
                        statement,
                        Statement::FunctionDeclaration(_) | Statement::ClassDeclaration(_)
                    )
                });
                return (!lexical).then_some(PlaceKind::SwitchCase);
            }
            _ => {}
        }
        if self.slots.statements.contains(&kind.address()) {
            if self.transparent(index) {
                return None;
            }
            let declaration = match kind {
                AstKind::VariableDeclaration(declaration) => {
                    declaration.kind != VariableDeclarationKind::Var || declaration.declare
                }
                AstKind::Function(_)
                | AstKind::Class(_)
                | AstKind::ImportDeclaration(_)
                | AstKind::ExportNamedDeclaration(_)
                | AstKind::ExportDefaultDeclaration(_)
                | AstKind::ExportAllDeclaration(_)
                | AstKind::TSExportAssignment(_)
                | AstKind::TSNamespaceExportDeclaration(_)
                | AstKind::TSTypeAliasDeclaration(_)
                | AstKind::TSInterfaceDeclaration(_)
                | AstKind::TSEnumDeclaration(_)
                | AstKind::TSModuleDeclaration(_)
                | AstKind::TSGlobalDeclaration(_)
                | AstKind::TSImportEqualsDeclaration(_) => true,
                _ => false,
            };
            let labeled =
                index > 0 && matches!(self.stack[index - 1], AstKind::LabeledStatement(_));
            return (!declaration && !labeled).then_some(PlaceKind::Statement);
        }
        if !self.slots.expressions.contains(&self.slot_address(index)) {
            return None;
        }
        if matches!(kind, AstKind::Super(_) | AstKind::MetaProperty(_)) {
            return None;
        }
        self.valid_expression(index)
            .then_some(PlaceKind::Expression)
    }

    /// StrykerJS's `isValidExpression`, and a key written without brackets.
    fn valid_expression(&self, index: usize) -> bool {
        let kind = self.stack[index];
        let span = kind.span();
        let Some(parent) = self.babel_parent(index) else {
            return true;
        };
        let within = |outer: Span| outer.start <= span.start && span.end <= outer.end;
        let is_key = |key: &PropertyKey<'_>| within(key.span());
        let link = matches!(
            kind,
            AstKind::StaticMemberExpression(_)
                | AstKind::ComputedMemberExpression(_)
                | AstKind::PrivateFieldExpression(_)
                | AstKind::CallExpression(_)
                | AstKind::TSNonNullExpression(_)
        );
        match self.stack[parent] {
            AstKind::ObjectProperty(property) => !is_key(&property.key),
            AstKind::BindingProperty(property) => !is_key(&property.key),
            AstKind::AssignmentTargetPropertyProperty(property) => !is_key(&property.name),
            AstKind::MethodDefinition(method) => method.computed || !is_key(&method.key),
            AstKind::PropertyDefinition(property) => property.computed || !is_key(&property.key),
            AstKind::AccessorProperty(property) => property.computed || !is_key(&property.key),
            AstKind::TaggedTemplateExpression(_) => false,
            AstKind::UnaryExpression(unary) => unary.operator != UnaryOperator::Delete,
            AstKind::AssignmentExpression(assignment) => !within(assignment.left.span()),
            AstKind::StaticMemberExpression(_) | AstKind::PrivateFieldExpression(_) => !link,
            AstKind::ComputedMemberExpression(member) => !link || within(member.expression.span()),
            AstKind::TSNonNullExpression(_) => !link,
            AstKind::CallExpression(call) => !(link && within(call.callee.span())),
            _ => true,
        }
    }

    /// The name an anonymous function or class at `index` has from where it
    /// is written.
    fn inferred_name(&self, index: usize) -> Option<String> {
        let anonymous = match self.stack[index] {
            AstKind::ArrowFunctionExpression(_) => true,
            AstKind::Function(function) => function.id.is_none(),
            AstKind::Class(class) => class.id.is_none(),
            _ => false,
        };
        if !anonymous {
            return None;
        }
        let span = self.stack[index].span();
        let within = |outer: Span| outer.start <= span.start && span.end <= outer.end;
        let key_name = |key: &PropertyKey<'_>| match key {
            PropertyKey::StaticIdentifier(name) => Some(name.name.to_string()),
            PropertyKey::PrivateIdentifier(name) => Some(format!("#{}", name.name)),
            PropertyKey::StringLiteral(literal) => Some(literal.value.to_string()),
            _ => None,
        };
        let parent = self.babel_parent(index)?;
        match self.stack[parent] {
            AstKind::VariableDeclarator(declarator) => match &declarator.id {
                BindingPattern::BindingIdentifier(id)
                    if declarator
                        .init
                        .as_ref()
                        .is_some_and(|init| within(init.span())) =>
                {
                    Some(id.name.to_string())
                }
                _ => None,
            },
            AstKind::AssignmentExpression(assignment)
                if matches!(
                    assignment.operator,
                    AssignmentOperator::Assign
                        | AssignmentOperator::LogicalAnd
                        | AssignmentOperator::LogicalOr
                        | AssignmentOperator::LogicalNullish
                ) && within(assignment.right.span()) =>
            {
                match &assignment.left {
                    AssignmentTarget::AssignmentTargetIdentifier(id) => Some(id.name.to_string()),
                    _ => None,
                }
            }
            AstKind::ObjectProperty(property)
                if !property.computed
                    && !property.method
                    && property.kind == PropertyKind::Init
                    && within(property.value.span()) =>
            {
                key_name(&property.key)
            }
            AstKind::PropertyDefinition(property)
                if !property.computed
                    && property
                        .value
                        .as_ref()
                        .is_some_and(|value| within(value.span())) =>
            {
                key_name(&property.key)
            }
            AstKind::AssignmentPattern(pattern) if within(pattern.right.span()) => {
                match &pattern.left {
                    BindingPattern::BindingIdentifier(id) => Some(id.name.to_string()),
                    _ => None,
                }
            }
            AstKind::FormalParameter(parameter)
                if parameter
                    .initializer
                    .as_ref()
                    .is_some_and(|initializer| within(initializer.span())) =>
            {
                match &parameter.pattern {
                    BindingPattern::BindingIdentifier(id) => Some(id.name.to_string()),
                    _ => None,
                }
            }
            AstKind::AssignmentTargetWithDefault(target) if within(target.init.span()) => {
                match &target.binding {
                    AssignmentTarget::AssignmentTargetIdentifier(id) => Some(id.name.to_string()),
                    _ => None,
                }
            }
            AstKind::ExportDefaultDeclaration(_) => Some("default".to_owned()),
            _ => None,
        }
    }

    /// Whether the node at `index` runs only when a function is called: a
    /// function or an instance field's initializer is around it.
    fn in_function(&self, index: usize) -> bool {
        let span = self.stack[index].span();
        let within = |outer: Span| outer.start <= span.start && span.end <= outer.end;
        self.stack[..index].iter().any(|ancestor| match ancestor {
            AstKind::Function(_) | AstKind::ArrowFunctionExpression(_) => true,
            AstKind::PropertyDefinition(property) => {
                !property.r#static
                    && property
                        .value
                        .as_ref()
                        .is_some_and(|value| within(value.span()))
            }
            AstKind::AccessorProperty(property) => {
                !property.r#static
                    && property
                        .value
                        .as_ref()
                        .is_some_and(|value| within(value.span()))
            }
            _ => false,
        })
    }

    fn plan(&self, index: usize) -> MutantPlan {
        let kept = self.stack[..index]
            .iter()
            .find_map(|ancestor| match ancestor {
                AstKind::WithStatement(_) => Some(WITH_STATEMENT),
                AstKind::Function(_) | AstKind::ArrowFunctionExpression(_)
                    if self
                        .kept
                        .contains(&(ancestor.span().start, ancestor.span().end)) =>
                {
                    Some(FUNCTION_SOURCE)
                }
                _ => None,
            });
        if let Some(reason) = kept {
            return MutantPlan {
                placement: Err(reason),
                is_static: !self.in_function(index),
            };
        }
        let found = (0..=index)
            .rev()
            .find_map(|candidate| self.placeable(candidate).map(|kind| (candidate, kind)));
        match found {
            Some((at, kind)) => {
                let span = self.stack[at].span();
                let key = match kind {
                    PlaceKind::Expression => PlaceKey::Node(self.slot_address(at)),
                    PlaceKind::Statement => PlaceKey::Node(self.stack[at].address()),
                    PlaceKind::FunctionBody => PlaceKey::Body(span.start, span.end),
                    PlaceKind::SwitchCase => PlaceKey::Case(span.start, span.end),
                };
                MutantPlan {
                    placement: Ok(Placement {
                        key,
                        itself: at == index,
                        name: (kind == PlaceKind::Expression)
                            .then(|| self.inferred_name(at))
                            .flatten(),
                    }),
                    is_static: !self.in_function(at),
                }
            }
            None => MutantPlan {
                placement: Err(NO_PLACEABLE_ANCESTOR),
                is_static: !self.in_function(index),
            },
        }
    }
}

impl<'a> Visit<'a> for Planner<'_, 'a> {
    fn enter_node(&mut self, kind: AstKind<'a>) {
        self.stack.push(kind);
        let span = kind.span();
        let node_kind = ast_kind(&kind);
        if node_kind == NodeKind::Other {
            return;
        }
        let Some(waiting) = self.pending.get_mut(&(span.start, span.end)) else {
            return;
        };
        let mutants = self.mutants;
        let (matched, rest): (Vec<usize>, Vec<usize>) = waiting
            .iter()
            .partition(|index| accepts(&mutants[**index], node_kind));
        *waiting = rest;
        let index = self.stack.len() - 1;
        for mutant in matched {
            self.plans[mutant] = Some(self.plan(index));
        }
    }

    fn leave_node(&mut self, _kind: AstKind<'a>) {
        self.stack.pop();
    }
}

fn plans<'a>(
    program: &oxc_ast::ast::Program<'a>,
    mutants: &[LedgerMutant],
    kept: &HashSet<(u32, u32)>,
) -> Vec<MutantPlan> {
    let mut slots = Slots::default();
    slots.visit_program(program);
    let mut pending = HashMap::<(u32, u32), Vec<usize>>::new();
    for (index, mutant) in mutants.iter().enumerate() {
        pending
            .entry((mutant.span.start, mutant.span.end))
            .or_default()
            .push(index);
    }
    let mut planner = Planner {
        mutants,
        pending,
        slots,
        kept,
        stack: Vec::new(),
        plans: vec![None; mutants.len()],
    };
    planner.visit_program(program);
    planner
        .plans
        .into_iter()
        .map(|plan| {
            plan.unwrap_or(MutantPlan {
                placement: Err(NODE_NOT_FOUND),
                is_static: false,
            })
        })
        .collect()
}

// ---- replacements -----------------------------------------------------

/// A mutant's replacement for the node it is on.
#[cfg(test)]
pub(crate) enum Replacement<'a> {
    Expression(Expression<'a>),
    Statement(Statement<'a>),
    /// A function body emptied.
    EmptyBody,
    /// A `case` without its statements.
    Case(SwitchCase<'a>),
    /// A tagged template's template.
    Quasi(TemplateLiteral<'a>),
}

fn binary_operator(text: &str) -> Option<BinaryOperator> {
    Some(match text {
        "+" => BinaryOperator::Addition,
        "-" => BinaryOperator::Subtraction,
        "*" => BinaryOperator::Multiplication,
        "/" => BinaryOperator::Division,
        "<" => BinaryOperator::LessThan,
        "<=" => BinaryOperator::LessEqualThan,
        ">" => BinaryOperator::GreaterThan,
        ">=" => BinaryOperator::GreaterEqualThan,
        "==" => BinaryOperator::Equality,
        "!=" => BinaryOperator::Inequality,
        "===" => BinaryOperator::StrictEquality,
        "!==" => BinaryOperator::StrictInequality,
        _ => return None,
    })
}

fn assignment_operator(text: &str) -> Option<AssignmentOperator> {
    Some(match text {
        "+=" => AssignmentOperator::Addition,
        "-=" => AssignmentOperator::Subtraction,
        "*=" => AssignmentOperator::Multiplication,
        "/=" => AssignmentOperator::Division,
        "<<=" => AssignmentOperator::ShiftLeft,
        ">>=" => AssignmentOperator::ShiftRight,
        "&=" => AssignmentOperator::BitwiseAnd,
        "|=" => AssignmentOperator::BitwiseOR,
        "&&=" => AssignmentOperator::LogicalAnd,
        "||=" => AssignmentOperator::LogicalOr,
        _ => return None,
    })
}

fn template<'a>(ast: AstBuilder<'a>, span: Span, raw: &str) -> TemplateLiteral<'a> {
    let value = TemplateElementValue {
        raw: ast.str(raw),
        cooked: Some(ast.str(raw)),
    };
    ast.template_literal(
        span,
        ast.vec1(ast.template_element(span, value, true, false)),
        ast.vec(),
    )
}

fn unparenthesized<'r, 'a>(expression: &'r Expression<'a>) -> &'r Expression<'a> {
    match expression {
        Expression::ParenthesizedExpression(inner) => unparenthesized(&inner.expression),
        expression => expression,
    }
}

/// `mutant`'s replacement for `expression`, the node it is on (a chain when
/// the node is the chain's last link), as StrykerJS's mutator builds it.
fn expression_replacement<'a>(
    ast: AstBuilder<'a>,
    expression: &Expression<'a>,
    mutant: &LedgerMutant,
) -> Option<Expression<'a>> {
    let allocator = ast.allocator;
    let span = expression.span();
    let descriptor = mutant.replacement.as_str();
    if let Expression::ChainExpression(chain) = expression {
        return chain_replacement(ast, chain.span, &chain.expression, mutant);
    }
    let replacement = match (mutant.mutator, expression) {
        ("ArithmeticOperator" | "EqualityOperator", Expression::BinaryExpression(binary)) => {
            let mut clone = binary.clone_in(allocator);
            clone.operator = binary_operator(descriptor)?;
            Expression::BinaryExpression(clone)
        }
        ("LogicalOperator", Expression::LogicalExpression(logical)) => {
            let mut clone = logical.clone_in(allocator);
            clone.operator = match descriptor {
                "&&" => LogicalOperator::And,
                _ => LogicalOperator::Or,
            };
            Expression::LogicalExpression(clone)
        }
        ("AssignmentOperator", Expression::AssignmentExpression(assignment)) => {
            let mut clone = assignment.clone_in(allocator);
            clone.operator = assignment_operator(descriptor)?;
            Expression::AssignmentExpression(clone)
        }
        ("UpdateOperator", Expression::UpdateExpression(update)) => {
            let mut clone = update.clone_in(allocator);
            clone.operator = if descriptor == "++" {
                UpdateOperator::Increment
            } else {
                UpdateOperator::Decrement
            };
            Expression::UpdateExpression(clone)
        }
        ("UnaryOperator", Expression::UnaryExpression(unary)) => {
            let argument = unary.argument.clone_in(allocator);
            match descriptor {
                "+" => ast.expression_unary(span, UnaryOperator::UnaryPlus, argument),
                "-" => ast.expression_unary(span, UnaryOperator::UnaryNegation, argument),
                _ => argument,
            }
        }
        ("BooleanLiteral", Expression::BooleanLiteral(literal)) => {
            ast.expression_boolean_literal(span, !literal.value)
        }
        ("BooleanLiteral", Expression::UnaryExpression(unary)) => {
            unary.argument.clone_in(allocator)
        }
        ("ConditionalExpression", _) => ast.expression_boolean_literal(span, descriptor == "true"),
        ("StringLiteral", Expression::TemplateLiteral(_)) => {
            let raw = if descriptor == "empty" {
                ""
            } else {
                "Stryker was here!"
            };
            Expression::TemplateLiteral(ast.alloc(template(ast, span, raw)))
        }
        ("StringLiteral", Expression::StringLiteral(_)) => {
            let value = if descriptor == "empty" {
                ""
            } else {
                "Stryker was here!"
            };
            ast.expression_string_literal(span, ast.str(value), None)
        }
        ("MethodExpression", Expression::CallExpression(call)) => {
            method_replacement(ast, span, call, descriptor)?
        }
        ("OptionalChaining", expression) => {
            let mut clone = expression.clone_in(allocator);
            match &mut clone {
                Expression::StaticMemberExpression(member) => member.optional = false,
                Expression::ComputedMemberExpression(member) => member.optional = false,
                Expression::PrivateFieldExpression(member) => member.optional = false,
                Expression::CallExpression(call) => call.optional = false,
                _ => return None,
            }
            clone
        }
        ("ArrowFunction", Expression::ArrowFunctionExpression(_)) => {
            let parameters = ast.alloc_formal_parameters(
                span,
                FormalParameterKind::ArrowFormalParameters,
                ast.vec(),
                NONE,
            );
            let body = ast.alloc_function_body(
                span,
                ast.vec(),
                ast.vec1(ast.statement_expression(
                    span,
                    ast.expression_identifier(span, ast.ident("undefined")),
                )),
            );
            ast.expression_arrow_function(span, true, false, NONE, parameters, NONE, body)
        }
        ("ObjectLiteral", Expression::ObjectExpression(_)) => {
            ast.expression_object(span, ast.vec())
        }
        ("ArrayDeclaration", Expression::ArrayExpression(_)) => {
            let elements = if descriptor == "[]" {
                ast.vec()
            } else {
                ast.vec1(
                    ast.expression_string_literal(span, ast.str("Stryker was here"), None)
                        .into(),
                )
            };
            ast.expression_array(span, elements)
        }
        ("ArrayDeclaration", Expression::CallExpression(call)) => {
            let arguments = array_arguments(ast, span, descriptor);
            let callee = unparenthesized(&call.callee).clone_in(allocator);
            ast.expression_call(span, callee, NONE, arguments, false)
        }
        ("ArrayDeclaration", Expression::NewExpression(new)) => {
            let arguments = array_arguments(ast, span, descriptor);
            let callee = unparenthesized(&new.callee).clone_in(allocator);
            ast.expression_new(span, callee, NONE, arguments)
        }
        _ => return None,
    };
    Some(replacement)
}

fn array_arguments<'a>(
    ast: AstBuilder<'a>,
    span: Span,
    descriptor: &str,
) -> oxc_allocator::Vec<'a, Argument<'a>> {
    if descriptor.ends_with("([])") {
        ast.vec1(Argument::from(ast.expression_array(span, ast.vec())))
    } else {
        ast.vec()
    }
}

/// `x.trim()` to `x`, or `x.toLowerCase()` to `x.toUpperCase()`.
fn method_replacement<'a>(
    ast: AstBuilder<'a>,
    span: Span,
    call: &oxc_ast::ast::CallExpression<'a>,
    descriptor: &str,
) -> Option<Expression<'a>> {
    let allocator = ast.allocator;
    let (object, optional) = match unparenthesized(&call.callee) {
        Expression::StaticMemberExpression(member) => (&member.object, member.optional),
        Expression::ComputedMemberExpression(member) => (&member.object, member.optional),
        _ => return None,
    };
    if descriptor == "removed" {
        return Some(unparenthesized(object).clone_in(allocator));
    }
    let callee = Expression::StaticMemberExpression(ast.alloc_static_member_expression(
        span,
        object.clone_in(allocator),
        ast.identifier_name(span, ast.ident(descriptor)),
        optional,
    ));
    Some(ast.expression_call(
        span,
        callee,
        NONE,
        call.arguments.clone_in(allocator),
        call.optional,
    ))
}

/// The replacement of a chain's last link: a chain whose last link it is,
/// or, where it is no link (`text?.trim()` to `text`, or `true`), itself.
fn chain_replacement<'a>(
    ast: AstBuilder<'a>,
    span: Span,
    element: &ChainElement<'a>,
    mutant: &LedgerMutant,
) -> Option<Expression<'a>> {
    let allocator = ast.allocator;
    let expression = match element {
        ChainElement::CallExpression(call) => Expression::CallExpression(call.clone_in(allocator)),
        ChainElement::StaticMemberExpression(member) => {
            Expression::StaticMemberExpression(member.clone_in(allocator))
        }
        ChainElement::ComputedMemberExpression(member) => {
            Expression::ComputedMemberExpression(member.clone_in(allocator))
        }
        ChainElement::PrivateFieldExpression(member) => {
            Expression::PrivateFieldExpression(member.clone_in(allocator))
        }
        ChainElement::TSNonNullExpression(non_null) => {
            Expression::TSNonNullExpression(non_null.clone_in(allocator))
        }
    };
    let replaced = expression_replacement(ast, &expression, mutant)?;
    let element = match replaced {
        Expression::CallExpression(call) => ChainElement::CallExpression(call),
        Expression::StaticMemberExpression(member) => ChainElement::StaticMemberExpression(member),
        Expression::ComputedMemberExpression(member) => {
            ChainElement::ComputedMemberExpression(member)
        }
        Expression::PrivateFieldExpression(member) => ChainElement::PrivateFieldExpression(member),
        Expression::TSNonNullExpression(non_null) => ChainElement::TSNonNullExpression(non_null),
        other => return Some(other),
    };
    Some(ast.expression_chain(span, element))
}

/// Whether `expression`, a chain's last link, is the mutant's node.
fn expression_matches(expression: &Expression<'_>, mutant: &LedgerMutant) -> bool {
    let span = expression.span();
    if span != mutant.span {
        return false;
    }
    match expression {
        Expression::ChainExpression(chain) => accepts(mutant, chain_kind(&chain.expression)),
        expression => accepts(mutant, expression_kind(expression)),
    }
}

fn statement_matches(statement: &Statement<'_>, mutant: &LedgerMutant) -> bool {
    statement.span() == mutant.span && accepts(mutant, statement_kind(statement))
}

fn statement_replacement<'a>(
    ast: AstBuilder<'a>,
    statement: &Statement<'a>,
    mutant: &LedgerMutant,
) -> Option<Statement<'a>> {
    let span = statement.span();
    Some(match (mutant.mutator, statement) {
        ("CallExpression", _) => ast.statement_empty(span),
        ("BlockStatement", Statement::BlockStatement(_)) => ast.statement_block(span, ast.vec()),
        ("ConditionalExpression", Statement::ForStatement(for_statement)) => {
            let mut clone = for_statement.clone_in(ast.allocator);
            clone.test = Some(ast.expression_boolean_literal(span, false));
            Statement::ForStatement(clone)
        }
        _ => return None,
    })
}

// ---- finding a mutant's node ----------------------------------------------

/// Builds each mutant's replacement on the program as written, in order.
#[cfg(test)]
struct Builder<'p, 'a> {
    ast: AstBuilder<'a>,
    mutants: &'p [LedgerMutant],
    pending: HashMap<(u32, u32), Vec<usize>>,
    built: Vec<Option<Replacement<'a>>>,
}

#[cfg(test)]
impl<'a> Builder<'_, 'a> {
    fn waiting(&mut self, span: Span) -> Vec<usize> {
        self.pending
            .get(&(span.start, span.end))
            .cloned()
            .unwrap_or_default()
    }

    fn done(&mut self, span: Span, index: usize) {
        if let Some(list) = self.pending.get_mut(&(span.start, span.end)) {
            list.retain(|candidate| *candidate != index);
        }
    }
}

#[cfg(test)]
impl<'a> Visit<'a> for Builder<'_, 'a> {
    fn visit_expression(&mut self, expression: &Expression<'a>) {
        for index in self.waiting(expression.span()) {
            let mutant = &self.mutants[index];
            if expression_matches(expression, mutant) {
                self.built[index] = expression_replacement(self.ast, expression, mutant)
                    .map(Replacement::Expression);
                self.done(expression.span(), index);
            }
        }
        walk::walk_expression(self, expression);
    }

    fn visit_statement(&mut self, statement: &Statement<'a>) {
        for index in self.waiting(statement.span()) {
            let mutant = &self.mutants[index];
            if statement_matches(statement, mutant) {
                self.built[index] =
                    statement_replacement(self.ast, statement, mutant).map(Replacement::Statement);
                self.done(statement.span(), index);
            }
        }
        walk::walk_statement(self, statement);
    }

    fn visit_block_statement(&mut self, block: &oxc_ast::ast::BlockStatement<'a>) {
        for index in self.waiting(block.span) {
            if accepts(&self.mutants[index], NodeKind::Block) {
                self.built[index] = Some(Replacement::Statement(
                    self.ast.statement_block(block.span, self.ast.vec()),
                ));
                self.done(block.span, index);
            }
        }
        walk::walk_block_statement(self, block);
    }

    fn visit_function_body(&mut self, body: &FunctionBody<'a>) {
        for index in self.waiting(body.span) {
            if accepts(&self.mutants[index], NodeKind::FunctionBody) {
                self.built[index] = Some(Replacement::EmptyBody);
                self.done(body.span, index);
            }
        }
        walk::walk_function_body(self, body);
    }

    fn visit_switch_case(&mut self, case: &SwitchCase<'a>) {
        for index in self.waiting(case.span) {
            if accepts(&self.mutants[index], NodeKind::SwitchCase) {
                let mut clone = case.clone_in(self.ast.allocator);
                clone.consequent.clear();
                self.built[index] = Some(Replacement::Case(clone));
                self.done(case.span, index);
            }
        }
        walk::walk_switch_case(self, case);
    }

    fn visit_tagged_template_expression(
        &mut self,
        tagged: &oxc_ast::ast::TaggedTemplateExpression<'a>,
    ) {
        let span = tagged.quasi.span;
        for index in self.waiting(span) {
            let mutant = &self.mutants[index];
            if mutant.mutator == "StringLiteral" {
                let raw = if mutant.replacement == "empty" {
                    ""
                } else {
                    "Stryker was here!"
                };
                self.built[index] = Some(Replacement::Quasi(template(self.ast, span, raw)));
                self.done(span, index);
            }
        }
        walk::walk_tagged_template_expression(self, tagged);
    }
}

#[cfg(test)]
fn replacements<'a>(
    allocator: &'a Allocator,
    program: &oxc_ast::ast::Program<'a>,
    mutants: &[LedgerMutant],
) -> Vec<Option<Replacement<'a>>> {
    let mut pending = HashMap::<(u32, u32), Vec<usize>>::new();
    for (index, mutant) in mutants.iter().enumerate() {
        pending
            .entry((mutant.span.start, mutant.span.end))
            .or_default()
            .push(index);
    }
    let mut builder = Builder {
        ast: AstBuilder::new(allocator),
        mutants,
        pending,
        built: (0..mutants.len()).map(|_| None).collect(),
    };
    builder.visit_program(program);
    builder.built
}

/// Each mutant's replacement code as StrykerJS prints it (`text` in the
/// parity goldens), on the program as written. `None` where Supercov builds
/// no replacement.
#[cfg(test)]
pub(crate) fn replacement_texts(
    allocator: &Allocator,
    program: &oxc_ast::ast::Program<'_>,
    source: &str,
    mutants: &[LedgerMutant],
) -> Vec<Option<String>> {
    // The replacements borrow the program's allocator; a copy of the
    // program in `allocator` gives them one to live in.
    let program = program.clone_in(allocator);
    replacements(allocator, &program, mutants)
        .into_iter()
        .map(|replacement| {
            replacement.map(|replacement| match replacement {
                Replacement::Expression(expression) => {
                    crate::js_babel_print::expression(source, &expression)
                }
                Replacement::Statement(statement) => {
                    crate::js_babel_print::statement(source, &statement)
                }
                Replacement::EmptyBody => "{}".to_owned(),
                Replacement::Case(case) => crate::js_babel_print::switch_case(source, &case),
                Replacement::Quasi(quasi) => crate::js_babel_print::expression(
                    source,
                    &Expression::TemplateLiteral(oxc_allocator::Box::new_in(quasi, allocator)),
                ),
            })
        })
        .collect()
}

// ---- writing the switches ------------------------------------------------

/// The test of mutant `index`'s switch: `record.a === index`.
fn switch_test<'a>(ast: AstBuilder<'a>, record: &str, index: usize) -> Expression<'a> {
    let active = Expression::StaticMemberExpression(ast.alloc_static_member_expression(
        Span::default(),
        ast.expression_identifier(Span::default(), ast.ident(record)),
        ast.identifier_name(Span::default(), ast.ident("a")),
        false,
    ));
    ast.expression_binary(
        Span::default(),
        active,
        BinaryOperator::StrictEquality,
        ast.expression_numeric_literal(
            Span::default(),
            index as f64,
            Some(ast.str(&index.to_string())),
            NumberBase::Decimal,
        ),
    )
}

/// `record.h()`, which counts an evaluation of the active mutant.
fn count<'a>(ast: AstBuilder<'a>, record: &str) -> Expression<'a> {
    let callee = Expression::StaticMemberExpression(ast.alloc_static_member_expression(
        Span::default(),
        ast.expression_identifier(Span::default(), ast.ident(record)),
        ast.identifier_name(Span::default(), ast.ident("h")),
        false,
    ));
    ast.expression_call(Span::default(), callee, NONE, ast.vec(), false)
}

/// Whether `test` is a switch's test.
fn is_switch_test(test: &Expression<'_>, record: &str) -> bool {
    let Expression::BinaryExpression(binary) = test else {
        return false;
    };
    binary.operator == BinaryOperator::StrictEquality
        && matches!(&binary.right, Expression::NumericLiteral(_))
        && matches!(
            &binary.left,
            Expression::StaticMemberExpression(member)
                if member.property.name == "a"
                    && matches!(&member.object, Expression::Identifier(object) if object.name == record)
        )
}

fn is_switch_if(statement: &Statement<'_>, record: &str) -> bool {
    matches!(statement, Statement::IfStatement(statement) if is_switch_test(&statement.test, record))
}

/// The original statements at the end of a switch's `else` chain.
fn final_else<'a>(
    statement: &mut Statement<'a>,
    ast: AstBuilder<'a>,
    record: &str,
) -> oxc_allocator::Vec<'a, Statement<'a>> {
    let mut current = statement.take_in(ast.allocator);
    loop {
        match current {
            Statement::IfStatement(mut statement) if is_switch_test(&statement.test, record) => {
                match statement.alternate.take() {
                    Some(alternate) => current = alternate,
                    None => return ast.vec(),
                }
            }
            Statement::BlockStatement(mut block) => {
                return block.body.take_in(ast.allocator);
            }
            other => return ast.vec1(other),
        }
    }
}

/// Takes the switches out of a copy: each placed on a node that does not
/// end before `cutoff` becomes its original again.
struct Unplace<'a, 'r> {
    ast: AstBuilder<'a>,
    record: &'r str,
    cutoff: u32,
}

impl<'a> VisitMut<'a> for Unplace<'a, '_> {
    fn visit_expression(&mut self, expression: &mut Expression<'a>) {
        while let Expression::ConditionalExpression(conditional) = expression {
            if conditional.span.end <= self.cutoff
                || !is_switch_test(&conditional.test, self.record)
            {
                break;
            }
            let original = conditional.alternate.take_in(self.ast.allocator);
            *expression = original;
        }
        walk_mut::walk_expression(self, expression);
    }

    fn visit_statement(&mut self, statement: &mut Statement<'a>) {
        loop {
            let span = statement.span();
            if span.end <= self.cutoff {
                break;
            }
            match statement {
                Statement::BlockStatement(block)
                    if block.body.len() == 1
                        && is_switch_if(&block.body[0], self.record)
                        && block.body[0].span() == block.span =>
                {
                    let mut inner = block.body.pop().expect("one statement");
                    let mut original = final_else(&mut inner, self.ast, self.record);
                    if original.len() == 1 {
                        *statement = original.pop().expect("one statement");
                    } else {
                        *statement = self.ast.statement_block(span, original);
                    }
                }
                Statement::IfStatement(_) if is_switch_if(statement, self.record) => {
                    let mut original = final_else(statement, self.ast, self.record);
                    *statement = if original.len() == 1 {
                        original.pop().expect("one statement")
                    } else {
                        self.ast.statement_block(span, original)
                    };
                }
                _ => break,
            }
        }
        walk_mut::walk_statement(self, statement);
    }

    fn visit_function_body(&mut self, body: &mut FunctionBody<'a>) {
        if body.span.end > self.cutoff
            && body.statements.len() == 1
            && is_switch_if(&body.statements[0], self.record)
            && body.statements[0].span() == body.span
        {
            let mut inner = body.statements.pop().expect("one statement");
            body.statements = final_else(&mut inner, self.ast, self.record);
        }
        walk_mut::walk_function_body(self, body);
    }

    fn visit_switch_case(&mut self, case: &mut SwitchCase<'a>) {
        if case.span.end > self.cutoff
            && case.consequent.len() == 1
            && is_switch_if(&case.consequent[0], self.record)
            && case.consequent[0].span() == case.span
        {
            let mut inner = case.consequent.pop().expect("one statement");
            case.consequent = final_else(&mut inner, self.ast, self.record);
        }
        walk_mut::walk_switch_case(self, case);
    }
}

/// Puts a mutant's replacement in place of its node, in a copy of the
/// ancestor its switch is on.
struct Apply<'p, 'a> {
    ast: AstBuilder<'a>,
    mutant: &'p LedgerMutant,
    applied: bool,
}

impl<'a> VisitMut<'a> for Apply<'_, 'a> {
    fn visit_expression(&mut self, expression: &mut Expression<'a>) {
        if self.applied {
            return;
        }
        if expression_matches(expression, self.mutant)
            && let Some(replacement) = expression_replacement(self.ast, expression, self.mutant)
        {
            *expression = replacement;
            self.applied = true;
            return;
        }
        walk_mut::walk_expression(self, expression);
    }

    fn visit_statement(&mut self, statement: &mut Statement<'a>) {
        if self.applied {
            return;
        }
        if statement_matches(statement, self.mutant)
            && let Some(replacement) = statement_replacement(self.ast, statement, self.mutant)
        {
            *statement = replacement;
            self.applied = true;
            return;
        }
        walk_mut::walk_statement(self, statement);
    }

    fn visit_block_statement(&mut self, block: &mut oxc_ast::ast::BlockStatement<'a>) {
        if self.applied {
            return;
        }
        if block.span == self.mutant.span && accepts(self.mutant, NodeKind::Block) {
            block.body.clear();
            self.applied = true;
            return;
        }
        walk_mut::walk_block_statement(self, block);
    }

    fn visit_function_body(&mut self, body: &mut FunctionBody<'a>) {
        if self.applied {
            return;
        }
        if body.span == self.mutant.span && accepts(self.mutant, NodeKind::FunctionBody) {
            body.statements.clear();
            body.directives.clear();
            self.applied = true;
            return;
        }
        walk_mut::walk_function_body(self, body);
    }

    fn visit_switch_case(&mut self, case: &mut SwitchCase<'a>) {
        if self.applied {
            return;
        }
        if case.span == self.mutant.span && accepts(self.mutant, NodeKind::SwitchCase) {
            case.consequent.clear();
            self.applied = true;
            return;
        }
        walk_mut::walk_switch_case(self, case);
    }

    fn visit_tagged_template_expression(
        &mut self,
        tagged: &mut oxc_ast::ast::TaggedTemplateExpression<'a>,
    ) {
        if self.applied {
            return;
        }
        if tagged.quasi.span == self.mutant.span && self.mutant.mutator == "StringLiteral" {
            let raw = if self.mutant.replacement == "empty" {
                ""
            } else {
                "Stryker was here!"
            };
            tagged.quasi = template(self.ast, tagged.quasi.span, raw);
            self.applied = true;
            return;
        }
        walk_mut::walk_tagged_template_expression(self, tagged);
    }
}

/// `({ "name": value })["name"]`: `value`, named as its place names it.
fn named<'a>(ast: AstBuilder<'a>, value: Expression<'a>, name: &str) -> Expression<'a> {
    let span = value.span();
    let key = || ast.expression_string_literal(Span::default(), ast.str(name), None);
    let property = ast.object_property_kind_object_property(
        Span::default(),
        PropertyKind::Init,
        PropertyKey::from(key()),
        value,
        false,
        false,
        false,
    );
    let object = ast.expression_parenthesized(
        Span::default(),
        ast.expression_object(Span::default(), ast.vec1(property)),
    );
    Expression::ComputedMemberExpression(ast.alloc_computed_member_expression(
        span,
        object,
        key(),
        false,
    ))
}

fn anonymous(expression: &Expression<'_>) -> bool {
    match unparenthesized(expression) {
        Expression::ArrowFunctionExpression(_) => true,
        Expression::FunctionExpression(function) => function.id.is_none(),
        Expression::ClassExpression(class) => class.id.is_none(),
        _ => false,
    }
}

/// A node a switch can go on, visited from itself.
trait Visitable<'a>: CloneIn<'a, Cloned = Self> + Sized {
    fn accept<V: VisitMut<'a>>(&mut self, visitor: &mut V);
}

impl<'a> Visitable<'a> for Expression<'a> {
    fn accept<V: VisitMut<'a>>(&mut self, visitor: &mut V) {
        visitor.visit_expression(self);
    }
}

impl<'a> Visitable<'a> for Statement<'a> {
    fn accept<V: VisitMut<'a>>(&mut self, visitor: &mut V) {
        visitor.visit_statement(self);
    }
}

impl<'a> Visitable<'a> for FunctionBody<'a> {
    fn accept<V: VisitMut<'a>>(&mut self, visitor: &mut V) {
        visitor.visit_function_body(self);
    }
}

impl<'a> Visitable<'a> for SwitchCase<'a> {
    fn accept<V: VisitMut<'a>>(&mut self, visitor: &mut V) {
        visitor.visit_switch_case(self);
    }
}

struct Switches<'p, 'a> {
    ast: AstBuilder<'a>,
    record: &'p str,
    mutants: &'p [LedgerMutant],
    plans: &'p [MutantPlan],
    /// By the node a switch goes on: the mutants, in order.
    places: HashMap<PlaceKey, Vec<usize>>,
}

impl<'a> Switches<'_, 'a> {
    /// A copy of `node`, a switch's place, with mutant `index` applied as
    /// StrykerJS applies it.
    fn applied<T: Visitable<'a>>(&self, node: &T, index: usize) -> Option<T> {
        let mutant = &self.mutants[index];
        let plan = &self.plans[index];
        let mut copy = node.clone_in(self.ast.allocator);
        let itself = plan
            .placement
            .as_ref()
            .is_ok_and(|placement| placement.itself);
        let mut unplace = Unplace {
            ast: self.ast,
            record: self.record,
            cutoff: if itself { 0 } else { mutant.span.start },
        };
        copy.accept(&mut unplace);
        let mut apply = Apply {
            ast: self.ast,
            mutant,
            applied: false,
        };
        copy.accept(&mut apply);
        apply.applied.then_some(copy)
    }

    fn place_expression(&self, expression: &mut Expression<'a>, mutants: &[usize]) {
        let span = expression.span();
        let name = mutants
            .first()
            .and_then(|index| self.plans[*index].placement.as_ref().ok())
            .and_then(|placement| placement.name.clone());
        let branches = mutants
            .iter()
            .filter_map(|index| {
                self.applied(expression, *index)
                    .map(|applied| (*index, applied))
            })
            .collect::<Vec<_>>();
        let mut result = expression.take_in(self.ast.allocator);
        if let Some(name) = &name
            && anonymous(&result)
        {
            result = named(self.ast, result, name);
        }
        for (index, mut applied) in branches.into_iter().rev() {
            if let Some(name) = &name
                && anonymous(&applied)
            {
                applied = named(self.ast, applied, name);
            }
            let counted = self.ast.expression_sequence(
                Span::default(),
                self.ast
                    .vec_from_array([count(self.ast, self.record), applied]),
            );
            result = self.ast.expression_conditional(
                span,
                switch_test(self.ast, self.record, index),
                self.ast.expression_parenthesized(Span::default(), counted),
                result,
            );
        }
        *expression = result;
    }

    /// `if (a === i) { h(); <applied> } else if ... else { <original> }`.
    fn switch_if(
        &self,
        span: Span,
        branches: Vec<(usize, oxc_allocator::Vec<'a, Statement<'a>>)>,
        original: oxc_allocator::Vec<'a, Statement<'a>>,
    ) -> Statement<'a> {
        let ast = self.ast;
        let mut result = ast.statement_block(Span::default(), original);
        for (index, applied) in branches.into_iter().rev() {
            let mut body =
                ast.vec1(ast.statement_expression(Span::default(), count(ast, self.record)));
            body.extend(applied);
            result = ast.statement_if(
                span,
                switch_test(ast, self.record, index),
                ast.statement_block(Span::default(), body),
                Some(result),
            );
        }
        result
    }

    fn place_statement(&self, statement: &mut Statement<'a>, mutants: &[usize]) {
        let span = statement.span();
        let branches = mutants
            .iter()
            .filter_map(|index| {
                self.applied(statement, *index)
                    .map(|applied| (*index, self.ast.vec1(applied)))
            })
            .collect::<Vec<_>>();
        let block = matches!(statement, Statement::BlockStatement(_));
        let original = statement.take_in(self.ast.allocator);
        let switch = self.switch_if(span, branches, self.ast.vec1(original));
        *statement = if block {
            self.ast.statement_block(span, self.ast.vec1(switch))
        } else {
            switch
        };
    }

    fn place_body(&self, body: &mut FunctionBody<'a>, mutants: &[usize]) {
        let branches = mutants
            .iter()
            .filter_map(|index| {
                self.applied(body, *index).map(|applied| {
                    let applied = applied.statements;
                    (
                        *index,
                        self.ast
                            .vec1(self.ast.statement_block(Span::default(), applied)),
                    )
                })
            })
            .collect::<Vec<_>>();
        let original = body.statements.take_in(self.ast.allocator);
        body.statements = self.ast.vec1(self.switch_if(body.span, branches, original));
    }

    fn place_case(&self, case: &mut SwitchCase<'a>, mutants: &[usize]) {
        let branches = mutants
            .iter()
            .filter_map(|index| {
                self.applied(case, *index)
                    .map(|applied| (*index, applied.consequent))
            })
            .collect::<Vec<_>>();
        let original = case.consequent.take_in(self.ast.allocator);
        case.consequent = self.ast.vec1(self.switch_if(case.span, branches, original));
    }
}

impl<'a> VisitMut<'a> for Switches<'_, 'a> {
    fn visit_expression(&mut self, expression: &mut Expression<'a>) {
        let key = PlaceKey::Node(expression.address());
        walk_mut::walk_expression(self, expression);
        if let Some(mutants) = self.places.remove(&key) {
            self.place_expression(expression, &mutants);
        }
    }

    fn visit_statement(&mut self, statement: &mut Statement<'a>) {
        let key = PlaceKey::Node(statement.address());
        walk_mut::walk_statement(self, statement);
        if let Some(mutants) = self.places.remove(&key) {
            self.place_statement(statement, &mutants);
        }
    }

    fn visit_function_body(&mut self, body: &mut FunctionBody<'a>) {
        let key = PlaceKey::Body(body.span.start, body.span.end);
        walk_mut::walk_function_body(self, body);
        if let Some(mutants) = self.places.remove(&key) {
            self.place_body(body, &mutants);
        }
    }

    fn visit_switch_case(&mut self, case: &mut SwitchCase<'a>) {
        let key = PlaceKey::Case(case.span.start, case.span.end);
        walk_mut::walk_switch_case(self, case);
        if let Some(mutants) = self.places.remove(&key) {
            self.place_case(case, &mutants);
        }
    }
}

/// Writes `mutants`, the file's listed mutants, into `program` behind
/// switches that read `record`, and says of each whether it is static and
/// why it has no switch where it has none. `kept` are the spans of the
/// functions the instrumenter writes as they were.
pub(crate) fn place<'a>(
    allocator: &'a Allocator,
    program: &mut oxc_ast::ast::Program<'a>,
    mutants: &[LedgerMutant],
    record: &str,
    kept: &HashSet<(u32, u32)>,
) -> Vec<StrongMutant> {
    let plans = plans(program, mutants, kept);
    let mut places = HashMap::<PlaceKey, Vec<usize>>::new();
    for (index, plan) in plans.iter().enumerate() {
        if let Ok(placement) = &plan.placement {
            places.entry(placement.key).or_default().push(index);
        }
    }
    let mut switches = Switches {
        ast: AstBuilder::new(allocator),
        record,
        mutants,
        plans: &plans,
        places,
    };
    switches.visit_program(program);
    plans
        .iter()
        .map(|plan| StrongMutant {
            is_static: plan.is_static,
            unplaceable: plan.placement.as_ref().err().copied(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use oxc_parser::Parser;
    use serde_json::Value;

    use super::*;
    use crate::js_instrumenter::{
        CandidateOutput, STRONG_FOR_TEST, instrument_candidate, project_source_type,
    };

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/stryker-parity");

    fn golden(name: &str) -> Vec<Value> {
        let path = Path::new(FIXTURES).join(format!("{name}.stryker.json"));
        let golden: Value =
            serde_json::from_str(&fs::read_to_string(path).expect("a golden")).expect("JSON");
        golden["mutants"].as_array().expect("mutants").clone()
    }

    fn pair(value: &Value) -> (u32, u32) {
        (
            value[0].as_u64().expect("a line") as u32,
            value[1].as_u64().expect("a column") as u32,
        )
    }

    fn fixtures() -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(FIXTURES)
            .expect("the parity fixtures")
            .map(|entry| {
                entry
                    .expect("a fixture")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .filter(|name| {
                Path::new(name).extension().is_some_and(|extension| {
                    ["ts", "tsx", "js", "mjs"].contains(&extension.to_str().unwrap_or(""))
                })
            })
            .collect();
        names.sort();
        names
    }

    fn strong(source: &str, file: &str) -> CandidateOutput {
        STRONG_FOR_TEST.with(|flag| flag.set(Some(true)));
        let output = instrument_candidate(source, file);
        STRONG_FOR_TEST.with(|flag| flag.set(None));
        output.unwrap_or_else(|error| panic!("{file}: {error}"))
    }

    fn reparses(output: &CandidateOutput, file: &str) {
        let allocator = Allocator::default();
        let source_type = project_source_type(Path::new(file)).unwrap();
        let parsed = Parser::new(&allocator, &output.code, source_type).parse();
        assert!(
            parsed.errors.is_empty(),
            "{file}: {:?}\n{}",
            parsed.errors,
            output.code
        );
    }

    /// Every mutant of every parity fixture gets the replacement code
    /// StrykerJS prints for it, byte for byte.
    #[test]
    fn replacements_are_strykers_on_every_parity_fixture() {
        let mut compared = 0;
        let mut mismatches = Vec::new();
        for name in fixtures() {
            let source = fs::read_to_string(Path::new(FIXTURES).join(&name)).unwrap();
            let allocator = Allocator::default();
            let source_type = project_source_type(Path::new(&name)).unwrap();
            let parsed = Parser::new(&allocator, &source, source_type).parse();
            let mutants = crate::js_mutants::enumerate(&parsed.program, &source);
            let golden = golden(&name);
            assert_eq!(
                mutants.len(),
                golden.len(),
                "{name}: the enumeration's parity"
            );
            let texts = replacement_texts(&allocator, &parsed.program, &source, &mutants);
            for (index, ((mutant, text), entry)) in
                mutants.iter().zip(&texts).zip(&golden).enumerate()
            {
                assert_eq!(
                    (
                        mutant.mutator,
                        mutant.start,
                        mutant.end,
                        mutant.replacement.as_str()
                    ),
                    (
                        entry["mutator"].as_str().unwrap(),
                        pair(&entry["start"]),
                        pair(&entry["end"]),
                        entry["replacement"].as_str().unwrap()
                    ),
                    "{name} #{index}"
                );
                compared += 1;
                let expected = entry["text"].as_str().unwrap();
                if text.as_deref() != Some(expected) {
                    mismatches.push(format!(
                        "{name} #{index} {}:{}:{} {}: StrykerJS {expected:?}, Supercov {text:?}",
                        mutant.start.0, mutant.start.1, mutant.mutator, mutant.replacement
                    ));
                }
            }
        }
        assert!(compared >= 600, "{compared} mutants compared");
        assert!(
            mismatches.is_empty(),
            "{} of {compared} replacements differ from StrykerJS's:\n{}",
            mismatches.len(),
            mismatches.join("\n")
        );
    }

    /// Every parity fixture instrumented in strong mode parses, lists the
    /// weak enumeration's mutants in its order, and places all of them.
    #[test]
    fn every_parity_fixture_is_switched_into_code_that_parses() {
        for name in fixtures() {
            let source = fs::read_to_string(Path::new(FIXTURES).join(&name)).unwrap();
            let output = strong(&source, &name);
            reparses(&output, &name);
            let golden = golden(&name);
            assert_eq!(output.mutants.len(), golden.len(), "{name}");
            let unplaced = output
                .mutants
                .iter()
                .filter(|mutant| mutant.unplaceable.is_some())
                .map(|mutant| format!("{} {:?}", mutant.id, mutant.unplaceable))
                .collect::<Vec<_>>();
            assert!(unplaced.is_empty(), "{name}: {unplaced:#?}");
            assert!(output.points.is_empty() && output.decisions.is_empty());
            assert!(
                output
                    .code
                    .contains(&format!("__supercovStrongSlot1(\"{name}\")")),
                "{name}"
            );
            for index in 0..golden.len() {
                let test = format!("__supercovStrong.a === {index}");
                let switched = output.code.match_indices(&test).any(|(at, _)| {
                    !output.code[at + test.len()..].starts_with(|next: char| next.is_ascii_digit())
                });
                assert!(switched, "{name}: no switch for #{index}");
            }
        }
    }

    #[test]
    fn switches_take_the_documented_forms() {
        let output = strong(
            "export function f(a, b) {\n  if (a > b) g();\n  return a + b;\n}\nexport const h = (x) => x * 2;\n",
            "src/a.js",
        );
        reparses(&output, "src/a.js");
        let code = &output.code;
        assert!(
            code.contains("strongSlot as __supercovStrongSlot1"),
            "{code}"
        );
        assert!(
            code.contains("const __supercovStrong = __supercovStrongSlot1(\"src/a.js\");"),
            "{code}"
        );
        let index = |mutator: &str, descriptor: &str| {
            output
                .mutants
                .iter()
                .position(|mutant| mutant.mutator == mutator && mutant.replacement == descriptor)
                .unwrap_or_else(|| panic!("{mutator} {descriptor}: {:#?}", output.mutants))
        };
        let plus = index("ArithmeticOperator", "-");
        assert!(
            code.contains(&format!(
                "__supercovStrong.a === {plus} ? (__supercovStrong.h(), a - b) :"
            )),
            "{code}"
        );
        let call = index("CallExpression", "empty");
        assert!(
            code.contains(&format!("if (__supercovStrong.a === {call}) {{\n")),
            "{code}"
        );
        // The arrow keeps its name with its switch around it.
        let arrow = index("ArrowFunction", "undefined");
        assert!(code.contains("[\"h\"]"), "{code}");
        let mutant = |index: usize| &output.mutants[index];
        assert!(mutant(arrow).is_static);
        assert!(!mutant(plus).is_static);
        assert!(!mutant(index("ArithmeticOperator", "/")).is_static);
        assert!(
            output
                .mutants
                .iter()
                .all(|mutant| mutant.unplaceable.is_none())
        );
        let serialized = serde_json::to_value(mutant(arrow)).unwrap();
        assert_eq!(serialized["static"], true, "{serialized}");
        assert!(serialized.get("unplaceable").is_none());
    }

    /// Where no ancestor can be wrapped without changing the program, the
    /// mutant is listed with the reason and gets no switch.
    #[test]
    fn a_mutant_with_no_placeable_ancestor_is_listed_as_such() {
        let output = strong(
            "const { [k + 1]: v } = o;\nfunction g() { const { [k - 1]: w } = o; return w; }\nwith (o) { p(1 + 2); }\nouter: for (;;) { break outer; }\n",
            "src/b.cjs",
        );
        reparses(&output, "src/b.cjs");
        let reason = |original: &str| {
            output
                .mutants
                .iter()
                .find(|mutant| mutant.original == original)
                .unwrap_or_else(|| panic!("{original}: {:#?}", output.mutants))
                .unplaceable
                .clone()
        };
        assert_eq!(reason("k + 1").as_deref(), Some(NO_PLACEABLE_ANCESTOR));
        // In a function, the body takes it.
        assert_eq!(reason("k - 1"), None);
        assert_eq!(reason("1 + 2").as_deref(), Some(WITH_STATEMENT));
        // A labelled loop is wrapped with its label.
        assert_eq!(reason("for (;;) { break outer; }"), None);
        assert!(
            output.code.contains("outer: for (; false;)"),
            "{}",
            output.code
        );
    }

    /// Strong mode instruments in place of the probes, and only when asked.
    #[test]
    fn strong_mode_is_off_unless_switched_on() {
        let source = "export const f = (a) => a + 1;\n";
        let measured = instrument_candidate(source, "src/c.js").unwrap();
        assert!(!measured.code.contains("__supercovStrong"));
        assert!(!measured.points.is_empty());
        let switched = strong(source, "src/c.js");
        assert!(switched.code.contains("__supercovStrong"));
        assert!(!switched.code.contains("registerProbeV2"));
    }

    /// The behaviour check: on the parity fixtures Node runs as they are,
    /// running the strong copy with mutant `i` switched on gives what
    /// running StrykerJS's mutated program (its replacement code spliced
    /// into the source) gives, for every mutant, and with no mutant on it
    /// gives what the source gives.
    #[test]
    fn a_switched_on_mutant_behaves_as_strykers_mutated_program() {
        let directory = std::env::temp_dir().join(format!(
            "supercov-strong-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("runtime.mjs"),
            include_str!("../runtime-assets/javascript/runtime.mjs"),
        )
        .unwrap();
        fs::write(directory.join("harness.mjs"), HARNESS).unwrap();
        let mut expected = 0;
        let mut cases = Vec::new();
        for (name, driver) in [
            ("operators.ts", OPERATORS_DRIVER),
            ("module.mjs", MODULE_DRIVER),
        ] {
            let source = fs::read_to_string(Path::new(FIXTURES).join(name)).unwrap();
            let output = strong(&source, name);
            let allocator = Allocator::default();
            let source_type = project_source_type(Path::new(name)).unwrap();
            let parsed = Parser::new(&allocator, &source, source_type).parse();
            let ledger = crate::js_mutants::enumerate(&parsed.program, &source);
            let golden = golden(name);
            assert_eq!(ledger.len(), golden.len());
            let mutants = ledger
                .iter()
                .zip(&golden)
                .enumerate()
                .map(|(index, (mutant, entry))| {
                    let text = entry["text"].as_str().unwrap();
                    let mutated = format!(
                        "{}{text}{}",
                        &source[..mutant.span.start as usize],
                        &source[mutant.span.end as usize..]
                    );
                    serde_json::json!({
                        "index": index,
                        "id": output.mutants[index].id,
                        "source": mutated,
                    })
                })
                .collect::<Vec<_>>();
            expected += mutants.len();
            cases.push(serde_json::json!({
                "file": name,
                "typescript": name.ends_with(".ts"),
                "original": source,
                "strong": output.code,
                "driver": driver,
                "mutants": mutants,
            }));
        }
        let input = directory.join("input.json");
        fs::write(&input, serde_json::to_vec(&cases).unwrap()).unwrap();
        let result = std::process::Command::new("node")
            .arg("--experimental-vm-modules")
            .arg("--no-warnings")
            .arg(directory.join("harness.mjs"))
            .arg(&input)
            .output()
            .expect("Node.js for the behaviour check");
        let _ = fs::remove_dir_all(&directory);
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        let report: Value = serde_json::from_slice(&result.stdout)
            .unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(&result.stdout)));
        let mismatches = report["mismatches"].as_array().unwrap();
        assert!(
            mismatches.is_empty(),
            "{} of {} mutants behave otherwise than StrykerJS's program:\n{}",
            mismatches.len(),
            report["compared"],
            serde_json::to_string_pretty(mismatches).unwrap()
        );
        assert_eq!(report["compared"].as_u64(), Some(expected as u64));
        // Some mutants change what the drivers see, and some loop forever.
        assert!(report["killed"].as_u64().unwrap() > 100, "{report}");
        assert!(report["timeouts"].as_u64().unwrap() > 0, "{report}");
    }

    /// Runs each case's original, its strong copy with no mutant on, and for
    /// each mutant StrykerJS's mutated program beside the strong copy with
    /// that mutant on, each as a fresh module in a fresh context, and prints
    /// what differs.
    const HARNESS: &str = r#"
import vm from "node:vm";
import { readFileSync } from "node:fs";
import * as nodeModule from "node:module";
const runtime = await import(new URL("./runtime.mjs", import.meta.url).href);
runtime.setMutantHitLimit(1e6);
const cases = JSON.parse(readFileSync(process.argv[2], "utf8"));
const strip = (typescript, code) => typescript ? nodeModule.stripTypeScriptTypes(code, { mode: "strip" }) : code;
function classify(error) {
  if (runtime.isMutantTimeout(error) || /timed out/.test(String(error?.message))) return "timeout";
  return `error:${error?.name ?? typeof error}`;
}
async function run(item, code) {
  const context = vm.createContext({});
  vm.runInContext("globalThis.__logs = []", context);
  context.console = { log: (...args) => context.__logs.push(args.map(String).join(" ")), error: (...args) => context.__logs.push("error " + args.map(String).join(" ")) };
  const synthetic = (values) => new vm.SyntheticModule(Object.keys(values), function () {
    for (const [key, value] of Object.entries(values)) this.setExport(key, value);
  }, { context });
  const fixture = new vm.SourceTextModule(code, {
    context,
    identifier: item.file,
    initializeImportMeta(meta) { meta.url = `file:///project/${item.file}`; },
  });
  const driver = new vm.SourceTextModule(`import * as m from "fixture";\n${item.driver}`, { context });
  const link = async (specifier) => {
    if (specifier === "fixture") return fixture;
    if (specifier === "virtual:supercov-runtime") return synthetic({ mcdcBegin: runtime.mcdcBegin, strongSlot: runtime.strongSlot });
    if (specifier === "node:process") return synthetic({ argv: ["node", item.file, "2", "--verbose"] });
    throw new Error(`unexpected import ${specifier}`);
  };
  try {
    await driver.link(link);
    await driver.evaluate({ timeout: 1500 });
  } catch (error) {
    return classify(error);
  }
  return context.__outcome;
}
const mismatches = [];
let compared = 0, killed = 0, timeouts = 0;
for (const item of cases) {
  const strong = strip(item.typescript, item.strong);
  runtime.clearActiveMutant();
  const original = await run(item, strip(item.typescript, item.original));
  const unswitched = await run(item, strong);
  if (typeof original !== "string" || original.startsWith("error") || original === "timeout")
    mismatches.push({ file: item.file, original });
  if (unswitched !== original) mismatches.push({ file: item.file, mutant: null, original, strong: unswitched });
  for (const mutant of item.mutants) {
    runtime.clearActiveMutant();
    let stryker;
    try {
      stryker = await run(item, strip(item.typescript, mutant.source));
    } catch (error) {
      stryker = `error:${error?.name}`;
    }
    runtime.setActiveMutant(item.file, mutant.index);
    const switched = await run(item, strong);
    runtime.clearActiveMutant();
    compared++;
    if (stryker !== original) killed++;
    if (stryker === "timeout") timeouts++;
    if (stryker !== switched) mismatches.push({ file: item.file, index: mutant.index, id: mutant.id, stryker, strong: switched });
  }
}
process.stdout.write(JSON.stringify({ compared, killed, timeouts, mismatches }));
"#;

    const OPERATORS_DRIVER: &str = r#"
const out = [];
const run = (label, call) => {
  try {
    out.push([label, "ok", call()]);
  } catch (error) {
    if (error && error.code === "SUPERCOV_MUTANT_TIMEOUT") throw error;
    out.push([label, "error", error && error.name]);
  }
};
const replacer = (key, value) => {
  if (value === undefined) return "<undefined>";
  if (typeof value === "function") return `<function ${value.name}>`;
  if (typeof value === "bigint") return `${value}n`;
  if (typeof value === "number" && !Number.isFinite(value)) return String(value);
  if (typeof value === "symbol") return value.toString();
  return value;
};
for (const [a, b] of [[0, 0], [1, 2], [3, 4], [-2, 5], [10, 1], [2, 2]]) {
  run("arithmetic", () => m.arithmetic(a, b, "lbl"));
  for (const other of [null, undefined, a, String(a), new Date(0)]) run("equality", () => m.equality(a, other));
}
for (const a of [true, false, 0, 1, 3]) for (const b of [true, false, null]) for (const c of [undefined, "x", "y"]) run("logical", () => m.logical(a, b, c));
for (const [input, name] of [[0, "a"], [7, "bb"], [-3, ""], [100, "x"]]) run("assignment", () => m.assignment(input, name));
for (const values of [[], [0], [1, 2], [5, 0, 3]]) run("unary", () => m.unaryAndUpdate([...values]));
for (const flag of [true, false]) run("booleans", () => m.booleans(flag));
for (const [items, limit] of [[[], 0], [[], 11], [[1], 0], [[1, 2, 3], 5], [[4], 1], [[2, 2], 2], [[0, 5], -1], [[3], 20], [[1], 3]]) run("conditions", () => m.conditions([...items], limit));
for (const kind of ["a", "b", "c", "d", "e", "", "xyz"]) run("switches", () => m.switches(kind));
globalThis.__outcome = JSON.stringify({ out, logs: globalThis.__logs }, replacer);
"#;

    const MODULE_DRIVER: &str = r#"
const out = [];
const run = (label, call) => {
  try {
    out.push([label, "ok", call()]);
  } catch (error) {
    if (error && error.code === "SUPERCOV_MUTANT_TIMEOUT") throw error;
    out.push([label, "error", error && error.name]);
  }
};
const replacer = (key, value) => {
  if (value === undefined) return "<undefined>";
  if (typeof value === "function") return `<function ${value.name}>`;
  if (typeof value === "bigint") return `${value}n`;
  if (typeof value === "number" && !Number.isFinite(value)) return String(value);
  if (typeof value === "symbol") return value.toString();
  return value;
};
run("counter", () => { const counter = new m.Counter(2); const bumped = counter.increment(); return [bumped, counter.count, [...counter], m.Counter.instances]; });
run("counter from zero", () => { const counter = new m.Counter(); return [counter.count, [...counter], counter.increment()]; });
run("exports", () => [m.meta, m.total, m.spread, m.sequence, m.voided, m.deleted, m.typeOf, m.bigint, m.regex]);
run("names", () => [m.lazy.name, typeof m.lazy, m.load.name, m.Counter.name]);
globalThis.__outcome = JSON.stringify({ out, logs: globalThis.__logs }, replacer);
"#;
}
