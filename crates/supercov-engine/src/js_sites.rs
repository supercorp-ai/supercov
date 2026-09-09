//! Effect sites in JavaScript and TypeScript: the part of the asserted-coverage denominator that is not
//! already a decision.
//!
//! Supercov's decisions and their conditions are the decision half of that denominator and are discovered
//! elsewhere. This module finds the other half: the places where a value leaves the enclosing code. A
//! `return`, a `throw`, an outbound call, scheduling, a write to state that is not local, a call through
//! an import, a parameter or an ambient global. Each carries a classification, because a denominator is
//! only honest if what it leaves out is visible:
//!
//! - **contractual**: behavior a test could reasonably be expected to pin.
//! - **incidental**: diagnostics. Logging through a logger abstraction, and anything computed inside a
//!   logging call, is excluded by rule rather than by judgement.
//! - **review**: a site whose nature cannot be settled from syntax, such as a call through an import,
//!   which may be a pure computation or the boundary of the system. Counted separately, never as locked.
//!
//! Sites are *not* coverage obligations. A `return` statement is already a statement obligation; adding
//! it again would change what 100% means. They answer a different question, and the manifest keeps them
//! apart for that reason.
//!
//! Two rules need a type checker, which oxc does not provide: whether a mutator call's receiver is a
//! container (`Map`, `Set`, an array), and whether a method call's receiver has a type declared in this
//! project. Both are declared as limitations rather than guessed at in either direction.

use std::collections::BTreeSet;

use oxc_allocator::Allocator;
use oxc_ast::{
    AstKind,
    ast::{
        AssignmentTarget, BindingPattern, Expression, FunctionType, IdentifierReference,
        SimpleAssignmentTarget, UnaryOperator, UpdateOperator,
    },
};
use oxc_parser::Parser;
use oxc_semantic::{NodeId, Semantic, SemanticBuilder, SymbolId};
use oxc_span::{GetSpan, SourceType, Span};
use serde::{Deserialize, Serialize};

use crate::js_instrumenter::line_and_utf16_column;

/// A method whose call sends something outward: I/O, a process, a socket.
const IO_METHODS: &[&str] = &[
    "write",
    "end",
    "send",
    "json",
    "status",
    "writeHead",
    "setHeader",
    "sendStatus",
    "redirect",
    "emit",
    "kill",
    "listen",
    "close",
    "connect",
    "handleRequest",
    "terminate",
    "destroy",
    "exit",
    "request",
];
/// Free functions with outward effects.
const IO_FUNCTIONS: &[&str] = &["spawn", "exec", "execFile", "fork", "createServer"];
const SCHEDULE: &[&str] = &[
    "setTimeout",
    "setInterval",
    "clearTimeout",
    "clearInterval",
    "setImmediate",
    "queueMicrotask",
];
/// Methods that mutate a container in place.
const MUTATORS: &[&str] = &[
    "set", "delete", "clear", "push", "pop", "shift", "unshift", "splice", "add",
];
/// Ambient globals whose calls compute rather than act, so they are not effect sites.
const PURE_GLOBALS: &[&str] = &[
    "Math",
    "JSON",
    "Object",
    "Array",
    "Number",
    "String",
    "Boolean",
    "Symbol",
    "BigInt",
    "Date",
    "RegExp",
    "Promise",
    "Reflect",
    "Proxy",
    "Intl",
    "Map",
    "Set",
    "WeakMap",
    "WeakSet",
    "WeakRef",
    "Error",
    "TypeError",
    "RangeError",
    "SyntaxError",
    "parseInt",
    "parseFloat",
    "isNaN",
    "isFinite",
    "encodeURIComponent",
    "decodeURIComponent",
    "encodeURI",
    "decodeURI",
    "structuredClone",
    "atob",
    "btoa",
    "URL",
    "URLSearchParams",
    "TextEncoder",
    "TextDecoder",
    "Buffer",
    "Response",
    "Request",
    "Headers",
    "FormData",
    "Blob",
    "ArrayBuffer",
    "Uint8Array",
    "Function",
    "globalThis",
    "undefined",
    "NaN",
    "Infinity",
    "AbortController",
    "AbortSignal",
    "Event",
    "CustomEvent",
    "queueMicrotask",
];
/// Roots whose property, not themselves, names what is being called.
const GLOBAL_ROOTS: &[&str] = &["globalThis", "window", "self", "global"];

/// A mutator call whose receiver could not be confirmed to be a container.
pub const LIMIT_CONTAINER_TYPES: &str = "js-sites-mutator-needs-types";
/// A method call whose receiver could not be confirmed to have a project-declared type.
pub const LIMIT_PROJECT_TYPES: &str = "js-sites-state-call-needs-types";
/// A `this.field.method()` call whose method the enclosing class does not declare. Whether it belongs to
/// a class or to an interface decides whether the call runs code this class owns, and that needs types,
/// so such calls are reported as review sites: never fewer than a type checker would find, sometimes
/// more.
pub const LIMIT_THIS_CALL_TYPES: &str = "js-sites-this-call-needs-types";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SiteCategory {
    IoCall,
    Schedule,
    StateWrite,
    ParamWrite,
    Return,
    CallbackReturn,
    Throw,
    Log,
    ExternalCall,
}

impl SiteCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            SiteCategory::IoCall => "io-call",
            SiteCategory::Schedule => "schedule",
            SiteCategory::StateWrite => "state-write",
            SiteCategory::ParamWrite => "param-write",
            SiteCategory::Return => "return",
            SiteCategory::CallbackReturn => "callback-return",
            SiteCategory::Throw => "throw",
            SiteCategory::Log => "log",
            SiteCategory::ExternalCall => "external-call",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Classification {
    Contractual,
    Incidental,
    Review,
}

impl Classification {
    pub fn as_str(self) -> &'static str {
        match self {
            Classification::Contractual => "contractual",
            Classification::Incidental => "incidental",
            Classification::Review => "review",
        }
    }
}

/// 1-based line, 1-based UTF-16 column: the manifest's convention everywhere else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SitePosition {
    pub line: usize,
    pub column: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SiteMeta {
    pub id: String,
    pub file: String,
    pub category: SiteCategory,
    pub classification: Classification,
    pub start: SitePosition,
    pub end: SitePosition,
    /// the innermost enclosing function, callbacks included
    pub function: String,
    /// the innermost enclosing *named* function: whose contract this site belongs to
    pub owner: String,
    pub exported: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// for a call: the callee chain, its method name, and the first argument's text
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chain: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arg0: Option<String>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SiteDiscovery {
    pub sites: Vec<SiteMeta>,
    /// rules this pass could not decide from syntax alone, by id
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SiteError {
    UnknownSourceType(String),
    Parse(Vec<String>),
}

impl std::fmt::Display for SiteError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SiteError::UnknownSourceType(message) => {
                write!(formatter, "unknown source type: {message}")
            }
            SiteError::Parse(errors) => write!(formatter, "parse errors: {}", errors.join("; ")),
        }
    }
}

/// Where the root of a write lives relative to the code doing the writing. Only state that outlives the
/// writing call is a contractual site; a fresh local object is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Locality {
    Local,
    /// a local binding that may alias state obtained from elsewhere
    Alias,
    Param,
    NonLocal,
    This,
    Global,
}

impl Locality {
    /// Does a write here outlive the call?
    fn escapes(self) -> bool {
        matches!(
            self,
            Locality::NonLocal | Locality::This | Locality::Global | Locality::Alias
        )
    }

    fn as_str(self) -> &'static str {
        match self {
            Locality::Local => "local",
            Locality::Alias => "alias",
            Locality::Param => "param",
            Locality::NonLocal => "nonlocal",
            Locality::This => "this",
            Locality::Global => "global",
        }
    }
}

/// The root of a chain, in the only three shapes locality cares about.
enum Root<'a, 'b> {
    This,
    Identifier(&'b IdentifierReference<'a>),
    Other,
}

pub fn discover_effect_sites(file: &str, source: &str) -> Result<SiteDiscovery, SiteError> {
    let source_type = SourceType::from_path(std::path::Path::new(file))
        .map_err(|error| SiteError::UnknownSourceType(error.to_string()))?;
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source, source_type).parse();
    if !parsed.errors.is_empty() {
        return Err(SiteError::Parse(
            parsed.errors.iter().map(ToString::to_string).collect(),
        ));
    }
    let semantic = SemanticBuilder::new().build(&parsed.program).semantic;
    let mut finder = Finder {
        file,
        source,
        semantic: &semantic,
        sites: Vec::new(),
        limitations: BTreeSet::new(),
    };
    finder.run();
    Ok(SiteDiscovery {
        sites: finder.sites,
        limitations: finder.limitations.into_iter().collect(),
    })
}

struct Finder<'a, 's> {
    file: &'s str,
    source: &'s str,
    semantic: &'s Semantic<'a>,
    sites: Vec<SiteMeta>,
    limitations: BTreeSet<String>,
}

impl<'a> Finder<'a, '_> {
    fn run(&mut self) {
        // Node order follows the parse, so sites come out in source order, as they do from a traversal.
        let ids: Vec<NodeId> = self
            .semantic
            .nodes()
            .iter_enumerated()
            .map(|(id, _)| id)
            .collect();
        for id in ids {
            self.classify(id);
        }
    }

    fn classify(&mut self, id: NodeId) {
        match self.semantic.nodes().kind(id) {
            AstKind::CallExpression(call) => self.classify_call(id, call),
            AstKind::AssignmentExpression(assignment) => {
                let how = assignment.operator.as_str();
                match &assignment.left {
                    AssignmentTarget::AssignmentTargetIdentifier(identifier) => {
                        self.write_to_identifier(id, identifier, how);
                    }
                    AssignmentTarget::StaticMemberExpression(member) => {
                        self.write_to_member(id, &member.object, how);
                    }
                    AssignmentTarget::ComputedMemberExpression(member) => {
                        self.write_to_member(id, &member.object, how);
                    }
                    AssignmentTarget::PrivateFieldExpression(member) => {
                        self.write_to_member(id, &member.object, how);
                    }
                    _ => {}
                }
            }
            AstKind::UpdateExpression(update) => {
                let how = match update.operator {
                    UpdateOperator::Increment => "++",
                    UpdateOperator::Decrement => "--",
                };
                match &update.argument {
                    SimpleAssignmentTarget::AssignmentTargetIdentifier(identifier) => {
                        self.write_to_identifier(id, identifier, how);
                    }
                    SimpleAssignmentTarget::StaticMemberExpression(member) => {
                        self.write_to_member(id, &member.object, how);
                    }
                    SimpleAssignmentTarget::ComputedMemberExpression(member) => {
                        self.write_to_member(id, &member.object, how);
                    }
                    SimpleAssignmentTarget::PrivateFieldExpression(member) => {
                        self.write_to_member(id, &member.object, how);
                    }
                    _ => {}
                }
            }
            AstKind::UnaryExpression(unary) if unary.operator == UnaryOperator::Delete => {
                let target = unwrap_expr(&unary.argument);
                if let Some(object) = member_object(target) {
                    self.write_to_member(id, object, "delete");
                }
            }
            AstKind::ReturnStatement(statement) if statement.argument.is_some() => {
                let category = if self
                    .enclosing_function(id)
                    .is_some_and(|function| self.is_callback_function(function))
                {
                    SiteCategory::CallbackReturn
                } else {
                    SiteCategory::Return
                };
                self.push(id, category, Classification::Contractual, None);
            }
            AstKind::ArrowFunctionExpression(arrow) => {
                // `const f = (x) => expr`: the expression body is the function's return. An arrow whose
                // body is another function is a factory, not a value, so it is left alone.
                if !arrow.expression {
                    return;
                }
                let Some(oxc_ast::ast::Statement::ExpressionStatement(body)) =
                    arrow.body.statements.first()
                else {
                    return;
                };
                if matches!(
                    unwrap_expr(&body.expression),
                    Expression::ArrowFunctionExpression(_) | Expression::FunctionExpression(_)
                ) {
                    return;
                }
                let category = if self.is_callback_function(id) {
                    SiteCategory::CallbackReturn
                } else {
                    SiteCategory::Return
                };
                self.push_span(
                    body.expression.span(),
                    id,
                    category,
                    Classification::Contractual,
                    Some("expression body".to_owned()),
                    None,
                );
            }
            AstKind::ThrowStatement(_) => {
                self.push(id, SiteCategory::Throw, Classification::Contractual, None);
            }
            _ => {}
        }
    }

    fn classify_call(&mut self, id: NodeId, call: &oxc_ast::ast::CallExpression<'a>) {
        let callee = unwrap_expr(&call.callee);
        let Some(method) = callee_method(callee) else {
            return;
        };
        let chain = chain_names(callee);
        if is_log_callee(&chain) {
            // Calls through a logger abstraction are diagnostics. Direct console calls are stream
            // writes, and inside a logger implementation they *are* the boundary, so they need
            // judgement rather than a blanket verdict.
            let direct = chain.first().is_some_and(|name| name == "console");
            let (classification, note) = if direct {
                (Classification::Review, "console")
            } else {
                (Classification::Incidental, "logger")
            };
            self.push(id, SiteCategory::Log, classification, Some(note.to_owned()));
            return;
        }
        let plain_identifier = matches!(callee, Expression::Identifier(_));
        if plain_identifier && SCHEDULE.contains(&method) {
            self.push(
                id,
                SiteCategory::Schedule,
                Classification::Contractual,
                None,
            );
            return;
        }
        if plain_identifier && IO_FUNCTIONS.contains(&method) {
            self.push(id, SiteCategory::IoCall, Classification::Contractual, None);
            return;
        }
        let receiver = member_object(callee);
        if receiver.is_some() && IO_METHODS.contains(&method) {
            let classification = if self.inside_log_call(id) {
                Classification::Incidental
            } else {
                Classification::Contractual
            };
            self.push(id, SiteCategory::IoCall, classification, None);
            return;
        }
        if let Some(receiver) = receiver {
            let locality = self.locality(root_kind(root_of(receiver)), id);
            // A mutator call on state that outlives the call is a state write. Confirming the receiver
            // is a container (`Map`, `Set`, an array) needs types, and the choice of what to do without
            // them matters: emitting nothing would shrink the denominator and flatter the metric, so the
            // site is reported and the limitation declared.
            if MUTATORS.contains(&method) && (locality.escapes() || locality == Locality::Param) {
                self.limitations.insert(LIMIT_CONTAINER_TYPES.to_owned());
                let (category, classification) = if locality == Locality::Param {
                    (SiteCategory::ParamWrite, Classification::Review)
                } else {
                    (SiteCategory::StateWrite, Classification::Contractual)
                };
                let note = if locality == Locality::Param {
                    format!("mutator:{method}")
                } else {
                    format!("mutator:{method} {}", locality.as_str())
                };
                self.push(id, category, classification, Some(note));
                return;
            }
            // A method call on a non-local object whose type is declared in this project drives
            // project state. Deciding that needs types too.
            if matches!(locality, Locality::NonLocal | Locality::Global) {
                self.limitations.insert(LIMIT_PROJECT_TYPES.to_owned());
            }
        }
        let root = root_of(callee);
        if let Expression::Identifier(identifier) = root {
            let symbol = self.symbol_of_reference(identifier);
            match symbol.map(|symbol| self.declaration_kind(symbol)) {
                Some(DeclarationKind::Import) => {
                    self.push(
                        id,
                        SiteCategory::ExternalCall,
                        Classification::Review,
                        Some("import".to_owned()),
                    );
                }
                Some(DeclarationKind::Parameter) | Some(DeclarationKind::BindingElement) => {
                    self.push(
                        id,
                        SiteCategory::ExternalCall,
                        Classification::Review,
                        Some("param".to_owned()),
                    );
                }
                _ => {
                    // A call through an ambient global leaves the module the way an import call does.
                    // `globalThis.x.y()` is read through its property, since globalThis computes
                    // nothing itself.
                    let name = identifier.name.as_str();
                    let head = if GLOBAL_ROOTS.contains(&name) {
                        chain.get(1).map(String::as_str)
                    } else {
                        Some(name)
                    };
                    let callee_is_pure = plain_identifier && PURE_GLOBALS.contains(&method);
                    if symbol.is_none()
                        && head
                            .is_some_and(|head| !head.is_empty() && !PURE_GLOBALS.contains(&head))
                        && !callee_is_pure
                    {
                        self.push(
                            id,
                            SiteCategory::ExternalCall,
                            Classification::Review,
                            Some("global".to_owned()),
                        );
                    }
                }
            }
        } else if matches!(root, Expression::ThisExpression(_)) && receiver.is_some() {
            // `this.onMessage(...)` where the property is a field rather than a method of the class:
            // the call runs whatever was injected there.
            if self.this_call_is_not_a_method(id, method) {
                // `this.field.method()`: what the field holds decides whether this is the class's own
                // code, and that needs types. Declared, and resolved towards reporting the site.
                if chain.len() > 2 {
                    self.limitations.insert(LIMIT_THIS_CALL_TYPES.to_owned());
                }
                self.push(
                    id,
                    SiteCategory::ExternalCall,
                    Classification::Review,
                    Some("this-callback".to_owned()),
                );
            }
        }
    }

    fn write_to_identifier(&mut self, id: NodeId, target: &IdentifierReference<'a>, how: &str) {
        let locality = self.locality(Root::Identifier(target), id);
        if matches!(locality, Locality::NonLocal | Locality::Global) {
            self.push(
                id,
                SiteCategory::StateWrite,
                Classification::Contractual,
                Some(format!("{how} variable {}", locality.as_str())),
            );
        }
    }

    fn write_to_member(&mut self, id: NodeId, object: &Expression<'a>, how: &str) {
        let locality = self.locality(root_kind(root_of(object)), id);
        if locality.escapes() {
            self.push(
                id,
                SiteCategory::StateWrite,
                Classification::Contractual,
                Some(format!("{how} property {}", locality.as_str())),
            );
        } else if locality == Locality::Param {
            self.push(
                id,
                SiteCategory::ParamWrite,
                Classification::Review,
                Some(format!("{how} property")),
            );
        }
    }

    // -----------------------------------------------------------------------
    // Semantics
    // -----------------------------------------------------------------------

    fn symbol_of_reference(&self, identifier: &IdentifierReference<'a>) -> Option<SymbolId> {
        identifier
            .reference_id
            .get()
            .and_then(|reference| self.semantic.scoping().get_reference(reference).symbol_id())
    }

    fn declaration_kind(&self, symbol: SymbolId) -> DeclarationKind {
        let declaration = self.semantic.symbol_declaration(symbol).id();
        // The declaration node may itself be the parameter or declarator, so it is examined before its
        // ancestors: a binding identifier inside a pattern is what makes a destructured binding.
        let own = std::iter::once(self.semantic.nodes().kind(declaration));
        for kind in own.chain(self.semantic.nodes().ancestor_kinds(declaration)) {
            match kind {
                AstKind::ImportDeclaration(_) => return DeclarationKind::Import,
                AstKind::FormalParameter(_) => return DeclarationKind::Parameter,
                AstKind::VariableDeclarator(declarator) => {
                    return if matches!(declarator.id, BindingPattern::BindingIdentifier(_)) {
                        DeclarationKind::Variable
                    } else {
                        DeclarationKind::BindingElement
                    };
                }
                AstKind::Function(_)
                | AstKind::ArrowFunctionExpression(_)
                | AstKind::Program(_) => {
                    break;
                }
                _ => {}
            }
        }
        DeclarationKind::Other
    }

    /// Where the root of a write lives relative to the code writing it.
    fn locality(&self, root: Root<'a, '_>, at: NodeId) -> Locality {
        let identifier = match root {
            Root::This => return Locality::This,
            Root::Other => return Locality::Local,
            Root::Identifier(identifier) => identifier,
        };
        let Some(symbol) = self.symbol_of_reference(identifier) else {
            return Locality::Global;
        };
        let declaration = self.semantic.symbol_declaration(symbol).id();
        let kind = self.declaration_kind(symbol);
        if kind == DeclarationKind::Import {
            // the binding lives at this file's module scope, so what it holds outlives any call here
            return Locality::NonLocal;
        }
        let declaring = self.enclosing_function(declaration);
        let using = self.enclosing_function(at);
        match kind {
            DeclarationKind::Parameter => {
                return if declaring == using {
                    Locality::Param
                } else {
                    Locality::NonLocal
                };
            }
            DeclarationKind::BindingElement => {
                return if declaring == using {
                    Locality::Local
                } else {
                    Locality::NonLocal
                };
            }
            _ => {}
        }
        let Some(declaring) = declaring else {
            return Locality::NonLocal; // module-scope variable
        };
        if Some(declaring) != using {
            return Locality::NonLocal;
        }
        // Same function: a fresh literal, object, array or `new` is truly local. Anything else may
        // alias state obtained from elsewhere. The declaration node may be the declarator itself.
        let own = std::iter::once(self.semantic.nodes().kind(declaration));
        for kind in own.chain(self.semantic.nodes().ancestor_kinds(declaration)) {
            if let AstKind::VariableDeclarator(declarator) = kind {
                return match declarator.init.as_ref().map(unwrap_expr) {
                    None => Locality::Local,
                    Some(init) if fresh_value(init) => Locality::Local,
                    Some(_) => Locality::Alias,
                };
            }
        }
        Locality::Local
    }

    /// Is `this.…<name>()` calling something other than a method of the enclosing class? A method call
    /// runs code this class owns; anything else runs whatever was assigned or injected there, including
    /// a container's own method such as `this.sessions.get(...)`.
    fn this_call_is_not_a_method(&self, at: NodeId, name: &str) -> bool {
        let mut class_body = None;
        for (node_id, node) in self.semantic.nodes().ancestors_enumerated(at) {
            if matches!(node.kind(), AstKind::ClassBody(_)) {
                class_body = Some(node_id);
                break;
            }
        }
        let Some(class_body) = class_body else {
            return false;
        };
        let AstKind::ClassBody(body) = self.semantic.nodes().kind(class_body) else {
            return false;
        };
        let declares_method = body.body.iter().any(|element| match element {
            oxc_ast::ast::ClassElement::MethodDefinition(method) => {
                method.key.static_name().as_deref() == Some(name)
            }
            _ => false,
        });
        !declares_method
    }

    /// The innermost function at or above this node. A site on a function node itself, such as an
    /// arrow's expression body, belongs to that function and not to the one around it.
    fn enclosing_function(&self, id: NodeId) -> Option<NodeId> {
        if is_function_kind(self.semantic.nodes().kind(id)) {
            return Some(id);
        }
        self.semantic
            .nodes()
            .ancestors_enumerated(id)
            .find(|(_, node)| is_function_kind(node.kind()))
            .map(|(node_id, _)| node_id)
    }

    /// A function is a callback unless it is a declaration, a method, a constructor, or the value of a
    /// variable or property: those have names their caller can hold.
    fn is_callback_function(&self, function: NodeId) -> bool {
        match self.semantic.nodes().kind(function) {
            AstKind::Function(declaration) => {
                if declaration.r#type == FunctionType::FunctionDeclaration {
                    return false;
                }
            }
            AstKind::ArrowFunctionExpression(_) => {}
            _ => return false,
        }
        // A variable's value and a method have names their caller can hold. A function held by an
        // object property or a class field does not: it is passed somewhere and called from there,
        // so a `return` inside it belongs to the callback, not to the surrounding contract.
        !matches!(
            self.semantic.nodes().parent_kind(function),
            AstKind::VariableDeclarator(_) | AstKind::MethodDefinition(_)
        )
    }

    fn function_name(&self, id: NodeId) -> String {
        match self.enclosing_function(id) {
            Some(function) => self.name_of_function(function),
            None => "<module>".to_owned(),
        }
    }

    fn name_of_function(&self, function: NodeId) -> String {
        let kind = self.semantic.nodes().kind(function);
        if let AstKind::Function(declaration) = kind
            && declaration.r#type == FunctionType::FunctionDeclaration
            && let Some(name) = &declaration.id
        {
            return name.name.to_string();
        }
        let parent = self.semantic.nodes().parent_id(function);
        match self.semantic.nodes().kind(parent) {
            AstKind::MethodDefinition(method) => {
                let class = self.enclosing_class_name(parent);
                let name = if method.kind.is_constructor() {
                    "constructor".to_owned()
                } else {
                    method.key.static_name().unwrap_or_default().to_string()
                };
                format!("{class}.{name}")
            }
            AstKind::VariableDeclarator(declarator) => match &declarator.id {
                BindingPattern::BindingIdentifier(identifier) => identifier.name.to_string(),
                _ => self.anonymous_name(kind.span()),
            },
            AstKind::ObjectProperty(property) => property
                .key
                .static_name()
                .map(|name| name.to_string())
                .unwrap_or_else(|| self.anonymous_name(kind.span())),
            AstKind::PropertyDefinition(property) => property
                .key
                .static_name()
                .map(|name| name.to_string())
                .unwrap_or_else(|| self.anonymous_name(kind.span())),
            AstKind::AssignmentExpression(assignment) => {
                format!("{} =", self.text_of(assignment.left.span(), 40))
            }
            AstKind::CallExpression(call) => {
                format!("{}(cb)", self.text_of(call.callee.span(), 40))
            }
            _ => self.anonymous_name(kind.span()),
        }
    }

    fn anonymous_name(&self, span: Span) -> String {
        let (line, _) = line_and_utf16_column(self.source, span.start as usize);
        format!("anonymous@{line}")
    }

    fn enclosing_class_name(&self, id: NodeId) -> String {
        for kind in self.semantic.nodes().ancestor_kinds(id) {
            if let AstKind::Class(class) = kind {
                return class
                    .id
                    .as_ref()
                    .map(|name| name.name.to_string())
                    .unwrap_or_else(|| "?".to_owned());
            }
        }
        "?".to_owned()
    }

    /// The innermost enclosing function that is not a callback: whose contract this site belongs to.
    fn owner_name(&self, id: NodeId) -> String {
        let mut function = self.enclosing_function(id);
        while let Some(candidate) = function {
            if !self.is_callback_function(candidate) {
                return self.name_of_function(candidate);
            }
            function = self
                .semantic
                .nodes()
                .ancestors_enumerated(candidate)
                .find(|(_, node)| is_function_kind(node.kind()))
                .map(|(node_id, _)| node_id);
        }
        "<module>".to_owned()
    }

    fn is_exported(&self, id: NodeId) -> bool {
        let Some(function) = self.enclosing_function(id) else {
            return false;
        };
        for kind in self.semantic.nodes().ancestor_kinds(function) {
            match kind {
                AstKind::ExportNamedDeclaration(_) | AstKind::ExportDefaultDeclaration(_) => {
                    return true;
                }
                AstKind::Program(_) => return false,
                _ => {}
            }
        }
        false
    }

    /// Is this node inside a logging call, without crossing a statement boundary? Anything computed for
    /// a log message is a diagnostic too.
    fn inside_log_call(&self, id: NodeId) -> bool {
        for node in self.semantic.nodes().ancestors(id) {
            if let AstKind::CallExpression(call) = node.kind()
                && is_log_callee(&chain_names(unwrap_expr(&call.callee)))
            {
                return true;
            }
            if is_statement_kind(node.kind()) {
                return false;
            }
        }
        false
    }

    // -----------------------------------------------------------------------
    // Emitting
    // -----------------------------------------------------------------------

    fn push(
        &mut self,
        id: NodeId,
        category: SiteCategory,
        classification: Classification,
        note: Option<String>,
    ) {
        let kind = self.semantic.nodes().kind(id);
        let call = match kind {
            AstKind::CallExpression(call) => Some(call),
            _ => None,
        };
        self.push_span(kind.span(), id, category, classification, note, call);
    }

    fn push_span(
        &mut self,
        span: Span,
        at: NodeId,
        category: SiteCategory,
        classification: Classification,
        note: Option<String>,
        call: Option<&oxc_ast::ast::CallExpression<'a>>,
    ) {
        let (start_line, start_column) = line_and_utf16_column(self.source, span.start as usize);
        let (end_line, end_column) = line_and_utf16_column(self.source, span.end as usize);
        let (chain, method, arg0) = match call {
            Some(call) => {
                let callee = unwrap_expr(&call.callee);
                (
                    chain_names(callee),
                    callee_method(callee).map(str::to_owned),
                    call.arguments
                        .first()
                        .map(|argument| self.text_of(argument.span(), 240)),
                )
            }
            None => (Vec::new(), None, None),
        };
        self.sites.push(SiteMeta {
            id: format!("{}#{}", self.file, self.sites.len() + 1),
            file: self.file.to_owned(),
            category,
            classification,
            start: SitePosition {
                line: start_line,
                column: start_column,
            },
            end: SitePosition {
                line: end_line,
                column: end_column,
            },
            function: self.function_name(at),
            owner: self.owner_name(at),
            exported: self.is_exported(at),
            note,
            chain,
            method,
            arg0,
            text: self.text_of(span, 100),
        });
    }

    /// Source text with runs of whitespace collapsed, truncated the way the prototype truncates.
    fn text_of(&self, span: Span, limit: usize) -> String {
        let raw = &self.source[span.start as usize..span.end as usize];
        let mut collapsed = String::with_capacity(raw.len());
        let mut in_space = false;
        for character in raw.chars() {
            if character.is_whitespace() {
                if !in_space {
                    collapsed.push(' ');
                    in_space = true;
                }
            } else {
                collapsed.push(character);
                in_space = false;
            }
        }
        collapsed.chars().take(limit).collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeclarationKind {
    Import,
    Parameter,
    BindingElement,
    Variable,
    Other,
}

fn is_function_kind(kind: AstKind<'_>) -> bool {
    matches!(
        kind,
        AstKind::Function(_) | AstKind::ArrowFunctionExpression(_)
    )
}

fn is_statement_kind(kind: AstKind<'_>) -> bool {
    matches!(
        kind,
        AstKind::ExpressionStatement(_)
            | AstKind::VariableDeclaration(_)
            | AstKind::ReturnStatement(_)
            | AstKind::IfStatement(_)
            | AstKind::ForStatement(_)
            | AstKind::ForInStatement(_)
            | AstKind::ForOfStatement(_)
            | AstKind::WhileStatement(_)
            | AstKind::DoWhileStatement(_)
            | AstKind::ThrowStatement(_)
            | AstKind::TryStatement(_)
            | AstKind::SwitchStatement(_)
            | AstKind::BlockStatement(_)
            | AstKind::BreakStatement(_)
            | AstKind::ContinueStatement(_)
            | AstKind::LabeledStatement(_)
            | AstKind::WithStatement(_)
    )
}

/// Strip the wrappers that do not change what an expression is: parentheses and TypeScript assertions.
fn unwrap_expr<'a, 'b>(expression: &'b Expression<'a>) -> &'b Expression<'a> {
    let mut current = expression;
    loop {
        current = match current {
            Expression::ParenthesizedExpression(inner) => &inner.expression,
            Expression::TSNonNullExpression(inner) => &inner.expression,
            Expression::TSAsExpression(inner) => &inner.expression,
            Expression::TSSatisfiesExpression(inner) => &inner.expression,
            Expression::TSTypeAssertion(inner) => &inner.expression,
            other => return other,
        };
    }
}

/// The object a member access reads through, if this expression is one.
fn member_object<'a, 'b>(expression: &'b Expression<'a>) -> Option<&'b Expression<'a>> {
    match expression {
        Expression::StaticMemberExpression(member) => Some(&member.object),
        Expression::ComputedMemberExpression(member) => Some(&member.object),
        Expression::PrivateFieldExpression(member) => Some(&member.object),
        _ => None,
    }
}

fn callee_method<'a>(callee: &'a Expression<'_>) -> Option<&'a str> {
    match callee {
        Expression::StaticMemberExpression(member) => Some(member.property.name.as_str()),
        Expression::ComputedMemberExpression(member) => match &member.expression {
            Expression::StringLiteral(literal) => Some(literal.value.as_str()),
            _ => None,
        },
        Expression::Identifier(identifier) => Some(identifier.name.as_str()),
        _ => None,
    }
}

/// The chain of names a callee reads through: `admin.rest.resources.Article.find` gives those five, with
/// `this` standing for itself.
fn chain_names(expression: &Expression<'_>) -> Vec<String> {
    let mut names = Vec::new();
    let mut current = unwrap_expr(expression);
    loop {
        match current {
            Expression::StaticMemberExpression(member) => {
                names.push(member.property.name.to_string());
                current = unwrap_expr(&member.object);
            }
            Expression::ComputedMemberExpression(member) => current = unwrap_expr(&member.object),
            Expression::PrivateFieldExpression(member) => current = unwrap_expr(&member.object),
            Expression::CallExpression(call) => current = unwrap_expr(&call.callee),
            _ => break,
        }
    }
    match current {
        Expression::Identifier(identifier) => names.push(identifier.name.to_string()),
        Expression::ThisExpression(_) => names.push("this".to_owned()),
        _ => {}
    }
    names.reverse();
    names
}

/// The object a chain of member accesses and calls starts from.
fn root_of<'a, 'b>(expression: &'b Expression<'a>) -> &'b Expression<'a> {
    let mut current = unwrap_expr(expression);
    loop {
        current = match current {
            Expression::StaticMemberExpression(member) => unwrap_expr(&member.object),
            Expression::ComputedMemberExpression(member) => unwrap_expr(&member.object),
            Expression::PrivateFieldExpression(member) => unwrap_expr(&member.object),
            Expression::CallExpression(call) => unwrap_expr(&call.callee),
            other => return other,
        };
    }
}

fn root_kind<'a, 'b>(expression: &'b Expression<'a>) -> Root<'a, 'b> {
    match expression {
        Expression::ThisExpression(_) => Root::This,
        Expression::Identifier(identifier) => Root::Identifier(identifier),
        _ => Root::Other,
    }
}

/// A value that cannot alias state from elsewhere.
fn fresh_value(expression: &Expression<'_>) -> bool {
    matches!(
        expression,
        Expression::ObjectExpression(_)
            | Expression::ArrayExpression(_)
            | Expression::NewExpression(_)
            | Expression::StringLiteral(_)
            | Expression::NumericLiteral(_)
            | Expression::TemplateLiteral(_)
            | Expression::BooleanLiteral(_)
            | Expression::NullLiteral(_)
    )
}

fn is_log_callee(chain: &[String]) -> bool {
    match chain.first().map(String::as_str) {
        Some("logger" | "console") => true,
        Some("this") => chain.get(1).is_some_and(|name| name == "logger"),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sites(source: &str) -> Vec<SiteMeta> {
        discover_effect_sites("src/a.ts", source)
            .expect("the fixture parses")
            .sites
    }

    fn categories(source: &str) -> Vec<(&'static str, &'static str, Option<String>)> {
        sites(source)
            .into_iter()
            .map(|site| {
                (
                    site.category.as_str(),
                    site.classification.as_str(),
                    site.note,
                )
            })
            .collect()
    }

    #[test]
    fn a_return_with_a_value_is_a_contractual_site() {
        let found = sites("export function f(x: number) { return x + 1 }");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].category, SiteCategory::Return);
        assert_eq!(found[0].classification, Classification::Contractual);
        assert_eq!(found[0].owner, "f");
        assert!(found[0].exported);
        assert_eq!(found[0].start.line, 1);
    }

    #[test]
    fn a_bare_return_is_not_a_site() {
        assert!(sites("function f() { return }").is_empty());
    }

    #[test]
    fn a_return_inside_a_callback_belongs_to_the_callback() {
        let found = sites("function f(xs: number[]) { return xs.map((x) => x * 2) }");
        let callback = found
            .iter()
            .find(|site| site.category == SiteCategory::CallbackReturn)
            .expect("the arrow body is a callback return");
        assert_eq!(callback.owner, "f");
        assert_eq!(callback.note.as_deref(), Some("expression body"));
    }

    #[test]
    fn an_arrow_returning_an_arrow_is_a_factory_not_a_value() {
        let found = sites("export const f = (a: number) => (b: number) => a + b");
        // the inner arrow's body is the only value returned
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].function, "anonymous@1");
    }

    #[test]
    fn a_throw_is_a_contractual_site() {
        let found = sites("function f() { throw new Error('no') }");
        assert_eq!(found[0].category, SiteCategory::Throw);
    }

    #[test]
    fn a_console_call_needs_review_and_a_logger_call_is_incidental() {
        assert_eq!(
            categories("function f() { console.log('x') }"),
            vec![("log", "review", Some("console".to_owned()))]
        );
        assert_eq!(
            categories("function f(logger: any) { logger.info('x') }"),
            vec![("log", "incidental", Some("logger".to_owned()))]
        );
    }

    #[test]
    fn work_done_inside_a_log_call_is_incidental_too() {
        let found = categories("function f(res: any) { console.log(res.write('x')) }");
        assert!(found.contains(&("io-call", "incidental", None)));
    }

    #[test]
    fn scheduling_and_outbound_calls_are_contractual() {
        assert_eq!(
            categories("function f() { setTimeout(() => 1, 5) }")
                .into_iter()
                .filter(|(category, _, _)| *category == "schedule")
                .count(),
            1
        );
        assert!(
            categories("function f(res: any) { res.json({ a: 1 }) }").contains(&(
                "io-call",
                "contractual",
                None
            ))
        );
    }

    #[test]
    fn a_write_to_a_module_variable_is_a_state_write_and_a_local_one_is_not() {
        let found = categories("let total = 0\nfunction f() { total = 1 }");
        assert_eq!(
            found,
            vec![(
                "state-write",
                "contractual",
                Some("= variable nonlocal".to_owned())
            )]
        );
        assert!(sites("function f() { let total = 0; total = 1; }").is_empty());
    }

    #[test]
    fn a_write_through_this_is_a_state_write() {
        let found = categories("class A { private x = 0; set(v: number) { this.x = v } }");
        assert!(found.contains(&(
            "state-write",
            "contractual",
            Some("= property this".to_owned())
        )));
    }

    #[test]
    fn a_write_to_a_fresh_local_object_is_not_a_site_but_an_aliased_one_is() {
        assert!(sites("function f() { const o = {}; o.a = 1; }").is_empty());
        let aliased = categories("function f(get: any) { const o = get(); o.a = 1; }");
        assert!(aliased.contains(&(
            "state-write",
            "contractual",
            Some("= property alias".to_owned())
        )));
    }

    #[test]
    fn a_write_through_a_parameter_is_a_review_site() {
        let found = categories("function f(target: any) { target.a = 1 }");
        assert!(found.contains(&("param-write", "review", Some("= property".to_owned()))));
    }

    #[test]
    fn a_call_through_an_import_or_a_parameter_needs_review() {
        assert!(
            categories("import { helper } from './h'\nfunction f() { return helper() }")
                .contains(&("external-call", "review", Some("import".to_owned())))
        );
        assert!(
            categories("function f(admin: any) { return admin.find() }").contains(&(
                "external-call",
                "review",
                Some("param".to_owned())
            ))
        );
    }

    #[test]
    fn a_call_through_an_ambient_global_needs_review_but_a_pure_builtin_does_not() {
        assert!(
            categories("function f() { return shopify.toast.show('x') }").contains(&(
                "external-call",
                "review",
                Some("global".to_owned())
            ))
        );
        let pure = categories("function f(a: unknown) { return JSON.stringify(a) }");
        assert_eq!(pure, vec![("return", "contractual", None)]);
        let coerced = categories("function f(a: string) { return Number(a) }");
        assert_eq!(coerced, vec![("return", "contractual", None)]);
    }

    #[test]
    fn a_call_on_an_injected_field_needs_review_but_a_method_call_does_not() {
        let field = categories("class A { onMessage: any; run() { return this.onMessage(1) } }");
        assert!(field.contains(&("external-call", "review", Some("this-callback".to_owned()))));
        let method = sites("class A { helper() { return 1 } run() { return this.helper() } }");
        assert!(
            !method
                .iter()
                .any(|site| site.note.as_deref() == Some("this-callback"))
        );
    }

    #[test]
    fn a_mutator_call_is_declared_a_limitation_rather_than_guessed() {
        let found = discover_effect_sites(
            "src/a.ts",
            "class A { private sessions = new Map(); add(k: string) { this.sessions.set(k, 1) } }",
        )
        .expect("parses");
        assert!(
            found
                .limitations
                .iter()
                .any(|limit| limit == LIMIT_CONTAINER_TYPES)
        );
    }

    #[test]
    fn a_method_is_named_with_its_class_and_a_callback_with_what_holds_it() {
        let found = sites("class A { run(xs: number[]) { return xs.map((x) => x + 1) } }");
        let method_site = found
            .iter()
            .find(|site| site.category == SiteCategory::Return)
            .expect("the method returns");
        assert_eq!(method_site.owner, "A.run");
        assert_eq!(method_site.function, "A.run");
        let callback = found
            .iter()
            .find(|site| site.category == SiteCategory::CallbackReturn)
            .expect("the arrow returns");
        assert_eq!(callback.owner, "A.run");
        assert_eq!(callback.function, "xs.map(cb)");
    }

    #[test]
    fn the_text_of_a_site_collapses_whitespace() {
        let found = sites("function f() {\n  return {\n    a: 1,\n  }\n}");
        assert_eq!(found[0].text, "return { a: 1, }");
    }
}
