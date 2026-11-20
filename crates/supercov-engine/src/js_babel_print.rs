//! Prints a JavaScript or TypeScript node the way `@babel/generator` 8
//! prints it with its default options, which is how StrykerJS 10.1.1 writes a
//! mutant's replacement code (`generate(replacement).code`).
//!
//! Strong mutation writes StrykerJS's replacements into the program; this
//! printer is what shows, byte for byte, that a replacement Supercov builds is
//! the one StrykerJS builds. It covers what a mutant's replacement holds:
//! expressions, the statements a `for` loop or a `case` carries, and JSX.
//! Babel's rules for spaces, parentheses (`needsParens`), blocks and lists are
//! followed as its source states them. A node it does not print -- a class
//! body, a destructuring pattern, a type -- is written as the source wrote
//! it, which Babel would only re-space.
//!
//! Comments are not printed: Babel prints those a cloned node carries.

use oxc_ast::ast::{
    Argument, ArrayExpressionElement, ArrowFunctionExpression, AssignmentTarget, BindingPattern,
    ChainElement, Expression, ForStatementInit, ForStatementLeft, FormalParameters, Function,
    FunctionBody, JSXAttributeItem, JSXAttributeName, JSXAttributeValue, JSXChild, JSXElement,
    JSXElementName, JSXExpression, JSXFragment, JSXMemberExpression, JSXMemberExpressionObject,
    ObjectPropertyKind, PropertyKey, PropertyKind, SimpleAssignmentTarget, Statement, SwitchCase,
    TemplateLiteral, VariableDeclaration,
};
use oxc_span::{GetSpan, Span};
use oxc_syntax::operator::{BinaryOperator, LogicalOperator, UnaryOperator};

/// An expression as Babel prints it.
pub(crate) fn expression(source: &str, expression: &Expression<'_>) -> String {
    let mut printer = Printer::new(source);
    printer.expression(expression, Ctx::Top);
    printer.finish()
}

/// A statement as Babel prints it.
pub(crate) fn statement(source: &str, statement: &Statement<'_>) -> String {
    let mut printer = Printer::new(source);
    printer.statement(statement);
    printer.finish()
}

/// A `case` (or `default`) clause as Babel prints it.
pub(crate) fn switch_case(source: &str, case: &SwitchCase<'_>) -> String {
    let mut printer = Printer::new(source);
    printer.switch_case(case);
    printer.finish()
}

/// What the last thing written was, as Babel's buffer tracks it: a word or an
/// integer may need a space before the next word or `.`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Last {
    Nothing,
    Char(char),
    Word,
    Number,
    QueuedSpace,
}

/// Where an expression is printed: Babel's `needsParens` reads the parent
/// and the expression's place in it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ctx {
    Top,
    /// A statement's own expression: an `if`'s test, a `return`'s argument.
    Statement,
    Binary {
        precedence: u8,
        left: bool,
    },
    Logical {
        precedence: u8,
        left: bool,
    },
    /// `as` and `satisfies`, which bind as a comparison.
    TsAs,
    TsAssertion,
    Unary,
    Spread,
    Await,
    /// A call's callee, or an optional call's.
    Callee,
    NewCallee,
    MemberObject,
    Tag,
    NonNull,
    ConditionalTest,
    ClassExtends,
    Sequence,
    TemplateExpression,
    ComputedProperty,
    Other,
}

impl Ctx {
    /// Babel's `hasPostfixPart`: the expression is followed by something
    /// that binds tighter than any operator.
    fn postfix(self) -> bool {
        matches!(
            self,
            Ctx::Callee | Ctx::NewCallee | Ctx::MemberObject | Ctx::Tag | Ctx::NonNull
        )
    }

    /// Babel's `UnaryLike`.
    fn unary_like(self) -> bool {
        self.postfix()
            || matches!(
                self,
                Ctx::Binary {
                    precedence: 11,
                    left: true
                }
            )
            || self == Ctx::ClassExtends
    }

    /// Babel's rule for `?:`, arrows and assignments.
    fn conditional_like(self) -> bool {
        matches!(
            self,
            Ctx::Unary
                | Ctx::Spread
                | Ctx::Binary { .. }
                | Ctx::Logical { .. }
                | Ctx::Await
                | Ctx::ConditionalTest
                | Ctx::TsAs
                | Ctx::TsAssertion
        ) || self.unary_like()
    }
}

fn binary_precedence(operator: BinaryOperator) -> u8 {
    match operator {
        BinaryOperator::BitwiseOR => 3,
        BinaryOperator::BitwiseXOR => 4,
        BinaryOperator::BitwiseAnd => 5,
        BinaryOperator::Equality
        | BinaryOperator::Inequality
        | BinaryOperator::StrictEquality
        | BinaryOperator::StrictInequality => 6,
        BinaryOperator::LessThan
        | BinaryOperator::LessEqualThan
        | BinaryOperator::GreaterThan
        | BinaryOperator::GreaterEqualThan
        | BinaryOperator::In
        | BinaryOperator::Instanceof => 7,
        BinaryOperator::ShiftLeft
        | BinaryOperator::ShiftRight
        | BinaryOperator::ShiftRightZeroFill => 8,
        BinaryOperator::Addition | BinaryOperator::Subtraction => 9,
        BinaryOperator::Multiplication | BinaryOperator::Division | BinaryOperator::Remainder => 10,
        BinaryOperator::Exponential => 11,
    }
}

fn logical_precedence(operator: LogicalOperator) -> u8 {
    match operator {
        LogicalOperator::Or => 0,
        LogicalOperator::Coalesce => 1,
        LogicalOperator::And => 2,
    }
}

/// Babel's `BinaryLike` for an operator of `precedence` (a logical one when
/// `logical`) printed at `ctx`.
fn binary_like_needs_parens(precedence: u8, logical: bool, ctx: Ctx) -> bool {
    if ctx == Ctx::ClassExtends
        || ctx.postfix()
        || matches!(ctx, Ctx::Unary | Ctx::Spread | Ctx::Await)
    {
        return true;
    }
    let (parent, parent_binary, left) = match ctx {
        Ctx::Binary { precedence, left } => (precedence, true, left),
        Ctx::Logical { precedence, left } => (precedence, false, left),
        Ctx::TsAs => (7, false, false),
        _ => return false,
    };
    if parent > precedence {
        return true;
    }
    if parent == precedence && parent_binary && (if precedence == 11 { left } else { !left }) {
        return true;
    }
    let parent_logical = matches!(ctx, Ctx::Logical { .. });
    logical && parent_logical && ((precedence == 1) != (parent == 1))
}

struct Printer<'s> {
    source: &'s str,
    out: String,
    last: Last,
    indent: usize,
    /// Babel's token context: the expression starts a statement (1), an
    /// arrow's body (2) or a default export (4). Cleared by the first token.
    start: u8,
}

impl<'s> Printer<'s> {
    fn new(source: &'s str) -> Self {
        Self {
            source,
            out: String::new(),
            last: Last::Nothing,
            indent: 0,
            start: 0,
        }
    }

    fn finish(self) -> String {
        self.out.trim_end().to_owned()
    }

    fn last_char(&self) -> Option<char> {
        match self.last {
            Last::QueuedSpace => Some(' '),
            _ => self.out.chars().next_back(),
        }
    }

    /// Writes `text`: the queued space first, and the indentation where a
    /// line starts.
    fn append(&mut self, text: &str) {
        self.start = 0;
        if self.last == Last::QueuedSpace {
            self.out.push(' ');
        } else if self.out.ends_with('\n') {
            for _ in 0..self.indent {
                self.out.push_str("  ");
            }
        }
        self.out.push_str(text);
    }

    fn space(&mut self) {
        if !matches!(self.last_char(), None | Some(' ' | '\n')) {
            self.last = Last::QueuedSpace;
        }
    }

    fn newline(&mut self) {
        if self.last == Last::QueuedSpace {
            self.last = Last::Char('\n');
        }
        if self.out.is_empty() || self.out.ends_with('\n') {
            return;
        }
        self.out.push('\n');
        self.last = Last::Char('\n');
    }

    fn word(&mut self, word: &str) {
        if matches!(self.last, Last::Word | Last::Number)
            || (self.last == Last::Char('/') && word.starts_with('/'))
        {
            self.last = Last::QueuedSpace;
        }
        self.append(word);
        self.last = Last::Word;
    }

    fn number(&mut self, text: &str, integer: bool) {
        self.word(text);
        let decimal = !(text.len() > 2
            && text.starts_with('0')
            && matches!(text.as_bytes()[1], b'b' | b'B' | b'o' | b'O' | b'x' | b'X'));
        let scientific = text.contains(['e', 'E']);
        let zero_decimal = text.len() > 1 && text.starts_with('0') && !text.contains('.');
        if integer && decimal && !scientific && !zero_decimal && !text.ends_with('.') {
            self.last = Last::Number;
        }
    }

    /// Babel's `token`: `+ +`, `- -` and `1 .x` keep their space.
    fn token(&mut self, token: &str) {
        let first = token.chars().next().unwrap_or(' ');
        let needs_space = ((first == '-' && token == "--" || first == '=')
            && self.last == Last::Char('!'))
            || (first == '+' && self.last == Last::Char('+'))
            || (first == '-' && self.last == Last::Char('-'))
            || (first == '.' && self.last == Last::Number);
        if needs_space {
            self.last = Last::QueuedSpace;
        }
        self.append(token);
        self.last = Last::Char(token.chars().next_back().unwrap_or(' '));
    }

    fn char(&mut self, character: char) {
        if (character == '+' && self.last == Last::Char('+'))
            || (character == '-' && self.last == Last::Char('-'))
            || (character == '.' && self.last == Last::Number)
        {
            self.last = Last::QueuedSpace;
        }
        let mut buffer = [0; 4];
        self.append(character.encode_utf8(&mut buffer));
        self.last = Last::Char(character);
    }

    /// The source of a node this printer does not print.
    fn verbatim(&mut self, span: Span) {
        let text = self
            .source
            .get(span.start as usize..span.end as usize)
            .unwrap_or_default()
            .to_owned();
        let first = text.chars().next().unwrap_or(' ');
        if (first.is_alphanumeric() || first == '_' || first == '$')
            && matches!(self.last, Last::Word | Last::Number)
        {
            self.last = Last::QueuedSpace;
        }
        self.append(&text);
        self.last = match text.chars().next_back() {
            Some(character)
                if character.is_alphanumeric() || character == '_' || character == '$' =>
            {
                Last::Word
            }
            Some(character) => Last::Char(character),
            None => self.last,
        };
    }

    // ---- expressions --------------------------------------------------

    fn needs_parens(&self, expression: &Expression<'_>, ctx: Ctx) -> bool {
        match expression {
            Expression::BinaryExpression(binary) => {
                binary_like_needs_parens(binary_precedence(binary.operator), false, ctx)
            }
            Expression::PrivateInExpression(_) => binary_like_needs_parens(7, false, ctx),
            Expression::LogicalExpression(logical) => {
                binary_like_needs_parens(logical_precedence(logical.operator), true, ctx)
            }
            Expression::UnaryExpression(_) => ctx.unary_like(),
            Expression::UpdateExpression(_) => ctx.postfix() || ctx == Ctx::ClassExtends,
            Expression::ConditionalExpression(_) | Expression::ArrowFunctionExpression(_) => {
                ctx.conditional_like()
            }
            Expression::AssignmentExpression(assignment) => {
                (self.start & 1 != 0
                    && matches!(assignment.left, AssignmentTarget::ObjectAssignmentTarget(_)))
                    || ctx.conditional_like()
            }
            Expression::AwaitExpression(_) | Expression::YieldExpression(_) => {
                matches!(
                    ctx,
                    Ctx::Binary { .. }
                        | Ctx::Logical { .. }
                        | Ctx::Unary
                        | Ctx::Spread
                        | Ctx::ConditionalTest
                        | Ctx::ClassExtends
                        | Ctx::TsAs
                        | Ctx::TsAssertion
                ) || ctx.postfix()
                    || (ctx == Ctx::Await && matches!(expression, Expression::YieldExpression(_)))
            }
            Expression::SequenceExpression(_) => !matches!(
                ctx,
                Ctx::Top
                    | Ctx::Statement
                    | Ctx::Sequence
                    | Ctx::ComputedProperty
                    | Ctx::TemplateExpression
            ),
            Expression::ObjectExpression(_) => self.start & 3 != 0,
            Expression::FunctionExpression(_) | Expression::ClassExpression(_) => {
                self.start & 5 != 0
            }
            Expression::ChainExpression(_) => matches!(ctx, Ctx::Callee | Ctx::MemberObject),
            Expression::TSAsExpression(_) | Expression::TSSatisfiesExpression(_) => {
                binary_like_needs_parens(7, false, ctx)
            }
            Expression::TSTypeAssertion(_) => ctx.unary_like(),
            _ => false,
        }
    }

    /// Babel's `newCalleeNeedsParens` and `templateTagNeedsParens`.
    fn callee_needs_parens(expression: &Expression<'_>, new: bool) -> bool {
        match expression {
            Expression::ParenthesizedExpression(inner) => {
                Self::callee_needs_parens(&inner.expression, new)
            }
            Expression::ChainExpression(_) => true,
            Expression::CallExpression(_) | Expression::ImportExpression(_) => new,
            Expression::StaticMemberExpression(member) if new => {
                Self::callee_needs_parens(&member.object, new)
            }
            Expression::ComputedMemberExpression(member) if new => {
                Self::callee_needs_parens(&member.object, new)
            }
            Expression::PrivateFieldExpression(member) if new => {
                Self::callee_needs_parens(&member.object, new)
            }
            Expression::TaggedTemplateExpression(tagged) if new => {
                Self::callee_needs_parens(&tagged.tag, new)
            }
            Expression::TSNonNullExpression(inner) => {
                Self::callee_needs_parens(&inner.expression, new)
            }
            Expression::TSInstantiationExpression(inner) => {
                Self::callee_needs_parens(&inner.expression, new)
            }
            _ => false,
        }
    }

    fn expression(&mut self, expression: &Expression<'_>, ctx: Ctx) {
        if let Expression::ParenthesizedExpression(inner) = expression {
            self.expression(&inner.expression, ctx);
            return;
        }
        let parens = self.needs_parens(expression, ctx)
            || (ctx == Ctx::NewCallee && Self::callee_needs_parens(expression, true))
            || (ctx == Ctx::Tag && Self::callee_needs_parens(expression, false));
        if parens {
            self.char('(');
        }
        self.expression_body(expression);
        if parens {
            self.char(')');
        }
    }

    fn expression_body(&mut self, expression: &Expression<'_>) {
        match expression {
            Expression::BooleanLiteral(literal) => {
                self.word(if literal.value { "true" } else { "false" });
            }
            Expression::NullLiteral(_) => self.word("null"),
            Expression::NumericLiteral(literal) => {
                let text = literal
                    .raw
                    .map_or_else(|| js_number(literal.value), |raw| raw.as_str().to_owned());
                self.number(
                    &text,
                    literal.value.fract() == 0.0 && literal.value.is_finite(),
                );
            }
            Expression::BigIntLiteral(literal) => match literal.raw {
                Some(raw) => self.word(raw.as_str()),
                None => self.word(&format!("{}n", literal.value)),
            },
            Expression::RegExpLiteral(literal) => match literal.raw {
                Some(raw) => self.word(raw.as_str()),
                None => self.word(&format!(
                    "/{}/{}",
                    literal.regex.pattern.text, literal.regex.flags
                )),
            },
            Expression::StringLiteral(literal) => match literal.raw {
                Some(raw) => self.token(raw.as_str()),
                None => self.token(&double_quoted(literal.value.as_str())),
            },
            Expression::TemplateLiteral(template) => self.template(template),
            Expression::Identifier(identifier) => self.word(identifier.name.as_str()),
            Expression::MetaProperty(meta) => {
                self.word(meta.meta.name.as_str());
                self.char('.');
                self.word(meta.property.name.as_str());
            }
            Expression::Super(_) => self.word("super"),
            Expression::ThisExpression(_) => self.word("this"),
            Expression::ArrayExpression(array) => {
                self.char('[');
                let length = array.elements.len();
                for (index, element) in array.elements.iter().enumerate() {
                    match element {
                        ArrayExpressionElement::Elision(_) => self.char(','),
                        ArrayExpressionElement::SpreadElement(spread) => {
                            if index > 0 {
                                self.space();
                            }
                            self.token("...");
                            self.expression(&spread.argument, Ctx::Spread);
                            if index < length - 1 {
                                self.char(',');
                            }
                        }
                        element => {
                            if index > 0 {
                                self.space();
                            }
                            self.expression(element.to_expression(), Ctx::Other);
                            if index < length - 1 {
                                self.char(',');
                            }
                        }
                    }
                }
                self.char(']');
            }
            Expression::ObjectExpression(object) => {
                self.char('{');
                if !object.properties.is_empty() {
                    self.space();
                    self.indent += 1;
                    let count = object.properties.len();
                    for (index, property) in object.properties.iter().enumerate() {
                        self.newline();
                        match property {
                            ObjectPropertyKind::SpreadProperty(spread) => {
                                self.token("...");
                                self.expression(&spread.argument, Ctx::Spread);
                            }
                            ObjectPropertyKind::ObjectProperty(property) => {
                                if property.method || property.kind != PropertyKind::Init {
                                    self.object_method(property);
                                } else {
                                    self.property_key(&property.key, property.computed);
                                    let shorthand = property.shorthand
                                        && matches!(
                                            (&property.key, &property.value),
                                            (PropertyKey::StaticIdentifier(key), Expression::Identifier(value))
                                                if key.name == value.name
                                        );
                                    if !shorthand {
                                        self.char(':');
                                        self.space();
                                        self.expression(&property.value, Ctx::Other);
                                    }
                                }
                            }
                        }
                        if index < count - 1 {
                            self.char(',');
                            self.space();
                        }
                    }
                    self.newline();
                    self.indent -= 1;
                    self.space();
                }
                self.char('}');
            }
            Expression::ArrowFunctionExpression(arrow) => self.arrow(arrow),
            Expression::FunctionExpression(function) => self.function(function),
            Expression::AssignmentExpression(assignment) => {
                self.assignment_target(&assignment.left);
                self.space();
                self.token(assignment.operator.as_str());
                self.space();
                self.expression(&assignment.right, Ctx::Other);
            }
            Expression::AwaitExpression(await_expression) => {
                self.word("await");
                self.space();
                self.expression(&await_expression.argument, Ctx::Await);
            }
            Expression::YieldExpression(yield_expression) => {
                self.word("yield");
                if yield_expression.delegate {
                    self.char('*');
                }
                if let Some(argument) = &yield_expression.argument {
                    self.space();
                    self.expression(argument, Ctx::Other);
                }
            }
            Expression::BinaryExpression(binary) => {
                let precedence = binary_precedence(binary.operator);
                self.expression(
                    &binary.left,
                    Ctx::Binary {
                        precedence,
                        left: true,
                    },
                );
                self.space();
                let operator = binary.operator.as_str();
                if operator.starts_with('i') {
                    self.word(operator);
                } else {
                    self.token(operator);
                }
                self.space();
                self.expression(
                    &binary.right,
                    Ctx::Binary {
                        precedence,
                        left: false,
                    },
                );
            }
            Expression::PrivateInExpression(private_in) => {
                self.char('#');
                self.word(private_in.left.name.as_str());
                self.space();
                self.word("in");
                self.space();
                self.expression(
                    &private_in.right,
                    Ctx::Binary {
                        precedence: 7,
                        left: false,
                    },
                );
            }
            Expression::LogicalExpression(logical) => {
                let precedence = logical_precedence(logical.operator);
                self.expression(
                    &logical.left,
                    Ctx::Logical {
                        precedence,
                        left: true,
                    },
                );
                self.space();
                self.token(logical.operator.as_str());
                self.space();
                self.expression(
                    &logical.right,
                    Ctx::Logical {
                        precedence,
                        left: false,
                    },
                );
            }
            Expression::UnaryExpression(unary) => {
                let operator = unary.operator.as_str();
                if matches!(
                    unary.operator,
                    UnaryOperator::Typeof | UnaryOperator::Void | UnaryOperator::Delete
                ) {
                    self.word(operator);
                    self.space();
                } else {
                    self.char(operator.chars().next().unwrap_or('!'));
                }
                self.expression(&unary.argument, Ctx::Unary);
            }
            Expression::UpdateExpression(update) => {
                if update.prefix {
                    self.token(update.operator.as_str());
                    self.simple_target(&update.argument);
                } else {
                    self.simple_target(&update.argument);
                    self.token(update.operator.as_str());
                }
            }
            Expression::ConditionalExpression(conditional) => {
                self.expression(&conditional.test, Ctx::ConditionalTest);
                self.space();
                self.char('?');
                self.space();
                self.expression(&conditional.consequent, Ctx::Other);
                self.space();
                self.char(':');
                self.space();
                self.expression(&conditional.alternate, Ctx::Other);
            }
            Expression::SequenceExpression(sequence) => {
                for (index, item) in sequence.expressions.iter().enumerate() {
                    if index > 0 {
                        self.char(',');
                        self.space();
                    }
                    self.expression(item, Ctx::Sequence);
                }
            }
            Expression::CallExpression(call) => {
                self.expression(&call.callee, Ctx::Callee);
                if call.optional {
                    self.token("?.");
                }
                if let Some(arguments) = &call.type_arguments {
                    self.verbatim(arguments.span);
                }
                self.arguments(&call.arguments);
            }
            Expression::NewExpression(new) => {
                self.word("new");
                self.space();
                self.expression(&new.callee, Ctx::NewCallee);
                if let Some(arguments) = &new.type_arguments {
                    self.verbatim(arguments.span);
                }
                self.arguments(&new.arguments);
            }
            Expression::ImportExpression(import) => {
                self.word("import");
                self.char('(');
                self.expression(&import.source, Ctx::Other);
                if let Some(options) = &import.options {
                    self.char(',');
                    self.space();
                    self.expression(options, Ctx::Other);
                }
                self.char(')');
            }
            Expression::ChainExpression(chain) => match &chain.expression {
                ChainElement::CallExpression(call) => {
                    self.expression_body_call(call);
                }
                ChainElement::TSNonNullExpression(non_null) => {
                    self.expression(&non_null.expression, Ctx::NonNull);
                    self.char('!');
                }
                element => {
                    let member = element.to_member_expression();
                    self.member(member);
                }
            },
            Expression::StaticMemberExpression(_)
            | Expression::ComputedMemberExpression(_)
            | Expression::PrivateFieldExpression(_) => {
                self.member(expression.to_member_expression());
            }
            Expression::TaggedTemplateExpression(tagged) => {
                self.expression(&tagged.tag, Ctx::Tag);
                if let Some(arguments) = &tagged.type_arguments {
                    self.verbatim(arguments.span);
                }
                self.template(&tagged.quasi);
            }
            Expression::TSAsExpression(as_expression) => {
                self.expression(&as_expression.expression, Ctx::TsAs);
                self.space();
                self.word("as");
                self.space();
                self.verbatim(as_expression.type_annotation.span());
            }
            Expression::TSSatisfiesExpression(satisfies) => {
                self.expression(&satisfies.expression, Ctx::TsAs);
                self.space();
                self.word("satisfies");
                self.space();
                self.verbatim(satisfies.type_annotation.span());
            }
            Expression::TSNonNullExpression(non_null) => {
                self.expression(&non_null.expression, Ctx::NonNull);
                self.char('!');
            }
            Expression::TSTypeAssertion(assertion) => {
                self.char('<');
                self.verbatim(assertion.type_annotation.span());
                self.char('>');
                self.space();
                self.expression(&assertion.expression, Ctx::TsAssertion);
            }
            Expression::TSInstantiationExpression(instantiation) => {
                self.expression(&instantiation.expression, Ctx::MemberObject);
                self.verbatim(instantiation.type_arguments.span);
            }
            Expression::JSXElement(element) => self.jsx_element(element),
            Expression::JSXFragment(fragment) => self.jsx_fragment(fragment),
            other => self.verbatim(other.span()),
        }
    }

    fn expression_body_call(&mut self, call: &oxc_ast::ast::CallExpression<'_>) {
        self.expression(&call.callee, Ctx::Callee);
        if call.optional {
            self.token("?.");
        }
        if let Some(arguments) = &call.type_arguments {
            self.verbatim(arguments.span);
        }
        self.arguments(&call.arguments);
    }

    fn member(&mut self, member: &oxc_ast::ast::MemberExpression<'_>) {
        use oxc_ast::ast::MemberExpression;
        match member {
            MemberExpression::StaticMemberExpression(member) => {
                self.expression(&member.object, Ctx::MemberObject);
                if member.optional {
                    self.token("?.");
                } else {
                    self.char('.');
                }
                self.word(member.property.name.as_str());
            }
            MemberExpression::ComputedMemberExpression(member) => {
                self.expression(&member.object, Ctx::MemberObject);
                if member.optional {
                    self.token("?.");
                }
                self.char('[');
                self.expression(&member.expression, Ctx::ComputedProperty);
                self.char(']');
            }
            MemberExpression::PrivateFieldExpression(member) => {
                self.expression(&member.object, Ctx::MemberObject);
                if member.optional {
                    self.token("?.");
                } else {
                    self.char('.');
                }
                self.char('#');
                self.word(member.field.name.as_str());
            }
        }
    }

    fn arguments(&mut self, arguments: &[Argument<'_>]) {
        self.char('(');
        for (index, argument) in arguments.iter().enumerate() {
            if index > 0 {
                self.char(',');
                self.space();
            }
            match argument {
                Argument::SpreadElement(spread) => {
                    self.token("...");
                    self.expression(&spread.argument, Ctx::Spread);
                }
                argument => self.expression(argument.to_expression(), Ctx::Other),
            }
        }
        self.char(')');
    }

    fn template(&mut self, template: &TemplateLiteral<'_>) {
        let mut part = String::from("`");
        let last = template.quasis.len().saturating_sub(1);
        for (index, quasi) in template.quasis.iter().enumerate() {
            part.push_str(quasi.value.raw.as_str());
            if index < last {
                part.push_str("${");
                self.token(&part);
                if let Some(expression) = template.expressions.get(index) {
                    self.expression(expression, Ctx::TemplateExpression);
                }
                part = String::from("}");
            }
        }
        part.push('`');
        self.token(&part);
    }

    fn property_key(&mut self, key: &PropertyKey<'_>, computed: bool) {
        if computed {
            self.char('[');
            if let Some(expression) = key.as_expression() {
                self.expression(expression, Ctx::Other);
            }
            self.char(']');
            return;
        }
        match key {
            PropertyKey::StaticIdentifier(name) => self.word(name.name.as_str()),
            PropertyKey::PrivateIdentifier(name) => {
                self.char('#');
                self.word(name.name.as_str());
            }
            key => {
                if let Some(expression) = key.as_expression() {
                    self.expression(expression, Ctx::Other);
                }
            }
        }
    }

    fn object_method(&mut self, property: &oxc_ast::ast::ObjectProperty<'_>) {
        let Expression::FunctionExpression(function) = &property.value else {
            self.verbatim(property.span);
            return;
        };
        match property.kind {
            PropertyKind::Get => {
                self.word("get");
                self.space();
            }
            PropertyKind::Set => {
                self.word("set");
                self.space();
            }
            PropertyKind::Init => {}
        }
        if function.r#async {
            self.word("async");
            self.space();
        }
        if function.generator {
            self.char('*');
        }
        self.property_key(&property.key, property.computed);
        if let Some(parameters) = &function.type_parameters {
            self.verbatim(parameters.span);
        }
        self.parameters(&function.params);
        if let Some(return_type) = &function.return_type {
            self.char(':');
            self.space();
            self.verbatim(return_type.type_annotation.span());
        }
        self.space();
        match &function.body {
            Some(body) => self.function_body(body),
            None => self.char(';'),
        }
    }

    fn arrow(&mut self, arrow: &ArrowFunctionExpression<'_>) {
        if arrow.r#async {
            self.word("async");
            self.space();
        }
        let single = arrow.params.items.len() == 1
            && arrow.params.rest.is_none()
            && arrow.type_parameters.is_none()
            && arrow.return_type.is_none()
            && {
                let parameter = &arrow.params.items[0];
                matches!(parameter.pattern, BindingPattern::BindingIdentifier(_))
                    && parameter.type_annotation.is_none()
                    && !parameter.optional
                    && parameter.initializer.is_none()
            };
        if single {
            if let BindingPattern::BindingIdentifier(identifier) = &arrow.params.items[0].pattern {
                self.word(identifier.name.as_str());
            }
        } else {
            if let Some(parameters) = &arrow.type_parameters {
                self.verbatim(parameters.span);
            }
            self.parameters(&arrow.params);
            if let Some(return_type) = &arrow.return_type {
                self.char(':');
                self.space();
                self.verbatim(return_type.type_annotation.span());
            }
        }
        self.space();
        self.token("=>");
        self.space();
        if arrow.expression {
            if let Some(Statement::ExpressionStatement(statement)) = arrow.body.statements.first() {
                self.start |= 2;
                self.expression(&statement.expression, Ctx::Other);
            }
        } else {
            self.function_body(&arrow.body);
        }
    }

    fn function(&mut self, function: &Function<'_>) {
        if function.r#async {
            self.word("async");
            self.space();
        }
        self.word("function");
        if function.generator {
            self.char('*');
        }
        self.space();
        if let Some(id) = &function.id {
            self.word(id.name.as_str());
        }
        if let Some(parameters) = &function.type_parameters {
            self.verbatim(parameters.span);
        }
        self.parameters(&function.params);
        if let Some(return_type) = &function.return_type {
            self.char(':');
            self.space();
            self.verbatim(return_type.type_annotation.span());
        }
        self.space();
        match &function.body {
            Some(body) => self.function_body(body),
            None => self.char(';'),
        }
    }

    fn parameters(&mut self, parameters: &FormalParameters<'_>) {
        self.char('(');
        let mut first = true;
        for parameter in &parameters.items {
            if !first {
                self.char(',');
                self.space();
            }
            first = false;
            if !parameter.decorators.is_empty()
                || parameter.accessibility.is_some()
                || parameter.readonly
                || parameter.r#override
            {
                self.verbatim(parameter.span);
                continue;
            }
            self.binding(&parameter.pattern);
            if parameter.optional {
                self.char('?');
            }
            if let Some(annotation) = &parameter.type_annotation {
                self.char(':');
                self.space();
                self.verbatim(annotation.type_annotation.span());
            }
            if let Some(initializer) = &parameter.initializer {
                self.space();
                self.char('=');
                self.space();
                self.expression(initializer, Ctx::Other);
            }
        }
        if let Some(rest) = &parameters.rest {
            if !first {
                self.char(',');
                self.space();
            }
            self.token("...");
            self.binding(&rest.rest.argument);
            if let Some(annotation) = &rest.type_annotation {
                self.char(':');
                self.space();
                self.verbatim(annotation.type_annotation.span());
            }
        }
        self.char(')');
    }

    fn binding(&mut self, pattern: &BindingPattern<'_>) {
        match pattern {
            BindingPattern::BindingIdentifier(identifier) => self.word(identifier.name.as_str()),
            BindingPattern::AssignmentPattern(assignment) => {
                self.binding(&assignment.left);
                self.space();
                self.char('=');
                self.space();
                self.expression(&assignment.right, Ctx::Other);
            }
            other => self.verbatim(other.span()),
        }
    }

    fn assignment_target(&mut self, target: &AssignmentTarget<'_>) {
        match target.as_simple_assignment_target() {
            Some(simple) => self.simple_target(simple),
            None => self.verbatim(target.span()),
        }
    }

    fn simple_target(&mut self, target: &SimpleAssignmentTarget<'_>) {
        match target {
            SimpleAssignmentTarget::AssignmentTargetIdentifier(identifier) => {
                self.word(identifier.name.as_str());
            }
            SimpleAssignmentTarget::TSNonNullExpression(non_null) => {
                self.expression(&non_null.expression, Ctx::NonNull);
                self.char('!');
            }
            target => match target.as_member_expression() {
                Some(member) => self.member(member),
                None => self.verbatim(target.span()),
            },
        }
    }

    // ---- JSX ----------------------------------------------------------

    fn jsx_name(&mut self, name: &JSXElementName<'_>) {
        match name {
            JSXElementName::Identifier(identifier) => self.word(identifier.name.as_str()),
            JSXElementName::IdentifierReference(identifier) => {
                self.word(identifier.name.as_str());
            }
            JSXElementName::NamespacedName(name) => {
                self.word(name.namespace.name.as_str());
                self.char(':');
                self.word(name.name.name.as_str());
            }
            JSXElementName::MemberExpression(member) => self.jsx_member(member),
            JSXElementName::ThisExpression(_) => self.word("this"),
        }
    }

    fn jsx_member(&mut self, member: &JSXMemberExpression<'_>) {
        match &member.object {
            JSXMemberExpressionObject::IdentifierReference(identifier) => {
                self.word(identifier.name.as_str());
            }
            JSXMemberExpressionObject::MemberExpression(inner) => self.jsx_member(inner),
            JSXMemberExpressionObject::ThisExpression(_) => self.word("this"),
        }
        self.char('.');
        self.word(member.property.name.as_str());
    }

    fn jsx_element(&mut self, element: &JSXElement<'_>) {
        let opening = &element.opening_element;
        self.char('<');
        self.jsx_name(&opening.name);
        if let Some(arguments) = &opening.type_arguments {
            self.verbatim(arguments.span);
        }
        if !opening.attributes.is_empty() {
            self.space();
            for (index, attribute) in opening.attributes.iter().enumerate() {
                if index > 0 {
                    self.space();
                }
                match attribute {
                    JSXAttributeItem::Attribute(attribute) => {
                        match &attribute.name {
                            JSXAttributeName::Identifier(name) => self.word(name.name.as_str()),
                            JSXAttributeName::NamespacedName(name) => {
                                self.word(name.namespace.name.as_str());
                                self.char(':');
                                self.word(name.name.name.as_str());
                            }
                        }
                        if let Some(value) = &attribute.value {
                            self.char('=');
                            match value {
                                JSXAttributeValue::StringLiteral(literal) => match literal.raw {
                                    Some(raw) => self.token(raw.as_str()),
                                    None => self.token(&double_quoted(literal.value.as_str())),
                                },
                                JSXAttributeValue::ExpressionContainer(container) => {
                                    self.jsx_container(&container.expression);
                                }
                                JSXAttributeValue::Element(element) => self.jsx_element(element),
                                JSXAttributeValue::Fragment(fragment) => {
                                    self.jsx_fragment(fragment);
                                }
                            }
                        }
                    }
                    JSXAttributeItem::SpreadAttribute(spread) => {
                        self.char('{');
                        self.token("...");
                        self.expression(&spread.argument, Ctx::Other);
                        self.char('}');
                    }
                }
            }
        }
        let Some(closing) = &element.closing_element else {
            self.space();
            self.char('/');
            self.char('>');
            return;
        };
        self.char('>');
        self.indent += 1;
        self.jsx_children(&element.children);
        self.indent -= 1;
        self.char('<');
        self.char('/');
        self.jsx_name(&closing.name);
        self.char('>');
    }

    fn jsx_fragment(&mut self, fragment: &JSXFragment<'_>) {
        self.char('<');
        self.char('>');
        self.indent += 1;
        self.jsx_children(&fragment.children);
        self.indent -= 1;
        self.token("</");
        self.char('>');
    }

    fn jsx_children(&mut self, children: &[JSXChild<'_>]) {
        for child in children {
            match child {
                JSXChild::Text(text) => match text.raw {
                    Some(raw) => self.token(raw.as_str()),
                    None => self.token(text.value.as_str()),
                },
                JSXChild::Element(element) => self.jsx_element(element),
                JSXChild::Fragment(fragment) => self.jsx_fragment(fragment),
                JSXChild::ExpressionContainer(container) => {
                    self.jsx_container(&container.expression);
                }
                JSXChild::Spread(spread) => {
                    self.char('{');
                    self.token("...");
                    self.expression(&spread.expression, Ctx::Other);
                    self.char('}');
                }
            }
        }
    }

    fn jsx_container(&mut self, expression: &JSXExpression<'_>) {
        self.char('{');
        match expression {
            JSXExpression::EmptyExpression(empty) => {
                // What it holds is a comment, which Babel prints.
                let text = self
                    .source
                    .get(empty.span.start as usize..empty.span.end as usize)
                    .unwrap_or_default()
                    .trim()
                    .to_owned();
                if !text.is_empty() {
                    self.append(&text);
                    self.last = Last::Char('/');
                }
            }
            expression => {
                if let Some(expression) = expression.as_expression() {
                    self.expression(expression, Ctx::Other);
                }
            }
        }
        self.char('}');
    }

    // ---- statements ---------------------------------------------------

    fn semicolon(&mut self) {
        self.char(';');
    }

    /// A statement after a keyword: a space, unless it is empty.
    fn body_after(&mut self, body: &Statement<'_>) {
        if !matches!(body, Statement::EmptyStatement(_)) {
            self.space();
        }
        self.statement(body);
    }

    fn statements(&mut self, statements: &[Statement<'_>]) {
        for statement in statements {
            self.newline();
            self.statement(statement);
        }
        self.newline();
    }

    fn block(&mut self, statements: &[Statement<'_>]) {
        self.char('{');
        if !statements.is_empty() {
            self.indent += 1;
            self.statements(statements);
            self.indent -= 1;
        }
        self.char('}');
    }

    fn function_body(&mut self, body: &FunctionBody<'_>) {
        self.char('{');
        if !body.directives.is_empty() || !body.statements.is_empty() {
            self.indent += 1;
            for directive in &body.directives {
                self.newline();
                self.verbatim(directive.expression.span);
                self.semicolon();
            }
            if !body.directives.is_empty() && !body.statements.is_empty() {
                self.newline();
                self.out.push('\n');
            }
            self.statements(&body.statements);
            self.indent -= 1;
        }
        self.char('}');
    }

    fn variable_declaration(&mut self, declaration: &VariableDeclaration<'_>, in_for: bool) {
        if declaration.declare {
            self.word("declare");
            self.space();
        }
        self.word(declaration.kind.as_str());
        self.space();
        let initialized = !in_for
            && declaration
                .declarations
                .iter()
                .any(|declarator| declarator.init.is_some());
        let several = declaration.declarations.len() > 1;
        if several {
            self.indent += 1;
        }
        for (index, declarator) in declaration.declarations.iter().enumerate() {
            if index > 0 {
                self.char(',');
                if initialized {
                    self.newline();
                } else {
                    self.space();
                }
            }
            self.binding(&declarator.id);
            if declarator.definite {
                self.char('!');
            }
            if let Some(annotation) = &declarator.type_annotation {
                self.char(':');
                self.space();
                self.verbatim(annotation.type_annotation.span());
            }
            if let Some(init) = &declarator.init {
                self.space();
                self.char('=');
                self.space();
                self.expression(init, Ctx::Other);
            }
        }
        if several {
            self.indent -= 1;
        }
        if !in_for {
            self.semicolon();
        }
    }

    fn statement(&mut self, statement: &Statement<'_>) {
        match statement {
            Statement::BlockStatement(block) => self.block(&block.body),
            Statement::EmptyStatement(_) => self.semicolon(),
            Statement::ExpressionStatement(statement) => {
                self.start |= 1;
                self.expression(&statement.expression, Ctx::Statement);
                self.semicolon();
            }
            Statement::IfStatement(statement) => {
                self.word("if");
                self.space();
                self.char('(');
                self.expression(&statement.test, Ctx::Statement);
                self.char(')');
                self.space();
                let needs_block = statement.alternate.is_some()
                    && matches!(
                        last_statement(&statement.consequent),
                        Statement::IfStatement(_)
                    );
                if needs_block {
                    self.char('{');
                    self.newline();
                    self.indent += 1;
                }
                self.statement(&statement.consequent);
                if needs_block {
                    self.indent -= 1;
                    self.newline();
                    self.char('}');
                }
                if let Some(alternate) = &statement.alternate {
                    if self.out.ends_with('}') {
                        self.space();
                    }
                    self.word("else");
                    self.space();
                    self.statement(alternate);
                }
            }
            Statement::ForStatement(statement) => {
                self.word("for");
                self.space();
                self.char('(');
                match &statement.init {
                    Some(ForStatementInit::VariableDeclaration(declaration)) => {
                        self.variable_declaration(declaration, true);
                    }
                    Some(init) => {
                        if let Some(expression) = init.as_expression() {
                            self.expression(expression, Ctx::Statement);
                        }
                    }
                    None => {}
                }
                self.char(';');
                if let Some(test) = &statement.test {
                    self.space();
                    self.expression(test, Ctx::Statement);
                }
                self.char(';');
                if let Some(update) = &statement.update {
                    self.space();
                    self.expression(update, Ctx::Statement);
                }
                self.char(')');
                self.body_after(&statement.body);
            }
            Statement::ForInStatement(statement) => {
                self.word("for");
                self.space();
                self.char('(');
                self.for_left(&statement.left);
                self.space();
                self.word("in");
                self.space();
                self.expression(&statement.right, Ctx::Statement);
                self.char(')');
                self.body_after(&statement.body);
            }
            Statement::ForOfStatement(statement) => {
                self.word("for");
                self.space();
                if statement.r#await {
                    self.word("await");
                    self.space();
                }
                self.char('(');
                self.for_left(&statement.left);
                self.space();
                self.word("of");
                self.space();
                self.expression(&statement.right, Ctx::Statement);
                self.char(')');
                self.body_after(&statement.body);
            }
            Statement::WhileStatement(statement) => {
                self.word("while");
                self.space();
                self.char('(');
                self.expression(&statement.test, Ctx::Statement);
                self.char(')');
                self.body_after(&statement.body);
            }
            Statement::DoWhileStatement(statement) => {
                self.word("do");
                self.space();
                self.statement(&statement.body);
                self.space();
                self.word("while");
                self.space();
                self.char('(');
                self.expression(&statement.test, Ctx::Statement);
                self.char(')');
                self.semicolon();
            }
            Statement::BreakStatement(statement) => {
                self.word("break");
                if let Some(label) = &statement.label {
                    self.space();
                    self.word(label.name.as_str());
                }
                self.semicolon();
            }
            Statement::ContinueStatement(statement) => {
                self.word("continue");
                if let Some(label) = &statement.label {
                    self.space();
                    self.word(label.name.as_str());
                }
                self.semicolon();
            }
            Statement::ReturnStatement(statement) => {
                self.word("return");
                if let Some(argument) = &statement.argument {
                    self.space();
                    self.expression(argument, Ctx::Statement);
                }
                self.semicolon();
            }
            Statement::ThrowStatement(statement) => {
                self.word("throw");
                self.space();
                self.expression(&statement.argument, Ctx::Statement);
                self.semicolon();
            }
            Statement::LabeledStatement(statement) => {
                self.word(statement.label.name.as_str());
                self.char(':');
                self.space();
                self.statement(&statement.body);
            }
            Statement::VariableDeclaration(declaration) => {
                self.variable_declaration(declaration, false);
            }
            Statement::SwitchStatement(statement) => {
                self.word("switch");
                self.space();
                self.char('(');
                self.expression(&statement.discriminant, Ctx::Statement);
                self.char(')');
                self.space();
                self.char('{');
                if !statement.cases.is_empty() {
                    for case in &statement.cases {
                        self.newline();
                        self.switch_case(case);
                    }
                    self.newline();
                }
                self.char('}');
            }
            Statement::DebuggerStatement(_) => {
                self.word("debugger");
                self.semicolon();
            }
            other => self.verbatim(other.span()),
        }
    }

    fn for_left(&mut self, left: &ForStatementLeft<'_>) {
        match left {
            ForStatementLeft::VariableDeclaration(declaration) => {
                self.variable_declaration(declaration, true);
            }
            left => match left.as_assignment_target() {
                Some(target) => self.assignment_target(target),
                None => self.verbatim(left.span()),
            },
        }
    }

    fn switch_case(&mut self, case: &SwitchCase<'_>) {
        match &case.test {
            Some(test) => {
                self.word("case");
                self.space();
                self.expression(test, Ctx::Statement);
                self.char(':');
            }
            None => {
                self.word("default");
                self.char(':');
            }
        }
        if !case.consequent.is_empty() {
            self.indent += 1;
            self.statements(&case.consequent);
            self.indent -= 1;
        }
    }
}

/// Babel's `getLastStatement`: the innermost body of a loop or label.
fn last_statement<'b, 'a>(statement: &'b Statement<'a>) -> &'b Statement<'a> {
    match statement {
        Statement::ForStatement(inner) => last_statement(&inner.body),
        Statement::ForInStatement(inner) => last_statement(&inner.body),
        Statement::ForOfStatement(inner) => last_statement(&inner.body),
        Statement::WhileStatement(inner) => last_statement(&inner.body),
        Statement::LabeledStatement(inner) => last_statement(&inner.body),
        Statement::WithStatement(inner) => last_statement(&inner.body),
        statement => statement,
    }
}

/// A string as jsesc writes it with Babel's options: double quotes, and only
/// what must be escaped.
fn double_quoted(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    for character in value.chars() {
        match character {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            '\u{8}' => quoted.push_str("\\b"),
            '\u{c}' => quoted.push_str("\\f"),
            '\u{b}' => quoted.push_str("\\v"),
            '\0' => quoted.push_str("\\0"),
            '\u{2028}' => quoted.push_str("\\u2028"),
            '\u{2029}' => quoted.push_str("\\u2029"),
            character if (character as u32) < 0x20 || character as u32 == 0x7f => {
                quoted.push_str(&format!("\\x{:02X}", character as u32));
            }
            character => quoted.push(character),
        }
    }
    quoted.push('"');
    quoted
}

/// `String(value)` of a JavaScript number, for the integers and simple
/// fractions a fresh literal holds.
fn js_number(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_owned();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Infinity" } else { "-Infinity" }.to_owned();
    }
    if value.fract() == 0.0 && value.abs() < 1e21 {
        return format!("{value:.0}");
    }
    format!("{value}")
}

#[cfg(test)]
mod tests {
    use oxc_allocator::Allocator;
    use oxc_parser::Parser;
    use oxc_span::SourceType;

    use super::*;

    fn printed(source: &str) -> String {
        let allocator = Allocator::default();
        let wrapped = format!("({source})");
        let parsed = Parser::new(&allocator, &wrapped, SourceType::tsx())
            .parse_expression()
            .expect("an expression");
        expression(&wrapped, &parsed)
    }

    #[test]
    fn prints_as_babel_does() {
        for (source, babel) in [
            ("a+b", "a + b"),
            ("(a * b) / 2 % 7", "a * b / 2 % 7"),
            ("-(-1)", "- -1"),
            ("+(-1)", "+-1"),
            ("(a || b) && c", "(a || b) && c"),
            ("a ?? (b || c)", "a ?? (b || c)"),
            ("(a, b)", "a, b"),
            ("f((a, b))", "f((a, b))"),
            ("(item) => item.trim()", "item => item.trim()"),
            ("(x: number) => x", "(x: number) => x"),
            ("() => ({ a: 1 })", "() => ({\n  a: 1\n})"),
            ("(a?.b).c", "(a?.b).c"),
            ("new (a.b())()", "new (a.b())()"),
            ("new Array", "new Array()"),
            ("typeof  x", "typeof x"),
            ("[, ]", "[,]"),
            ("[1, , 2]", "[1,, 2]"),
            ("`a${ b }c`", "`a${b}c`"),
            ("x  instanceof  Y", "x instanceof Y"),
            ("a ? (b, c) : d", "a ? (b, c) : d"),
            ("(a ? b : c) ? d : e", "(a ? b : c) ? d : e"),
            ("list!.length", "list!.length"),
            ("1..toString()", "1..toString()"),
            ("(1).toString()", "1 .toString()"),
        ] {
            assert_eq!(printed(source), babel, "{source}");
        }
    }
}
