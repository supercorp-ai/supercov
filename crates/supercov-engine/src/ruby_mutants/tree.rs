//! The tree the mutant gem mutates: Ruby's `parser` gem AST, as Prism's
//! translation layer (`Prism::Translation::Parser34`, with Unparser's modern
//! builder) builds it from the same source. mutant's mutators are written
//! against this shape -- `(send recv :sel args...)`, `(begin ...)` for a
//! statement list, `(args (procarg0 (arg :x)))` for a block's lone parameter
//! -- so Supercov converts Prism's tree into it and runs a port of the
//! mutators on that, rather than translating every rule.
//!
//! Each node converted from the source keeps its byte span, so a mutation
//! can be located and its untouched parts printed as written. Nodes a
//! mutator makes have none. Equality ignores spans, as `Parser::AST::Node`'s
//! does: two mutations that give the same tree are one mutant.

use std::rc::Rc;

use ruby_prism::{Location, Node};

/// One child of a node: a node, or one of the plain values parser stores
/// beside nodes.
#[derive(Debug, Clone, Eq)]
pub(crate) enum Child {
    Node(Rc<N>),
    Sym(String),
    Str(Vec<u8>),
    Int(i128),
    /// An integer past `i128`, as its decimal digits.
    BigInt(String),
    /// A float's IEEE bits. Equal as Ruby's `eql?` holds floats equal: by
    /// value, so `-0.0` is `0.0` (mutant drops `-0.0` made `0.0` as no
    /// change).
    Float(u64),
    /// A rational or imaginary literal, as `inspect` prints its value.
    Raw(String),
    Nil,
}

impl PartialEq for Child {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Child::Node(left), Child::Node(right)) => left == right,
            (Child::Sym(left), Child::Sym(right))
            | (Child::BigInt(left), Child::BigInt(right))
            | (Child::Raw(left), Child::Raw(right)) => left == right,
            (Child::Str(left), Child::Str(right)) => left == right,
            (Child::Int(left), Child::Int(right)) => left == right,
            (Child::Float(left), Child::Float(right)) => {
                left == right || f64::from_bits(*left) == f64::from_bits(*right)
            }
            (Child::Nil, Child::Nil) => true,
            _ => false,
        }
    }
}

impl std::hash::Hash for Child {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            Child::Node(value) => value.hash(state),
            Child::Sym(value) | Child::BigInt(value) | Child::Raw(value) => value.hash(state),
            Child::Str(value) => value.hash(state),
            Child::Int(value) => value.hash(state),
            // `-0.0` hashes as `0.0`, which it equals.
            Child::Float(bits) => {
                let zero = f64::from_bits(*bits) == 0.0;
                (if zero { 0 } else { *bits }).hash(state);
            }
            Child::Nil => {}
        }
    }
}

impl Child {
    pub(crate) fn node(&self) -> Option<&Rc<N>> {
        match self {
            Self::Node(node) => Some(node),
            _ => None,
        }
    }

    pub(crate) fn sym(&self) -> Option<&str> {
        match self {
            Self::Sym(name) => Some(name),
            _ => None,
        }
    }

    pub(crate) fn is_nil(&self) -> bool {
        matches!(self, Self::Nil)
    }
}

/// How a node was written, where printing it again needs to know.
pub(crate) mod syntax {
    /// A statement of a statement list (or a body of one statement).
    pub const STATEMENT: u32 = 1;
    /// `c ? a : b`.
    pub const TERNARY: u32 = 1 << 1;
    /// `a if c`, `a while c`.
    pub const MODIFIER: u32 = 1 << 2;
    /// A hash pair written `key: value`.
    pub const LABEL: u32 = 1 << 3;
    /// Parameters written in `|...|`.
    pub const BLOCK_PARAMETERS: u32 = 1 << 4;
    /// `def m = value`.
    pub const ENDLESS: u32 = 1 << 5;
    /// A `{ }` block, rather than `do end`.
    pub const BRACES: u32 = 1 << 6;
    /// An `unless`, whose branches parser stores swapped.
    pub const UNLESS: u32 = 1 << 7;
    /// `begin ... end while c`.
    pub const POST: u32 = 1 << 8;
    /// A node made up from others (a statement list, a block's parameters):
    /// its span's text is not one node's own syntax.
    pub const SYNTHETIC: u32 = 1 << 9;
    /// A string part of a regexp: its text is the pattern as written.
    pub const REGEXP_CONTENT: u32 = 1 << 10;
    /// A string part of an interpolated string or symbol.
    pub const STRING_CONTENT: u32 = 1 << 11;
    /// A word of `%w[]` or `%i[]`: no code can stand in its place.
    pub const WORD: u32 = 1 << 12;
    /// An `elsif`: written as its `if`'s else branch.
    pub const ELSIF: u32 = 1 << 13;
    /// An expression in a slot that holds any expression in parentheses: a
    /// switch can go around it.
    pub const EXPRESSION: u32 = 1 << 14;
    /// A heredoc: its span is its opening, and its body follows its line.
    pub const HEREDOC: u32 = 1 << 15;
    /// A part of a regexp written `%r{...}`: a `/` in it is not escaped.
    pub const PERCENT_REGEXP: u32 = 1 << 16;
}

/// A parser AST node.
#[derive(Debug, Clone)]
pub(crate) struct N {
    pub ty: &'static str,
    pub children: Vec<Child>,
    /// Byte offsets in the file, for a node converted from it.
    pub span: Option<(usize, usize)>,
    pub syntax: u32,
}

impl PartialEq for N {
    fn eq(&self, other: &Self) -> bool {
        self.ty == other.ty && self.children == other.children
    }
}

impl Eq for N {}

impl std::hash::Hash for N {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.ty.hash(state);
        self.children.hash(state);
    }
}

impl N {
    pub(crate) fn new(ty: &'static str, children: Vec<Child>) -> Rc<N> {
        Rc::new(N {
            ty,
            children,
            span: None,
            syntax: 0,
        })
    }

    pub(crate) fn has(&self, syntax: u32) -> bool {
        self.syntax & syntax != 0
    }

    pub(crate) fn child(&self, index: usize) -> &Child {
        self.children.get(index).unwrap_or(&Child::Nil)
    }

    pub(crate) fn node(&self, index: usize) -> Option<&Rc<N>> {
        self.children.get(index).and_then(Child::node)
    }

    /// This node with `children`, and no span: a mutation of it.
    pub(crate) fn with_children(&self, children: Vec<Child>) -> Rc<N> {
        Rc::new(N {
            ty: self.ty,
            children,
            span: None,
            syntax: self.syntax & !syntax::SYNTHETIC,
        })
    }

    pub(crate) fn with_type(&self, ty: &'static str) -> Rc<N> {
        Rc::new(N {
            ty,
            children: self.children.clone(),
            span: None,
            syntax: self.syntax & !syntax::SYNTHETIC,
        })
    }
}

/// `(type child ...)`, as `scripts/mutant-parity.rb` prints it.
pub(crate) fn sexp(node: &N) -> String {
    let mut out = String::new();
    write_sexp(node, &mut out);
    out
}

fn write_sexp(node: &N, out: &mut String) {
    out.push('(');
    out.push_str(node.ty);
    for child in &node.children {
        out.push(' ');
        write_child(child, out);
    }
    out.push(')');
}

fn write_child(child: &Child, out: &mut String) {
    match child {
        Child::Node(node) => write_sexp(node, out),
        Child::Sym(name) => {
            out.push(':');
            out.push_str(name);
        }
        Child::Str(bytes) => {
            out.push_str("s\"");
            for byte in bytes {
                out.push_str(&format!("{byte:02x}"));
            }
            out.push('"');
        }
        Child::Int(value) => out.push_str(&value.to_string()),
        Child::BigInt(digits) => out.push_str(digits),
        Child::Float(bits) => out.push_str(&format!("f{bits:016x}")),
        Child::Raw(text) => out.push_str(text),
        Child::Nil => out.push_str("nil"),
    }
}

/// Why a method's tree could not be built: it holds syntax this port does
/// not convert yet, and its mutants are not enumerated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Unsupported(pub String);

type Built = Result<Rc<N>, Unsupported>;

/// Converts Prism's nodes into parser's, as `Prism::Translation::Parser`'s
/// compiler does.
pub(crate) struct Converter<'s> {
    pub source: &'s [u8],
    /// Anonymous `*`, `**` and `&` the enclosing method forwards.
    forwarding: Vec<&'static str>,
}

fn span(location: &Location<'_>) -> (usize, usize) {
    (location.start_offset(), location.end_offset())
}

fn name(id: &ruby_prism::ConstantId<'_>) -> String {
    String::from_utf8_lossy(id.as_slice()).into_owned()
}

fn sym(text: impl Into<String>) -> Child {
    Child::Sym(text.into())
}

fn node(value: Rc<N>) -> Child {
    Child::Node(value)
}

fn opt(value: Option<Rc<N>>) -> Child {
    value.map_or(Child::Nil, Child::Node)
}

fn made(ty: &'static str, children: Vec<Child>, at: (usize, usize)) -> Rc<N> {
    Rc::new(N {
        ty,
        children,
        span: Some(at),
        syntax: 0,
    })
}

fn made_with(ty: &'static str, children: Vec<Child>, at: (usize, usize), syntax: u32) -> Rc<N> {
    Rc::new(N {
        ty,
        children,
        span: Some(at),
        syntax,
    })
}

fn marked(value: Rc<N>, syntax: u32) -> Rc<N> {
    let mut copy = (*value).clone();
    copy.syntax |= syntax;
    Rc::new(copy)
}

/// A Prism integer in decimal: long division by 10 on its base-2^32 digits.
fn integer_text(value: ruby_prism::Integer<'_>) -> String {
    let (negative, digits) = value.to_u32_digits();
    let mut digits = digits.to_vec();
    let mut decimal = Vec::new();
    loop {
        let mut remainder: u64 = 0;
        for digit in digits.iter_mut().rev() {
            let current = (remainder << 32) | u64::from(*digit);
            *digit = (current / 10) as u32;
            remainder = current % 10;
        }
        decimal.push(b'0' + remainder as u8);
        if digits.iter().all(|digit| *digit == 0) {
            break;
        }
    }
    if negative {
        decimal.push(b'-');
    }
    decimal.reverse();
    String::from_utf8(decimal).expect("digits")
}

/// The integer value of a Prism integer: past `i128`, its decimal text.
fn integer(value: ruby_prism::Integer<'_>) -> Child {
    let text = integer_text(value);
    match text.parse::<i128>() {
        Ok(number) => Child::Int(number),
        Err(_) => Child::BigInt(text),
    }
}

impl<'s> Converter<'s> {
    pub(crate) fn new(source: &'s [u8]) -> Self {
        Self {
            source,
            forwarding: Vec::new(),
        }
    }

    fn text(&self, location: &Location<'_>) -> &'s [u8] {
        &self.source[location.start_offset()..location.end_offset()]
    }

    fn unsupported<T>(&self, what: &str, location: &Location<'_>) -> Result<T, Unsupported> {
        Err(Unsupported(format!(
            "{what} at byte {}",
            location.start_offset()
        )))
    }

    fn all(
        &mut self,
        nodes: impl IntoIterator<Item = Node<'s>>,
    ) -> Result<Vec<Child>, Unsupported> {
        nodes
            .into_iter()
            .map(|each| self.convert(&each).map(node))
            .collect()
    }

    fn optional(&mut self, value: Option<Node<'s>>) -> Result<Child, Unsupported> {
        Ok(match value {
            Some(value) => node(self.convert(&value)?),
            None => Child::Nil,
        })
    }

    /// A statement list: nothing, its one statement, or `(begin ...)`.
    pub(crate) fn statements(
        &mut self,
        statements: Option<ruby_prism::StatementsNode<'s>>,
    ) -> Result<Child, Unsupported> {
        let Some(statements) = statements else {
            return Ok(Child::Nil);
        };
        let mut converted = Vec::new();
        for statement in &statements.body() {
            converted.push(marked(self.convert(&statement)?, syntax::STATEMENT));
        }
        Ok(match converted.len() {
            0 => Child::Nil,
            1 => node(converted.pop().expect("one")),
            _ => node(made_with(
                "begin",
                converted.into_iter().map(node).collect(),
                span(&statements.location()),
                syntax::SYNTHETIC,
            )),
        })
    }

    /// A method's tree.
    pub(crate) fn method(&mut self, def: &ruby_prism::DefNode<'s>) -> Built {
        let at = span(&def.location());
        let saved = std::mem::replace(&mut self.forwarding, forwarding(def.parameters()));
        let built = (|| {
            let args = self.def_args(def)?;
            let body = match def.body() {
                Some(body) => self.body(&body)?,
                None => Child::Nil,
            };
            let endless = if def.equal_loc().is_some() {
                syntax::ENDLESS
            } else {
                0
            };
            Ok(match def.receiver() {
                Some(receiver) => {
                    let receiver = match receiver.as_parentheses_node() {
                        Some(parens) => self.optional(parens.body())?,
                        None => node(self.convert(&receiver)?),
                    };
                    made_with(
                        "defs",
                        vec![receiver, sym(name(&def.name())), node(args), body],
                        at,
                        endless,
                    )
                }
                None => made_with(
                    "def",
                    vec![sym(name(&def.name())), node(args), body],
                    at,
                    endless,
                ),
            })
        })();
        self.forwarding = saved;
        built.map(|tree| mark_slots(&tree, self.source, false))
    }

    /// A method's or block's body: statements, or a `begin` without its
    /// keyword when it rescues or ensures.
    fn body(&mut self, body: &Node<'s>) -> Result<Child, Unsupported> {
        if let Some(statements) = body.as_statements_node() {
            return self.statements(Some(statements));
        }
        if let Some(begin) = body.as_begin_node() {
            return Ok(opt(self.begin_body(&begin)?));
        }
        Ok(node(marked(self.convert(body)?, syntax::STATEMENT)))
    }

    fn def_args(&mut self, def: &ruby_prism::DefNode<'s>) -> Built {
        let children = match def.parameters() {
            Some(parameters) => self.parameters(&parameters)?,
            None => Vec::new(),
        };
        let at = match (def.lparen_loc(), def.rparen_loc(), def.parameters()) {
            (Some(open), Some(close), _) => (open.start_offset(), close.end_offset()),
            (_, _, Some(parameters)) => span(&parameters.location()),
            _ => {
                let end = def.name_loc().end_offset();
                (end, end)
            }
        };
        Ok(made_with("args", children, at, syntax::SYNTHETIC))
    }

    fn parameters(
        &mut self,
        parameters: &ruby_prism::ParametersNode<'s>,
    ) -> Result<Vec<Child>, Unsupported> {
        let mut out = Vec::new();
        for required in &parameters.requireds() {
            out.push(node(self.parameter(&required)?));
        }
        for optional in &parameters.optionals() {
            out.push(node(self.parameter(&optional)?));
        }
        if let Some(rest) = parameters.rest()
            && rest.as_implicit_rest_node().is_none()
        {
            out.push(node(self.parameter(&rest)?));
        }
        for post in &parameters.posts() {
            out.push(node(self.parameter(&post)?));
        }
        for keyword in &parameters.keywords() {
            out.push(node(self.parameter(&keyword)?));
        }
        if let Some(rest) = parameters.keyword_rest() {
            out.push(node(self.parameter(&rest)?));
        }
        if let Some(block) = parameters.block() {
            out.push(node(self.parameter(&block.as_node())?));
        }
        Ok(out)
    }

    fn parameter(&mut self, parameter: &Node<'s>) -> Built {
        let at = span(&parameter.location());
        if let Some(required) = parameter.as_required_parameter_node() {
            return Ok(made("arg", vec![sym(name(&required.name()))], at));
        }
        if let Some(optional) = parameter.as_optional_parameter_node() {
            let value = self.convert(&optional.value())?;
            return Ok(made(
                "optarg",
                vec![sym(name(&optional.name())), node(value)],
                at,
            ));
        }
        if let Some(rest) = parameter.as_rest_parameter_node() {
            let children = rest
                .name()
                .map(|id| vec![sym(name(&id))])
                .unwrap_or_default();
            return Ok(made("restarg", children, at));
        }
        if let Some(keyword) = parameter.as_required_keyword_parameter_node() {
            return Ok(made("kwarg", vec![sym(name(&keyword.name()))], at));
        }
        if let Some(keyword) = parameter.as_optional_keyword_parameter_node() {
            let value = self.convert(&keyword.value())?;
            return Ok(made(
                "kwoptarg",
                vec![sym(name(&keyword.name())), node(value)],
                at,
            ));
        }
        if let Some(rest) = parameter.as_keyword_rest_parameter_node() {
            let children = rest
                .name()
                .map(|id| vec![sym(name(&id))])
                .unwrap_or_default();
            return Ok(made("kwrestarg", children, at));
        }
        if parameter.as_forwarding_parameter_node().is_some() {
            return Ok(made("forward_arg", Vec::new(), at));
        }
        if parameter.as_no_keywords_parameter_node().is_some() {
            return Ok(made("kwnilarg", Vec::new(), at));
        }
        if let Some(block) = parameter.as_block_parameter_node() {
            let name = block.name().map_or(Child::Nil, |id| sym(name(&id)));
            return Ok(made("blockarg", vec![name], at));
        }
        if let Some(block_local) = parameter.as_block_local_variable_node() {
            return Ok(made("shadowarg", vec![sym(name(&block_local.name()))], at));
        }
        if let Some(target) = parameter.as_multi_target_node() {
            let mut elements = Vec::new();
            for left in &target.lefts() {
                elements.push(node(self.parameter(&left)?));
            }
            if let Some(rest) = target.rest()
                && rest.as_implicit_rest_node().is_none()
            {
                elements.push(node(self.parameter(&rest)?));
            }
            for right in &target.rights() {
                elements.push(node(self.parameter(&right)?));
            }
            return Ok(made("mlhs", elements, at));
        }
        if let Some(splat) = parameter.as_splat_node() {
            // A destructuring rest, `|(a, *b)|`.
            let children = match splat.expression() {
                Some(expression) => match expression.as_required_parameter_node() {
                    Some(required) => vec![sym(name(&required.name()))],
                    None => {
                        return self.unsupported("a destructuring splat", &parameter.location());
                    }
                },
                None => Vec::new(),
            };
            return Ok(made("restarg", children, at));
        }
        self.unsupported("a parameter", &parameter.location())
    }

    fn block_args(
        &mut self,
        parameters: Option<Node<'s>>,
        at_default: usize,
    ) -> Result<Child, Unsupported> {
        let Some(parameters) = parameters else {
            return Ok(node(made_with(
                "args",
                Vec::new(),
                (at_default, at_default),
                syntax::SYNTHETIC | syntax::BLOCK_PARAMETERS,
            )));
        };
        if let Some(numbered) = parameters.as_numbered_parameters_node() {
            return Ok(Child::Int(i128::from(numbered.maximum())));
        }
        if parameters.as_it_parameters_node().is_some() {
            return Ok(sym("it"));
        }
        let Some(block) = parameters.as_block_parameters_node() else {
            return self.unsupported("block parameters", &parameters.location());
        };
        let at = span(&block.location());
        let mut children = Vec::new();
        if let Some(inner) = block.parameters() {
            if procarg0(&inner) {
                let first = inner.requireds().first().expect("one required");
                let visited = self.parameter(&first)?;
                let procarg = if visited.ty == "arg" {
                    made(
                        "procarg0",
                        vec![node(visited.clone())],
                        visited.span.expect("span"),
                    )
                } else {
                    let mut copy = (*visited).clone();
                    copy.ty = "procarg0";
                    Rc::new(copy)
                };
                children.push(node(procarg));
            } else {
                children.extend(self.parameters(&inner)?);
            }
        }
        for local in &block.locals() {
            children.push(node(self.parameter(&local)?));
        }
        Ok(node(made_with(
            "args",
            children,
            at,
            syntax::SYNTHETIC | syntax::BLOCK_PARAMETERS,
        )))
    }

    /// `call` with `block` around it, where there is one.
    fn with_block(&mut self, call: Rc<N>, block: Option<Node<'s>>, whole: (usize, usize)) -> Built {
        let Some(block) = block else {
            return Ok(call);
        };
        let Some(block) = block.as_block_node() else {
            return Ok(call);
        };
        let opening = block.opening_loc();
        let braces = self.text(&opening) == b"{";
        let args = self.block_args(block.parameters(), opening.end_offset())?;
        let body = match block.body() {
            Some(body) => self.body(&body)?,
            None => Child::Nil,
        };
        let ty = match &args {
            Child::Int(_) => "numblock",
            Child::Sym(_) => "itblock",
            _ => "block",
        };
        Ok(made_with(
            ty,
            vec![node(call), args, body],
            whole,
            if braces { syntax::BRACES } else { 0 },
        ))
    }

    fn arguments(
        &mut self,
        arguments: Option<ruby_prism::ArgumentsNode<'s>>,
    ) -> Result<Vec<Child>, Unsupported> {
        let Some(arguments) = arguments else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for argument in &arguments.arguments() {
            out.push(node(self.convert(&argument)?));
        }
        Ok(out)
    }

    /// parser's `rewrite_hash_args_to_kwargs`: a braceless hash last (or
    /// before a block pass) is `kwargs`.
    fn kwargs(mut arguments: Vec<Child>) -> Vec<Child> {
        let length = arguments.len();
        let last_is_pass = arguments
            .last()
            .and_then(Child::node)
            .is_some_and(|last| last.ty == "block_pass");
        let index = if last_is_pass {
            length.checked_sub(2)
        } else {
            length.checked_sub(1)
        };
        if let Some(index) = index
            && let Some(hash) = arguments[index].node()
            && hash.ty == "hash"
            && hash.has(syntax::SYNTHETIC)
        {
            let mut copy = (**hash).clone();
            copy.ty = "kwargs";
            arguments[index] = node(Rc::new(copy));
        }
        arguments
    }

    fn call(&mut self, call: &ruby_prism::CallNode<'s>) -> Built {
        let at = span(&call.location());
        let selector = name(&call.name());
        let mut arguments = self.arguments(call.arguments())?;
        let mut block = call.block();
        if let Some(pass) = block.as_ref().and_then(Node::as_block_argument_node) {
            arguments.push(node(self.block_argument(&pass)?));
            block = None;
        }
        let receiver = call.receiver();
        let call_operator = call.call_operator_loc();
        // The whole call without its block, for the call node's own span.
        let call_end = match (&block, call.block()) {
            (Some(_), Some(block_node)) => {
                let _ = block_node;
                call.closing_loc()
                    .map(|loc| loc.end_offset())
                    .or_else(|| call.arguments().map(|args| args.location().end_offset()))
                    .or_else(|| call.message_loc().map(|loc| loc.end_offset()))
                    .unwrap_or(at.1)
            }
            _ => at.1,
        };
        let call_at = (at.0, call_end);
        if call_operator.is_none() {
            match selector.as_str() {
                "!" => {
                    let receiver = match receiver {
                        Some(receiver) => node(self.convert(&receiver)?),
                        None => node(made("begin", Vec::new(), at)),
                    };
                    let not = made("send", vec![receiver, sym("!")], call_at);
                    return self.with_block(not, block, at);
                }
                // parser's `match_op`: a literal regexp's `=~` is a match
                // that may assign its named captures.
                "=~" if receiver
                    .as_ref()
                    .is_some_and(|receiver| receiver.as_regular_expression_node().is_some())
                    && arguments.len() == 1 =>
                {
                    let regexp = self.convert(&receiver.expect("receiver"))?;
                    let mut children = vec![node(regexp)];
                    children.extend(arguments);
                    return Ok(made("match_with_lvasgn", children, call_at));
                }
                "[]" if receiver.is_some() => {
                    let receiver = self.convert(&receiver.expect("receiver"))?;
                    let mut children = vec![node(receiver)];
                    children.extend(arguments);
                    let index = made("index", children, call_at);
                    return self.with_block(index, block, at);
                }
                "[]="
                    if receiver.is_some()
                        && call
                            .message_loc()
                            .is_some_and(|loc| self.text(&loc) != b"[]=")
                        && call.arguments().is_some()
                        && block.is_none()
                        && !call.is_safe_navigation() =>
                {
                    let receiver = self.convert(&receiver.expect("receiver"))?;
                    let mut children = vec![node(receiver)];
                    children.extend(arguments);
                    return Ok(made("indexasgn", children, at));
                }
                _ => {}
            }
        }
        let receiver = match receiver {
            Some(receiver) => node(self.convert(&receiver)?),
            None => Child::Nil,
        };
        let ty = if call.is_safe_navigation() {
            "csend"
        } else {
            "send"
        };
        let mut children = vec![receiver, sym(selector)];
        let attribute_write = call.is_attribute_write()
            && call
                .message_loc()
                .is_some_and(|loc| !self.text(&loc).ends_with(b"="));
        if attribute_write {
            children.extend(arguments);
        } else {
            children.extend(Self::kwargs(arguments));
        }
        let send = made(ty, children, call_at);
        self.with_block(send, block, at)
    }

    fn block_argument(&mut self, pass: &ruby_prism::BlockArgumentNode<'s>) -> Built {
        let value = self.optional(pass.expression())?;
        Ok(made("block_pass", vec![value], span(&pass.location())))
    }

    /// A `begin` body: statements, rescue clauses, else and ensure.
    fn begin_body(
        &mut self,
        begin: &ruby_prism::BeginNode<'s>,
    ) -> Result<Option<Rc<N>>, Unsupported> {
        let mut body = self.statements(begin.statements())?;
        let start = begin
            .statements()
            .map(|statements| statements.location().start_offset())
            .or_else(|| {
                begin
                    .rescue_clause()
                    .map(|clause| clause.location().start_offset())
            })
            .unwrap_or(begin.location().start_offset());
        if let Some(first) = begin.rescue_clause() {
            let mut children = vec![body];
            let mut clause = Some(first);
            let mut end = start;
            while let Some(current) = clause {
                let exceptions = self.all(current.exceptions().iter())?;
                let exceptions = if exceptions.is_empty() {
                    Child::Nil
                } else {
                    node(made_with(
                        "array",
                        exceptions,
                        span(&current.keyword_loc()),
                        syntax::SYNTHETIC,
                    ))
                };
                let reference = match current.reference() {
                    Some(reference) => node(self.target(&reference)?),
                    None => Child::Nil,
                };
                let statements = self.statements(current.statements())?;
                let clause_end = current
                    .statements()
                    .map(|statements| statements.location().end_offset())
                    .or_else(|| {
                        current
                            .reference()
                            .map(|reference| reference.location().end_offset())
                    })
                    .or_else(|| {
                        current
                            .exceptions()
                            .last()
                            .map(|last| last.location().end_offset())
                    })
                    .unwrap_or(current.keyword_loc().end_offset());
                end = clause_end;
                children.push(node(made(
                    "resbody",
                    vec![exceptions, reference, statements],
                    (current.keyword_loc().start_offset(), clause_end),
                )));
                clause = current.subsequent();
            }
            match begin.else_clause() {
                Some(else_clause) => {
                    children.push(self.statements(else_clause.statements())?);
                    end = else_clause
                        .statements()
                        .map_or(else_clause.else_keyword_loc().end_offset(), |statements| {
                            statements.location().end_offset()
                        });
                }
                None => children.push(Child::Nil),
            }
            body = node(made_with(
                "rescue",
                children,
                (start, end),
                syntax::SYNTHETIC,
            ));
        } else if begin.else_clause().is_some() {
            return self.unsupported("an else without rescue", &begin.location());
        }
        if let Some(ensure) = begin.ensure_clause() {
            let ensure_body = self.statements(ensure.statements())?;
            let end = ensure
                .statements()
                .map_or(ensure.ensure_keyword_loc().end_offset(), |statements| {
                    statements.location().end_offset()
                });
            body = node(made_with(
                "ensure",
                vec![body, ensure_body],
                (start, end),
                syntax::SYNTHETIC,
            ));
        }
        Ok(body.node().cloned())
    }

    /// An assignment target: `(lvasgn :a)`, `(ivasgn :@a)`, `(send r :a=)`,
    /// `(indexasgn r i)`, `(mlhs ...)`, `(splat (lvasgn :a))`.
    fn target(&mut self, target: &Node<'s>) -> Built {
        let at = span(&target.location());
        if let Some(local) = target.as_local_variable_target_node() {
            return Ok(made("lvasgn", vec![sym(name(&local.name()))], at));
        }
        if let Some(ivar) = target.as_instance_variable_target_node() {
            return Ok(made("ivasgn", vec![sym(name(&ivar.name()))], at));
        }
        if let Some(gvar) = target.as_global_variable_target_node() {
            return Ok(made("gvasgn", vec![sym(name(&gvar.name()))], at));
        }
        if let Some(cvar) = target.as_class_variable_target_node() {
            return Ok(made("cvasgn", vec![sym(name(&cvar.name()))], at));
        }
        if let Some(call) = target.as_call_target_node() {
            let receiver = self.convert(&call.receiver())?;
            let ty = if call.is_safe_navigation() {
                "csend"
            } else {
                "send"
            };
            return Ok(made(ty, vec![node(receiver), sym(name(&call.name()))], at));
        }
        if let Some(index) = target.as_index_target_node() {
            let receiver = self.convert(&index.receiver())?;
            let mut children = vec![node(receiver)];
            children.extend(self.arguments(index.arguments())?);
            return Ok(made("indexasgn", children, at));
        }
        if let Some(multi) = target.as_multi_target_node() {
            let mut elements = Vec::new();
            for left in &multi.lefts() {
                elements.push(node(self.target(&left)?));
            }
            if let Some(rest) = multi.rest()
                && rest.as_implicit_rest_node().is_none()
            {
                elements.push(node(self.target(&rest)?));
            }
            for right in &multi.rights() {
                elements.push(node(self.target(&right)?));
            }
            return Ok(made("mlhs", elements, at));
        }
        if let Some(splat) = target.as_splat_node() {
            let inner = match splat.expression() {
                Some(expression) => vec![node(self.target(&expression)?)],
                None => Vec::new(),
            };
            return Ok(made("splat", inner, at));
        }
        self.unsupported("an assignment target", &target.location())
    }

    fn string_parts(&mut self, parts: ruby_prism::NodeList<'s>) -> Result<Vec<Child>, Unsupported> {
        let mut out = Vec::new();
        for part in &parts {
            if let Some(string) = part.as_string_node() {
                if string.opening_loc().is_none()
                    && self.text(&string.content_loc()).contains(&b'\n')
                {
                    out.extend(self.lines(&string)?);
                    continue;
                }
                out.push(node(made_with(
                    "str",
                    vec![Child::Str(string.unescaped().to_vec())],
                    span(&part.location()),
                    syntax::STRING_CONTENT,
                )));
                continue;
            }
            out.push(node(self.convert(&part)?));
        }
        Ok(out)
    }

    /// A string's content written over several lines: parser keeps one
    /// `str` per source line.
    fn lines(&mut self, string: &ruby_prism::StringNode<'s>) -> Result<Vec<Child>, Unsupported> {
        let content = self.text(&string.content_loc());
        let unescaped = string.unescaped();
        let escaped_lines = content.split_inclusive(|byte| *byte == b'\n').count();
        let unescaped_lines: Vec<&[u8]> =
            unescaped.split_inclusive(|byte| *byte == b'\n').collect();
        if escaped_lines != unescaped_lines.len() {
            return self.unsupported("a string with escaped line breaks", &string.location());
        }
        let at = span(&string.location());
        Ok(unescaped_lines
            .into_iter()
            .map(|line| {
                node(made_with(
                    "str",
                    vec![Child::Str(line.to_vec())],
                    at,
                    syntax::STRING_CONTENT | syntax::SYNTHETIC,
                ))
            })
            .collect())
    }

    fn string(&mut self, string: &ruby_prism::StringNode<'s>) -> Built {
        let at = span(&string.location());
        if string
            .opening_loc()
            .is_some_and(|opening| self.text(&opening).starts_with(b"<<"))
        {
            let lines = string
                .unescaped()
                .split_inclusive(|byte| *byte == b'\n')
                .map(<[u8]>::to_vec)
                .collect::<Vec<_>>();
            return Ok(heredoc(
                lines.into_iter().map(HeredocPart::Text).collect(),
                at,
            ));
        }
        if self.text(&string.content_loc()).contains(&b'\n') {
            let parts = self.lines(string)?;
            if parts.len() == 1 {
                let only = parts.into_iter().next().expect("one");
                let mut copy = (**only.node().expect("node")).clone();
                copy.span = Some(at);
                return Ok(Rc::new(copy));
            }
            return Ok(made("dstr", parts, at));
        }
        Ok(made(
            "str",
            vec![Child::Str(string.unescaped().to_vec())],
            at,
        ))
    }

    fn regexp(
        &mut self,
        parts: Vec<Child>,
        closing_loc: &Location<'s>,
        at: (usize, usize),
    ) -> Built {
        let closing = self.text(closing_loc);
        let mut options: Vec<char> = closing.iter().skip(1).map(|byte| *byte as char).collect();
        options.sort_unstable();
        options.dedup();
        let regopt = made(
            "regopt",
            options
                .into_iter()
                .map(|option| sym(option.to_string()))
                .collect(),
            (closing_loc.start_offset() + 1, closing_loc.end_offset()),
        );
        let mut children = parts;
        children.push(node(regopt));
        Ok(made("regexp", children, at))
    }

    fn operator_write(
        &mut self,
        target: Rc<N>,
        operator: &[u8],
        value: &Node<'s>,
        at: (usize, usize),
    ) -> Built {
        let value = self.convert(value)?;
        let target = if target.ty == "index" {
            let mut copy = (*target).clone();
            copy.ty = "indexasgn";
            Rc::new(copy)
        } else {
            target
        };
        let operator =
            String::from_utf8_lossy(operator.strip_suffix(b"=").unwrap_or(operator)).into_owned();
        Ok(match operator.as_str() {
            "&&" => made("and_asgn", vec![node(target), node(value)], at),
            "||" => made("or_asgn", vec![node(target), node(value)], at),
            _ => made(
                "op_asgn",
                vec![node(target), sym(operator), node(value)],
                at,
            ),
        })
    }

    fn keyword(
        &mut self,
        ty: &'static str,
        arguments: Option<ruby_prism::ArgumentsNode<'s>>,
        at: (usize, usize),
    ) -> Built {
        let mut children = self.arguments(arguments)?;
        if matches!(ty, "yield" | "super") {
            children = Self::kwargs(children);
        }
        Ok(made(ty, children, at))
    }

    pub(crate) fn convert(&mut self, value: &Node<'s>) -> Built {
        let location = value.location();
        let at = span(&location);
        let leaf = |ty: &'static str, children: Vec<Child>| Ok(made(ty, children, at));
        match value {
            Node::CallNode { .. } => self.call(&value.as_call_node().expect("call")),
            Node::LocalVariableReadNode { .. } => {
                let read = value.as_local_variable_read_node().expect("lvar");
                leaf("lvar", vec![sym(name(&read.name()))])
            }
            Node::ItLocalVariableReadNode { .. } => leaf("lvar", vec![sym("it")]),
            Node::InstanceVariableReadNode { .. } => {
                let read = value.as_instance_variable_read_node().expect("ivar");
                leaf("ivar", vec![sym(name(&read.name()))])
            }
            Node::GlobalVariableReadNode { .. } => {
                let read = value.as_global_variable_read_node().expect("gvar");
                leaf("gvar", vec![sym(name(&read.name()))])
            }
            Node::ClassVariableReadNode { .. } => {
                let read = value.as_class_variable_read_node().expect("cvar");
                leaf("cvar", vec![sym(name(&read.name()))])
            }
            Node::BackReferenceReadNode { .. } => leaf(
                "back_ref",
                vec![sym(
                    String::from_utf8_lossy(self.text(&location)).into_owned()
                )],
            ),
            Node::NumberedReferenceReadNode { .. } => {
                let read = value.as_numbered_reference_read_node().expect("nth_ref");
                leaf("nth_ref", vec![Child::Int(i128::from(read.number()))])
            }
            Node::SelfNode { .. } => leaf("self", Vec::new()),
            Node::NilNode { .. } => leaf("nil", Vec::new()),
            Node::TrueNode { .. } => leaf("true", Vec::new()),
            Node::FalseNode { .. } => leaf("false", Vec::new()),
            Node::SourceFileNode { .. } => leaf("__FILE__", Vec::new()),
            Node::SourceLineNode { .. } => leaf("__LINE__", Vec::new()),
            Node::SourceEncodingNode { .. } => leaf("__ENCODING__", Vec::new()),
            Node::IntegerNode { .. } => {
                let integer_node = value.as_integer_node().expect("int");
                leaf("int", vec![integer(integer_node.value())])
            }
            Node::FloatNode { .. } => {
                let float = value.as_float_node().expect("float");
                leaf("float", vec![Child::Float(float.value().to_bits())])
            }
            Node::RationalNode { .. } => {
                let rational = value.as_rational_node().expect("rational");
                leaf(
                    "rational",
                    vec![Child::Raw(format!(
                        "({}/{})",
                        integer_text(rational.numerator()),
                        integer_text(rational.denominator())
                    ))],
                )
            }
            Node::ImaginaryNode { .. } => {
                let imaginary = value.as_imaginary_node().expect("imaginary");
                match imaginary.numeric().as_integer_node() {
                    Some(integer) => leaf(
                        "complex",
                        vec![Child::Raw(format!(
                            "(0+{}i)",
                            integer_text(integer.value())
                        ))],
                    ),
                    None => self.unsupported("an imaginary literal", &location),
                }
            }
            Node::StringNode { .. } => self.string(&value.as_string_node().expect("str")),
            Node::InterpolatedStringNode { .. } => {
                let string = value.as_interpolated_string_node().expect("dstr");
                if string
                    .opening_loc()
                    .is_some_and(|opening| self.text(&opening).starts_with(b"<<"))
                {
                    let mut parts = Vec::new();
                    for part in &string.parts() {
                        match part.as_string_node() {
                            Some(text) => parts.extend(
                                text.unescaped()
                                    .split_inclusive(|byte| *byte == b'\n')
                                    .map(|line| HeredocPart::Text(line.to_vec())),
                            ),
                            None => parts.push(HeredocPart::Node(self.convert(&part)?)),
                        }
                    }
                    return Ok(heredoc(parts, at));
                }
                let parts = self.string_parts(string.parts())?;
                if parts.len() == 1
                    && parts[0]
                        .node()
                        .is_some_and(|part| matches!(part.ty, "str" | "dstr"))
                {
                    let mut copy = (**parts[0].node().expect("node")).clone();
                    copy.span = Some(at);
                    return Ok(Rc::new(copy));
                }
                leaf("dstr", parts)
            }
            Node::XStringNode { .. } => {
                let string = value.as_x_string_node().expect("xstr");
                let parts = if string.unescaped().is_empty() {
                    Vec::new()
                } else {
                    vec![node(made(
                        "str",
                        vec![Child::Str(string.unescaped().to_vec())],
                        span(&string.content_loc()),
                    ))]
                };
                leaf("xstr", parts)
            }
            Node::InterpolatedXStringNode { .. } => {
                let string = value.as_interpolated_x_string_node().expect("xstr");
                let parts = self.string_parts(string.parts())?;
                leaf("xstr", parts)
            }
            Node::SymbolNode { .. } => {
                let symbol = value.as_symbol_node().expect("sym");
                leaf(
                    "sym",
                    vec![sym(String::from_utf8_lossy(symbol.unescaped()).into_owned())],
                )
            }
            Node::InterpolatedSymbolNode { .. } => {
                let symbol = value.as_interpolated_symbol_node().expect("dsym");
                let parts = self.string_parts(symbol.parts())?;
                leaf("dsym", parts)
            }
            Node::RegularExpressionNode { .. } => {
                let regexp = value.as_regular_expression_node().expect("regexp");
                let parts =
                    if regexp.content_loc().start_offset() == regexp.content_loc().end_offset() {
                        Vec::new()
                    } else if self.text(&regexp.content_loc()).contains(&b'\n') {
                        return self.unsupported("a regexp over several lines", &location);
                    } else {
                        let percent = if self.text(&regexp.opening_loc()).starts_with(b"%") {
                            syntax::PERCENT_REGEXP
                        } else {
                            0
                        };
                        vec![node(made_with(
                            "str",
                            vec![Child::Str(regexp.unescaped().to_vec())],
                            span(&regexp.content_loc()),
                            syntax::REGEXP_CONTENT | percent,
                        ))]
                    };
                self.regexp(parts, &regexp.closing_loc(), at)
            }
            Node::InterpolatedRegularExpressionNode { .. } => {
                let regexp = value
                    .as_interpolated_regular_expression_node()
                    .expect("regexp");
                let percent = if self.text(&regexp.opening_loc()).starts_with(b"%") {
                    syntax::PERCENT_REGEXP
                } else {
                    0
                };
                // Every part is in the pattern's text: a string standing in
                // for an interpolation is written raw too.
                let parts = self
                    .string_parts(regexp.parts())?
                    .into_iter()
                    .map(|part| match part {
                        Child::Node(part) => node(marked(part, syntax::REGEXP_CONTENT | percent)),
                        other => other,
                    })
                    .collect();
                self.regexp(parts, &regexp.closing_loc(), at)
            }
            Node::MatchWriteNode { .. } => {
                let write = value.as_match_write_node().expect("match");
                let call = write.call();
                let regexp = self.convert(&call.receiver().expect("a regexp"))?;
                let argument = call
                    .arguments()
                    .and_then(|arguments| arguments.arguments().first());
                let argument = self.optional(argument)?;
                leaf("match_with_lvasgn", vec![node(regexp), argument])
            }
            Node::EmbeddedStatementsNode { .. } => {
                let embedded = value.as_embedded_statements_node().expect("embedded");
                match self.statements(embedded.statements())? {
                    Child::Nil => leaf("begin", Vec::new()),
                    Child::Node(inner) if inner.ty == "begin" && inner.has(syntax::SYNTHETIC) => {
                        leaf("begin", inner.children.clone())
                    }
                    inner => leaf("begin", vec![inner]),
                }
            }
            Node::EmbeddedVariableNode { .. } => {
                let embedded = value.as_embedded_variable_node().expect("embedded");
                self.convert(&embedded.variable())
            }
            Node::ArrayNode { .. } => {
                let array = value.as_array_node().expect("array");
                let elements = self.array_elements(&array)?;
                if array.opening_loc().is_none() {
                    return Ok(made_with("array", elements, at, syntax::SYNTHETIC));
                }
                leaf("array", elements)
            }
            Node::HashNode { .. } => {
                let hash = value.as_hash_node().expect("hash");
                let elements = self.all(hash.elements().iter())?;
                leaf("hash", elements)
            }
            Node::KeywordHashNode { .. } => {
                let hash = value.as_keyword_hash_node().expect("hash");
                let elements = self.all(hash.elements().iter())?;
                Ok(made_with("hash", elements, at, syntax::SYNTHETIC))
            }
            Node::AssocNode { .. } => {
                let assoc = value.as_assoc_node().expect("pair");
                let key = assoc.key();
                // `{x:}`: the value is the local, the method or the constant
                // the key names, and has no text of its own.
                let shorthand = assoc.value().as_implicit_node().is_some();
                let value_node = match assoc.value().as_implicit_node() {
                    Some(implicit) => {
                        let mut converted = (*self.convert(&implicit.value())?).clone();
                        converted.span = key
                            .as_symbol_node()
                            .and_then(|symbol| symbol.value_loc())
                            .map(|loc| span(&loc));
                        converted.syntax |= syntax::WORD;
                        Rc::new(converted)
                    }
                    None => self.convert(&assoc.value())?,
                };
                let label = assoc.operator_loc().is_none();
                let key_node = self.convert(&key)?;
                let key_node = match (label, key.as_symbol_node()) {
                    (true, Some(symbol)) if symbol.value_loc().is_some() => {
                        let mut copy = (*key_node).clone();
                        // `name:` is the name; `"a b":` its quotes too, as
                        // parser spans it.
                        copy.span = Some(match (symbol.opening_loc(), symbol.closing_loc()) {
                            (Some(opening), Some(closing)) => {
                                (opening.start_offset(), closing.start_offset() + 1)
                            }
                            _ => span(&symbol.value_loc().expect("a label's name")),
                        });
                        copy.syntax |= syntax::LABEL;
                        Rc::new(copy)
                    }
                    (true, None) => marked(key_node, syntax::LABEL),
                    _ => key_node,
                };
                // A shorthand's key is its value's name too: neither is
                // written apart, so a change to either rewrites the pair.
                let key_node = if shorthand {
                    marked(key_node, syntax::WORD)
                } else {
                    key_node
                };
                Ok(made_with(
                    "pair",
                    vec![node(key_node), node(value_node)],
                    at,
                    if label { syntax::LABEL } else { 0 },
                ))
            }
            Node::AssocSplatNode { .. } => {
                let splat = value.as_assoc_splat_node().expect("kwsplat");
                match splat.value() {
                    Some(inner) => {
                        let inner = self.convert(&inner)?;
                        leaf("kwsplat", vec![node(inner)])
                    }
                    None if self.forwarding.contains(&"**") => {
                        leaf("forwarded_kwrestarg", Vec::new())
                    }
                    None => self.unsupported("an anonymous keyword splat", &location),
                }
            }
            Node::SplatNode { .. } => {
                let splat = value.as_splat_node().expect("splat");
                match splat.expression() {
                    Some(inner) => {
                        let inner = self.convert(&inner)?;
                        leaf("splat", vec![node(inner)])
                    }
                    None if self.forwarding.contains(&"*") => leaf("forwarded_restarg", Vec::new()),
                    None => leaf("splat", Vec::new()),
                }
            }
            Node::BlockArgumentNode { .. } => {
                self.block_argument(&value.as_block_argument_node().expect("pass"))
            }
            Node::ForwardingArgumentsNode { .. } => leaf("forwarded_args", Vec::new()),
            Node::RangeNode { .. } => {
                let range = value.as_range_node().expect("range");
                let left = self.optional(range.left())?;
                let right = self.optional(range.right())?;
                leaf(
                    if range.is_exclude_end() {
                        "erange"
                    } else {
                        "irange"
                    },
                    vec![left, right],
                )
            }
            Node::ParenthesesNode { .. } => {
                let parens = value.as_parentheses_node().expect("parens");
                match parens.body() {
                    None => leaf("begin", Vec::new()),
                    Some(body) => {
                        let inner = match body.as_statements_node() {
                            Some(statements) => self.statements(Some(statements))?,
                            None => node(self.convert(&body)?),
                        };
                        match inner {
                            Child::Node(inner)
                                if (inner.ty == "begin" && inner.has(syntax::SYNTHETIC))
                                    || inner.ty == "mlhs" =>
                            {
                                leaf(inner.ty, inner.children.clone())
                            }
                            Child::Nil => leaf("begin", Vec::new()),
                            inner => leaf("begin", vec![inner]),
                        }
                    }
                }
            }
            Node::StatementsNode { .. } => {
                let statements = value.as_statements_node().expect("statements");
                match self.statements(Some(statements))? {
                    Child::Node(inner) => Ok(inner),
                    _ => leaf("begin", Vec::new()),
                }
            }
            Node::BeginNode { .. } => {
                let begin = value.as_begin_node().expect("begin");
                let body = self.begin_body(&begin)?;
                if begin.begin_keyword_loc().is_none() {
                    return match body {
                        Some(body) => Ok(body),
                        None => leaf("begin", Vec::new()),
                    };
                }
                match body {
                    None => leaf("kwbegin", Vec::new()),
                    Some(body) if body.ty == "begin" && body.has(syntax::SYNTHETIC) => {
                        leaf("kwbegin", body.children.clone())
                    }
                    Some(body) => leaf("kwbegin", vec![node(body)]),
                }
            }
            Node::IfNode { .. } => {
                let if_node = value.as_if_node().expect("if");
                let predicate = self.convert(&if_node.predicate())?;
                let then_branch = self.statements(if_node.statements())?;
                let else_branch = match if_node.subsequent() {
                    Some(subsequent) => match subsequent.as_else_node() {
                        Some(else_node) => self.statements(else_node.statements())?,
                        None => node(self.convert(&subsequent)?),
                    },
                    None => Child::Nil,
                };
                let form = match if_node.if_keyword_loc() {
                    None => syntax::TERNARY,
                    Some(keyword) if keyword.start_offset() != location.start_offset() => {
                        syntax::MODIFIER
                    }
                    Some(_) => 0,
                };
                // An `elsif` ends where its last branch does: its `end` is
                // the whole `if`'s.
                let (at, form) = match if_node.if_keyword_loc() {
                    Some(keyword) if self.text(&keyword) == b"elsif" => {
                        ((at.0, elsif_end(&if_node)), form | syntax::ELSIF)
                    }
                    _ => (at, form),
                };
                Ok(made_with(
                    "if",
                    vec![node(predicate), then_branch, else_branch],
                    at,
                    form,
                ))
            }
            Node::UnlessNode { .. } => {
                let unless = value.as_unless_node().expect("unless");
                let predicate = self.convert(&unless.predicate())?;
                let then_branch = self.statements(unless.statements())?;
                let else_branch = match unless.else_clause() {
                    Some(else_node) => self.statements(else_node.statements())?,
                    None => Child::Nil,
                };
                let form = if unless.keyword_loc().start_offset() != location.start_offset() {
                    syntax::MODIFIER
                } else {
                    0
                };
                Ok(made_with(
                    "if",
                    vec![node(predicate), else_branch, then_branch],
                    at,
                    form | syntax::UNLESS,
                ))
            }
            Node::WhileNode { .. } | Node::UntilNode { .. } => {
                let (keyword, predicate, statements, modifier, ty) = match value.as_while_node() {
                    Some(each) => (
                        each.keyword_loc(),
                        each.predicate(),
                        each.statements(),
                        each.is_begin_modifier(),
                        "while",
                    ),
                    None => {
                        let each = value.as_until_node().expect("until");
                        (
                            each.keyword_loc(),
                            each.predicate(),
                            each.statements(),
                            each.is_begin_modifier(),
                            "until",
                        )
                    }
                };
                let predicate = self.convert(&predicate)?;
                let body = self.statements(statements)?;
                let post = modifier;
                let modifier_form = keyword.start_offset() != location.start_offset();
                let ty = match (ty, post) {
                    ("while", true) => "while_post",
                    ("until", true) => "until_post",
                    (ty, _) => ty,
                };
                let mut syntax = 0;
                if modifier_form {
                    syntax |= syntax::MODIFIER;
                }
                if post {
                    syntax |= syntax::POST;
                }
                Ok(made_with(ty, vec![node(predicate), body], at, syntax))
            }
            Node::ForNode { .. } => {
                let for_node = value.as_for_node().expect("for");
                let index = self.target(&for_node.index())?;
                let collection = self.convert(&for_node.collection())?;
                let body = self.statements(for_node.statements())?;
                leaf("for", vec![node(index), node(collection), body])
            }
            Node::CaseNode { .. } => {
                let case = value.as_case_node().expect("case");
                let mut children = vec![self.optional(case.predicate())?];
                for condition in &case.conditions() {
                    let when = condition.as_when_node().expect("when");
                    let mut parts = self.all(when.conditions().iter())?;
                    parts.push(self.statements(when.statements())?);
                    let end = when
                        .statements()
                        .map(|statements| statements.location().end_offset())
                        .unwrap_or(condition.location().end_offset());
                    children.push(node(made(
                        "when",
                        parts,
                        (condition.location().start_offset(), end),
                    )));
                }
                children.push(match case.else_clause() {
                    Some(else_node) => self.statements(else_node.statements())?,
                    None => Child::Nil,
                });
                leaf("case", children)
            }
            Node::CaseMatchNode { .. } => {
                let case = value.as_case_match_node().expect("case_match");
                let mut children = vec![self.optional(case.predicate())?];
                for condition in &case.conditions() {
                    let in_node = condition.as_in_node().expect("in");
                    let (pattern, guard) = match (
                        in_node.pattern().as_if_node(),
                        in_node.pattern().as_unless_node(),
                    ) {
                        (Some(guarded), _) => {
                            let pattern = self.pattern_statements(guarded.statements())?;
                            let predicate = self.convert(&guarded.predicate())?;
                            (
                                pattern,
                                node(made(
                                    "if_guard",
                                    vec![node(predicate)],
                                    span(&guarded.predicate().location()),
                                )),
                            )
                        }
                        (_, Some(guarded)) => {
                            let pattern = self.pattern_statements(guarded.statements())?;
                            let predicate = self.convert(&guarded.predicate())?;
                            (
                                pattern,
                                node(made(
                                    "unless_guard",
                                    vec![node(predicate)],
                                    span(&guarded.predicate().location()),
                                )),
                            )
                        }
                        _ => (self.pattern(&in_node.pattern())?, Child::Nil),
                    };
                    let body = self.statements(in_node.statements())?;
                    let end = in_node
                        .statements()
                        .map(|statements| statements.location().end_offset())
                        .unwrap_or(condition.location().end_offset());
                    children.push(node(made(
                        "in_pattern",
                        vec![node(pattern), guard, body],
                        (condition.location().start_offset(), end),
                    )));
                }
                children.push(match case.else_clause() {
                    Some(else_node) => self.statements(else_node.statements())?,
                    None => Child::Nil,
                });
                leaf("case_match", children)
            }
            Node::MatchPredicateNode { .. } => {
                let predicate = value.as_match_predicate_node().expect("in");
                let expression = self.convert(&predicate.value())?;
                let pattern = self.pattern(&predicate.pattern())?;
                leaf("match_pattern_p", vec![node(expression), node(pattern)])
            }
            Node::MatchRequiredNode { .. } => {
                let required = value.as_match_required_node().expect("=>");
                let expression = self.convert(&required.value())?;
                let pattern = self.pattern(&required.pattern())?;
                leaf("match_pattern", vec![node(expression), node(pattern)])
            }
            Node::AndNode { .. } => {
                let and = value.as_and_node().expect("and");
                let left = self.convert(&and.left())?;
                let right = self.convert(&and.right())?;
                leaf("and", vec![node(left), node(right)])
            }
            Node::OrNode { .. } => {
                let or = value.as_or_node().expect("or");
                let left = self.convert(&or.left())?;
                let right = self.convert(&or.right())?;
                leaf("or", vec![node(left), node(right)])
            }
            Node::LocalVariableWriteNode { .. } => {
                let write = value.as_local_variable_write_node().expect("lvasgn");
                let assigned = self.convert(&write.value())?;
                leaf("lvasgn", vec![sym(name(&write.name())), node(assigned)])
            }
            Node::InstanceVariableWriteNode { .. } => {
                let write = value.as_instance_variable_write_node().expect("ivasgn");
                let assigned = self.convert(&write.value())?;
                leaf("ivasgn", vec![sym(name(&write.name())), node(assigned)])
            }
            Node::GlobalVariableWriteNode { .. } => {
                let write = value.as_global_variable_write_node().expect("gvasgn");
                let assigned = self.convert(&write.value())?;
                leaf("gvasgn", vec![sym(name(&write.name())), node(assigned)])
            }
            Node::ClassVariableWriteNode { .. } => {
                let write = value.as_class_variable_write_node().expect("cvasgn");
                let assigned = self.convert(&write.value())?;
                leaf("cvasgn", vec![sym(name(&write.name())), node(assigned)])
            }
            Node::LocalVariableOperatorWriteNode { .. } => {
                let write = value.as_local_variable_operator_write_node().expect("op");
                let target = made(
                    "lvasgn",
                    vec![sym(name(&write.name()))],
                    span(&write.name_loc()),
                );
                let operator = self.text(&write.binary_operator_loc()).to_vec();
                self.operator_write(target, &operator, &write.value(), at)
            }
            Node::LocalVariableOrWriteNode { .. } => {
                let write = value.as_local_variable_or_write_node().expect("or");
                let target = made(
                    "lvasgn",
                    vec![sym(name(&write.name()))],
                    span(&write.name_loc()),
                );
                self.operator_write(target, b"||=", &write.value(), at)
            }
            Node::LocalVariableAndWriteNode { .. } => {
                let write = value.as_local_variable_and_write_node().expect("and");
                let target = made(
                    "lvasgn",
                    vec![sym(name(&write.name()))],
                    span(&write.name_loc()),
                );
                self.operator_write(target, b"&&=", &write.value(), at)
            }
            Node::InstanceVariableOperatorWriteNode { .. } => {
                let write = value
                    .as_instance_variable_operator_write_node()
                    .expect("op");
                let target = made(
                    "ivasgn",
                    vec![sym(name(&write.name()))],
                    span(&write.name_loc()),
                );
                let operator = self.text(&write.binary_operator_loc()).to_vec();
                self.operator_write(target, &operator, &write.value(), at)
            }
            Node::InstanceVariableOrWriteNode { .. } => {
                let write = value.as_instance_variable_or_write_node().expect("or");
                let target = made(
                    "ivasgn",
                    vec![sym(name(&write.name()))],
                    span(&write.name_loc()),
                );
                self.operator_write(target, b"||=", &write.value(), at)
            }
            Node::InstanceVariableAndWriteNode { .. } => {
                let write = value.as_instance_variable_and_write_node().expect("and");
                let target = made(
                    "ivasgn",
                    vec![sym(name(&write.name()))],
                    span(&write.name_loc()),
                );
                self.operator_write(target, b"&&=", &write.value(), at)
            }
            Node::GlobalVariableOperatorWriteNode { .. } => {
                let write = value.as_global_variable_operator_write_node().expect("op");
                let target = made(
                    "gvasgn",
                    vec![sym(name(&write.name()))],
                    span(&write.name_loc()),
                );
                let operator = self.text(&write.binary_operator_loc()).to_vec();
                self.operator_write(target, &operator, &write.value(), at)
            }
            Node::GlobalVariableOrWriteNode { .. } => {
                let write = value.as_global_variable_or_write_node().expect("or");
                let target = made(
                    "gvasgn",
                    vec![sym(name(&write.name()))],
                    span(&write.name_loc()),
                );
                self.operator_write(target, b"||=", &write.value(), at)
            }
            Node::GlobalVariableAndWriteNode { .. } => {
                let write = value.as_global_variable_and_write_node().expect("and");
                let target = made(
                    "gvasgn",
                    vec![sym(name(&write.name()))],
                    span(&write.name_loc()),
                );
                self.operator_write(target, b"&&=", &write.value(), at)
            }
            Node::ClassVariableOperatorWriteNode { .. } => {
                let write = value.as_class_variable_operator_write_node().expect("op");
                let target = made(
                    "cvasgn",
                    vec![sym(name(&write.name()))],
                    span(&write.name_loc()),
                );
                let operator = self.text(&write.binary_operator_loc()).to_vec();
                self.operator_write(target, &operator, &write.value(), at)
            }
            Node::ClassVariableOrWriteNode { .. } => {
                let write = value.as_class_variable_or_write_node().expect("or");
                let target = made(
                    "cvasgn",
                    vec![sym(name(&write.name()))],
                    span(&write.name_loc()),
                );
                self.operator_write(target, b"||=", &write.value(), at)
            }
            Node::ClassVariableAndWriteNode { .. } => {
                let write = value.as_class_variable_and_write_node().expect("and");
                let target = made(
                    "cvasgn",
                    vec![sym(name(&write.name()))],
                    span(&write.name_loc()),
                );
                self.operator_write(target, b"&&=", &write.value(), at)
            }
            // A constant's own writes are dynamic constant assignments in a
            // method, which Ruby refuses: no method holds one.
            Node::CallOperatorWriteNode { .. }
            | Node::CallOrWriteNode { .. }
            | Node::CallAndWriteNode { .. } => {
                let (receiver, safe, read_name, message, operator, assigned) =
                    if let Some(write) = value.as_call_operator_write_node() {
                        (
                            write.receiver(),
                            write.is_safe_navigation(),
                            name(&write.read_name()),
                            write.message_loc(),
                            self.text(&write.binary_operator_loc()).to_vec(),
                            write.value(),
                        )
                    } else if let Some(write) = value.as_call_or_write_node() {
                        (
                            write.receiver(),
                            write.is_safe_navigation(),
                            name(&write.read_name()),
                            write.message_loc(),
                            b"||=".to_vec(),
                            write.value(),
                        )
                    } else {
                        let write = value.as_call_and_write_node().expect("and");
                        (
                            write.receiver(),
                            write.is_safe_navigation(),
                            name(&write.read_name()),
                            write.message_loc(),
                            b"&&=".to_vec(),
                            write.value(),
                        )
                    };
                let receiver = self.optional(receiver)?;
                let end = message.map_or(at.0, |message| message.end_offset());
                let target = made(
                    if safe { "csend" } else { "send" },
                    vec![receiver, sym(read_name)],
                    (at.0, end),
                );
                self.operator_write(target, &operator, &assigned, at)
            }
            Node::IndexOperatorWriteNode { .. }
            | Node::IndexOrWriteNode { .. }
            | Node::IndexAndWriteNode { .. } => {
                let (receiver, arguments, closing, operator, assigned) =
                    if let Some(write) = value.as_index_operator_write_node() {
                        (
                            write.receiver(),
                            write.arguments(),
                            write.closing_loc(),
                            self.text(&write.binary_operator_loc()).to_vec(),
                            write.value(),
                        )
                    } else if let Some(write) = value.as_index_or_write_node() {
                        (
                            write.receiver(),
                            write.arguments(),
                            write.closing_loc(),
                            b"||=".to_vec(),
                            write.value(),
                        )
                    } else {
                        let write = value.as_index_and_write_node().expect("and");
                        (
                            write.receiver(),
                            write.arguments(),
                            write.closing_loc(),
                            b"&&=".to_vec(),
                            write.value(),
                        )
                    };
                let mut children = vec![self.optional(receiver)?];
                children.extend(self.arguments(arguments)?);
                let target = made("index", children, (at.0, closing.end_offset()));
                self.operator_write(target, &operator, &assigned, at)
            }
            Node::MultiWriteNode { .. } => {
                let write = value.as_multi_write_node().expect("masgn");
                let mut elements = Vec::new();
                for left in &write.lefts() {
                    elements.push(node(self.target(&left)?));
                }
                if let Some(rest) = write.rest()
                    && rest.as_implicit_rest_node().is_none()
                {
                    elements.push(node(self.target(&rest)?));
                }
                for right in &write.rights() {
                    elements.push(node(self.target(&right)?));
                }
                if elements.len() == 1
                    && write.rest().is_none()
                    && elements[0].node().is_some_and(|only| only.ty == "mlhs")
                {
                    elements = elements[0].node().expect("mlhs").children.clone();
                }
                let lhs_end = write.operator_loc().start_offset();
                let mlhs = made_with("mlhs", elements, (at.0, lhs_end), syntax::SYNTHETIC);
                let assigned = self.convert(&write.value())?;
                leaf("masgn", vec![node(mlhs), node(assigned)])
            }
            Node::ConstantReadNode { .. } => {
                let constant = value.as_constant_read_node().expect("const");
                leaf("const", vec![Child::Nil, sym(name(&constant.name()))])
            }
            Node::ConstantPathNode { .. } => {
                let path = value.as_constant_path_node().expect("const");
                let constant = path.name().map(|id| name(&id)).unwrap_or_default();
                let parent = match path.parent() {
                    Some(parent) => node(self.convert(&parent)?),
                    None => node(made("cbase", Vec::new(), span(&path.delimiter_loc()))),
                };
                leaf("const", vec![parent, sym(constant)])
            }
            Node::ReturnNode { .. } => {
                let keyword = value.as_return_node().expect("return");
                self.keyword("return", keyword.arguments(), at)
            }
            Node::NextNode { .. } => {
                let keyword = value.as_next_node().expect("next");
                self.keyword("next", keyword.arguments(), at)
            }
            Node::BreakNode { .. } => {
                let keyword = value.as_break_node().expect("break");
                self.keyword("break", keyword.arguments(), at)
            }
            Node::YieldNode { .. } => {
                let keyword = value.as_yield_node().expect("yield");
                self.keyword("yield", keyword.arguments(), at)
            }
            Node::RedoNode { .. } => leaf("redo", Vec::new()),
            Node::RetryNode { .. } => leaf("retry", Vec::new()),
            Node::SuperNode { .. } => {
                let super_node = value.as_super_node().expect("super");
                let mut arguments = self.arguments(super_node.arguments())?;
                let mut block = super_node.block();
                if let Some(pass) = block.as_ref().and_then(Node::as_block_argument_node) {
                    arguments.push(node(self.block_argument(&pass)?));
                    block = None;
                }
                let end = super_node
                    .rparen_loc()
                    .map(|loc| loc.end_offset())
                    .or_else(|| {
                        super_node
                            .arguments()
                            .map(|args| args.location().end_offset())
                    })
                    .unwrap_or(super_node.keyword_loc().end_offset());
                let call = made(
                    "super",
                    Self::kwargs(arguments),
                    (at.0, if block.is_some() { end } else { at.1 }),
                );
                self.with_block(call, block, at)
            }
            Node::ForwardingSuperNode { .. } => {
                let super_node = value.as_forwarding_super_node().expect("zsuper");
                let block = super_node.block().map(|block| block.as_node());
                let call = made("zsuper", Vec::new(), (at.0, at.0 + 5));
                if block.is_none() {
                    return Ok(made("zsuper", Vec::new(), at));
                }
                self.with_block(call, block, at)
            }
            Node::DefinedNode { .. } => {
                let defined = value.as_defined_node().expect("defined?");
                let inner = self.convert(&defined.value())?;
                leaf("defined?", vec![node(inner)])
            }
            Node::LambdaNode { .. } => {
                let lambda = value.as_lambda_node().expect("lambda");
                let call = made("lambda", Vec::new(), span(&lambda.operator_loc()));
                let args = match lambda.parameters() {
                    None => node(made_with(
                        "args",
                        Vec::new(),
                        (
                            lambda.operator_loc().end_offset(),
                            lambda.operator_loc().end_offset(),
                        ),
                        syntax::SYNTHETIC,
                    )),
                    Some(parameters) => {
                        if let Some(numbered) = parameters.as_numbered_parameters_node() {
                            Child::Int(i128::from(numbered.maximum()))
                        } else if parameters.as_it_parameters_node().is_some() {
                            sym("it")
                        } else {
                            let block = parameters.as_block_parameters_node().expect("parameters");
                            let mut children = match block.parameters() {
                                Some(inner) => self.parameters(&inner)?,
                                None => Vec::new(),
                            };
                            for local in &block.locals() {
                                children.push(node(self.parameter(&local)?));
                            }
                            node(made_with(
                                "args",
                                children,
                                span(&block.location()),
                                syntax::SYNTHETIC,
                            ))
                        }
                    }
                };
                let body = match lambda.body() {
                    Some(body) => self.body(&body)?,
                    None => Child::Nil,
                };
                let ty = match &args {
                    Child::Int(_) => "numblock",
                    Child::Sym(_) => "itblock",
                    _ => "block",
                };
                let braces = self.text(&lambda.opening_loc()) == b"{";
                Ok(made_with(
                    ty,
                    vec![node(call), args, body],
                    at,
                    if braces { syntax::BRACES } else { 0 },
                ))
            }
            Node::RescueModifierNode { .. } => {
                let rescue = value.as_rescue_modifier_node().expect("rescue");
                let expression = self.convert(&rescue.expression())?;
                let fallback = self.convert(&rescue.rescue_expression())?;
                let resbody = made(
                    "resbody",
                    vec![Child::Nil, Child::Nil, node(fallback)],
                    (rescue.keyword_loc().start_offset(), at.1),
                );
                leaf("rescue", vec![node(expression), node(resbody), Child::Nil])
            }
            Node::ShareableConstantNode { .. } => {
                let shareable = value.as_shareable_constant_node().expect("shareable");
                self.convert(&shareable.write())
            }
            Node::AliasMethodNode { .. } => {
                let alias = value.as_alias_method_node().expect("alias");
                let new_name = self.convert(&alias.new_name())?;
                let old_name = self.convert(&alias.old_name())?;
                leaf("alias", vec![node(new_name), node(old_name)])
            }
            Node::UndefNode { .. } => {
                let undef = value.as_undef_node().expect("undef");
                let names = self.all(undef.names().iter())?;
                leaf("undef", names)
            }
            Node::DefNode { .. } => {
                let def = value.as_def_node().expect("def");
                self.method(&def)
            }
            Node::FlipFlopNode { .. } => self.unsupported("a flip-flop", &location),
            _ => self.unsupported("syntax this port does not convert", &location),
        }
    }

    fn array_elements(
        &mut self,
        array: &ruby_prism::ArrayNode<'s>,
    ) -> Result<Vec<Child>, Unsupported> {
        let words = array.opening_loc().is_some_and(|opening| {
            let text = self.text(&opening);
            text.starts_with(b"%w")
                || text.starts_with(b"%W")
                || text.starts_with(b"%i")
                || text.starts_with(b"%I")
        });
        let mut elements = Vec::new();
        for element in &array.elements() {
            if words && let Some(string) = element.as_string_node() {
                elements.push(node(made_with(
                    "str",
                    vec![Child::Str(string.unescaped().to_vec())],
                    span(&element.location()),
                    syntax::WORD,
                )));
                continue;
            }
            let converted = self.convert(&element)?;
            elements.push(node(if words {
                marked(converted, syntax::WORD)
            } else {
                converted
            }));
        }
        Ok(elements)
    }

    fn pattern_statements(&mut self, statements: Option<ruby_prism::StatementsNode<'s>>) -> Built {
        let Some(statements) = statements else {
            return Err(Unsupported("an empty guarded pattern".into()));
        };
        let body = statements.body();
        if body.iter().count() != 1 {
            return self.unsupported("a guarded pattern", &statements.location());
        }
        self.pattern(&body.first().expect("one"))
    }

    /// A pattern of `case ... in`, `in` and `=>`.
    fn pattern(&mut self, pattern: &Node<'s>) -> Built {
        let location = pattern.location();
        let at = span(&location);
        if let Some(local) = pattern.as_local_variable_target_node() {
            return Ok(made("match_var", vec![sym(name(&local.name()))], at));
        }
        if let Some(capture) = pattern.as_capture_pattern_node() {
            let value = self.pattern(&capture.value())?;
            let target = self.pattern(&capture.target().as_node())?;
            return Ok(made("match_as", vec![node(value), node(target)], at));
        }
        if let Some(alternation) = pattern.as_alternation_pattern_node() {
            let left = self.pattern(&alternation.left())?;
            let right = self.pattern(&alternation.right())?;
            return Ok(made("match_alt", vec![node(left), node(right)], at));
        }
        if let Some(pinned) = pattern.as_pinned_variable_node() {
            let variable = self.convert(&pinned.variable())?;
            return Ok(made("pin", vec![node(variable)], at));
        }
        if let Some(array) = pattern.as_array_pattern_node() {
            if array.constant().is_some() {
                return self.unsupported("a constant array pattern", &location);
            }
            let mut elements = Vec::new();
            for required in &array.requireds() {
                elements.push(node(self.pattern(&required)?));
            }
            if let Some(rest) = array.rest() {
                if rest.as_implicit_rest_node().is_some() {
                    return self.unsupported("an implicit rest pattern", &location);
                }
                elements.push(node(self.pattern(&rest)?));
            }
            for post in &array.posts() {
                elements.push(node(self.pattern(&post)?));
            }
            return Ok(made("array_pattern", elements, at));
        }
        if let Some(splat) = pattern.as_splat_node() {
            let children = match splat.expression() {
                Some(expression) => vec![node(self.pattern(&expression)?)],
                None => Vec::new(),
            };
            return Ok(made("match_rest", children, at));
        }
        if let Some(hash) = pattern.as_hash_pattern_node() {
            if hash.constant().is_some() {
                return self.unsupported("a constant hash pattern", &location);
            }
            let mut elements = Vec::new();
            for element in &hash.elements() {
                let Some(assoc) = element.as_assoc_node() else {
                    return self.unsupported("a hash pattern element", &location);
                };
                let key = assoc.key();
                let Some(symbol) = key.as_symbol_node() else {
                    return self.unsupported("a hash pattern key", &location);
                };
                let key_name = String::from_utf8_lossy(symbol.unescaped()).into_owned();
                if assoc.value().as_implicit_node().is_some() {
                    elements.push(node(made(
                        "match_var",
                        vec![sym(key_name)],
                        span(&element.location()),
                    )));
                    continue;
                }
                let value = self.pattern(&assoc.value())?;
                let key_at = symbol
                    .value_loc()
                    .map_or_else(|| span(&key.location()), |value_loc| span(&value_loc));
                let key_node = made_with("sym", vec![sym(key_name)], key_at, syntax::LABEL);
                elements.push(node(made_with(
                    "pair",
                    vec![node(key_node), node(value)],
                    span(&element.location()),
                    syntax::LABEL,
                )));
            }
            if let Some(rest) = hash.rest() {
                if let Some(splat) = rest.as_assoc_splat_node() {
                    let children = match splat.value() {
                        Some(value) => vec![node(self.pattern(&value)?)],
                        None => Vec::new(),
                    };
                    elements.push(node(made("match_rest", children, span(&rest.location()))));
                } else if rest.as_no_keywords_parameter_node().is_some() {
                    elements.push(node(made(
                        "match_nil_pattern",
                        Vec::new(),
                        span(&rest.location()),
                    )));
                } else {
                    return self.unsupported("a hash pattern rest", &location);
                }
            }
            return Ok(made("hash_pattern", elements, at));
        }
        // A value pattern: a literal, a constant, a range.
        self.convert(pattern)
    }
}

enum HeredocPart {
    Text(Vec<u8>),
    Node(Rc<N>),
}

/// A heredoc as the compiler's `visit_heredoc` builds it: one `str` per line
/// (a line joins the one before only where that has no line break yet), no
/// empty `str`, and `str` alone where one part is left. Its span is its
/// opening only: its body lies after the line, so it is never printed as
/// written (`HEREDOC`).
fn heredoc(parts: Vec<HeredocPart>, at: (usize, usize)) -> Rc<N> {
    let mut children: Vec<Child> = Vec::new();
    for part in parts {
        match part {
            HeredocPart::Text(text) if text.is_empty() => {}
            HeredocPart::Text(text) => {
                let joins = children.last().and_then(Child::node).is_some_and(|last| {
                    last.ty == "str"
                        && matches!(last.child(0), Child::Str(bytes) if !bytes.ends_with(b"\n"))
                });
                if joins {
                    let Some(Child::Node(last)) = children.pop() else {
                        unreachable!()
                    };
                    let Child::Str(mut bytes) = last.child(0).clone() else {
                        unreachable!()
                    };
                    bytes.extend_from_slice(&text);
                    children.push(node(made_with(
                        "str",
                        vec![Child::Str(bytes)],
                        at,
                        syntax::HEREDOC,
                    )));
                } else {
                    children.push(node(made_with(
                        "str",
                        vec![Child::Str(text)],
                        at,
                        syntax::HEREDOC,
                    )));
                }
            }
            HeredocPart::Node(value) => children.push(node(value)),
        }
    }
    if children.len() == 1
        && let Some(only) = children[0].node()
        && only.ty == "str"
    {
        return made_with("str", only.children.clone(), at, syntax::HEREDOC);
    }
    made_with("dstr", children, at, syntax::HEREDOC)
}

fn elsif_end(if_node: &ruby_prism::IfNode<'_>) -> usize {
    match if_node.subsequent() {
        Some(subsequent) => match subsequent.as_else_node() {
            Some(else_node) => else_node
                .statements()
                .map_or(else_node.else_keyword_loc().end_offset(), |statements| {
                    statements.location().end_offset()
                }),
            None => subsequent
                .as_if_node()
                .map_or(subsequent.location().end_offset(), |inner| {
                    elsif_end(&inner)
                }),
        },
        None => if_node.statements().map_or_else(
            || {
                if_node
                    .then_keyword_loc()
                    .map_or(if_node.predicate().location().end_offset(), |then| {
                        then.end_offset()
                    })
            },
            |statements| statements.location().end_offset(),
        ),
    }
}

fn procarg0(parameters: &ruby_prism::ParametersNode<'_>) -> bool {
    parameters.requireds().iter().count() == 1
        && parameters.optionals().iter().count() == 0
        && parameters.rest().is_none()
        && parameters.posts().iter().count() == 0
        && parameters.keywords().iter().count() == 0
        && parameters.keyword_rest().is_none()
        && parameters.block().is_none()
}

/// The anonymous arguments a method forwards, as the compiler's
/// `find_forwarding`.
fn forwarding(parameters: Option<ruby_prism::ParametersNode<'_>>) -> Vec<&'static str> {
    let Some(parameters) = parameters else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if parameters
        .rest()
        .and_then(|rest| rest.as_rest_parameter_node())
        .is_some_and(|rest| rest.name().is_none())
    {
        out.push("*");
    }
    if let Some(rest) = parameters.keyword_rest() {
        if rest
            .as_keyword_rest_parameter_node()
            .is_some_and(|rest| rest.name().is_none())
        {
            out.push("**");
        }
        if rest.as_forwarding_parameter_node().is_some() {
            out.push("&");
            out.push("...");
        }
    }
    if parameters
        .block()
        .is_some_and(|block| block.name().is_none())
        && !out.contains(&"&")
    {
        out.push("&");
    }
    out
}

/// `tree` with each node in an expression slot marked `EXPRESSION`: a
/// condition, an operand, a receiver, an assigned or returned value, an
/// argument in parentheses, an element or a hash value.
fn mark_slots(tree: &Rc<N>, source: &[u8], pattern: bool) -> Rc<N> {
    let parenthesized_call = |call: &N| {
        call.span
            .is_some_and(|(start, end)| source[start..end].ends_with(b")"))
    };
    // A pattern is no expression: nothing in it is switched.
    let slots: Vec<usize> = if pattern {
        Vec::new()
    } else {
        match tree.ty {
            "if" | "while" | "until" => vec![0],
            "and" | "or" => vec![0, 1],
            "send" | "csend" => {
                let selector = tree.child(1).sym().unwrap_or_default();
                let operator = !selector
                    .chars()
                    .next()
                    .is_some_and(|first| first.is_alphanumeric() || first == '_');
                let mut slots = Vec::new();
                if !tree.child(0).is_nil() {
                    slots.push(0);
                }
                if (operator && selector != "[]=") || parenthesized_call(tree) {
                    slots.extend(2..tree.children.len());
                }
                slots
            }
            "lvasgn" | "ivasgn" | "gvasgn" | "cvasgn" => {
                vec![tree.children.len().saturating_sub(1)]
            }
            "op_asgn" => vec![2],
            "or_asgn" | "and_asgn" => vec![1],
            "return" | "next" | "break" | "index" => (0..tree.children.len()).collect(),
            "array" if !tree.has(syntax::SYNTHETIC) => (0..tree.children.len()).collect(),
            "pair" | "optarg" | "kwoptarg" => vec![1],
            _ => Vec::new(),
        }
    };
    let pattern_child = |index: usize| {
        pattern
            || (tree.ty == "in_pattern" && index == 0)
            || (matches!(tree.ty, "match_pattern" | "match_pattern_p") && index == 1)
    };
    let children = tree
        .children
        .iter()
        .enumerate()
        .map(|(index, child)| match child {
            Child::Node(inner) => {
                let mut marked_child = mark_slots(inner, source, pattern_child(index));
                let expression = slots.contains(&index)
                    && !matches!(
                        inner.ty,
                        "splat"
                            | "block_pass"
                            | "kwargs"
                            | "forwarded_args"
                            | "forwarded_restarg"
                            | "kwsplat"
                    )
                    && !inner.has(syntax::WORD);
                if expression {
                    let copy = Rc::make_mut(&mut marked_child);
                    copy.syntax |= syntax::EXPRESSION;
                }
                Child::Node(marked_child)
            }
            other => other.clone(),
        })
        .collect();
    Rc::new(N {
        ty: tree.ty,
        children,
        span: tree.span,
        syntax: tree.syntax,
    })
}
