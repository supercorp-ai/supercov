//! The mutants StrykerJS 10.1.1 makes for one JavaScript or TypeScript file,
//! enumerated from the original program without running StrykerJS.
//!
//! StrykerJS's instrumenter is the specification: it walks the Babel AST in
//! visitor-key order and asks each mutator, in a fixed order, for its mutants
//! on every node it enters. This module walks the oxc AST the same way and
//! applies the same rules, so `enumerate` returns the same mutants, in the
//! same order, with the same spans. `scripts/stryker-parity.mjs` prints
//! StrykerJS's own list and the tests here compare the two on the fixtures in
//! `tests/fixtures/stryker-parity`.
//!
//! Regex mutants come from weapon-regex and are not enumerated. Whether a
//! regex has any still matters, because a statement call is mutated only when
//! it is the statement's sole mutant; that is approximated from the pattern.
//!
//! # Descriptors
//!
//! A mutant is identified by `file:startLine:startCol-endLine:endCol:Mutator:descriptor`.
//! The descriptor names the mutant within its span and mutator without
//! printing code:
//!
//! | Mutator | Descriptor |
//! | --- | --- |
//! | `ArithmeticOperator` | the new operator: `+` `-` `*` `/` |
//! | `EqualityOperator` | the new operator: `<` `<=` `>` `>=` `==` `!=` `===` `!==` |
//! | `LogicalOperator` | the new operator: `&&` `\|\|` |
//! | `AssignmentOperator` | the new operator: `+=` `-=` `*=` `/=` `<<=` `>>=` `&=` `\|=` `&&=` `\|\|=` |
//! | `UpdateOperator` | the new operator: `++` `--` |
//! | `UnaryOperator` | the new operator (`+` `-`), or `removed` for `~x` to `x` |
//! | `BooleanLiteral` | the new literal (`true` `false`), or `removed` for `!x` to `x` |
//! | `ConditionalExpression` | `true` or `false` (a test, or a `for(;;)` given a false test), or `empty` for a switch case emptied |
//! | `StringLiteral` | `empty`, or `Stryker was here!` for an empty string or template |
//! | `MethodExpression` | the new method name (`startsWith`), or `removed` for `x.trim()` to `x` |
//! | `OptionalChaining` | the link that loses its `?`: `.name` (`.#name`), `[]` for a computed member, `()` for a call |
//! | `ArrowFunction` | `undefined` |
//! | `BlockStatement`, `ObjectLiteral` | `{}` |
//! | `ArrayDeclaration` | the replacement: `[]` `["Stryker was here"]` `Array()` `Array([])` `new Array()` `new Array([])` |
//! | `CallExpression` | `empty` (the statement removed) |
//!
//! The same grammar is implemented in JavaScript in the parity script; keep
//! the two in step.

#![cfg_attr(not(test), allow(dead_code))]

use std::collections::HashSet;

use oxc_allocator::Allocator;
use oxc_ast::{
    AstKind,
    ast::{
        Argument, ArrowFunctionExpression, BinaryExpression, CallExpression, ChainElement,
        ChainExpression, ClassElement, Comment, ComputedMemberExpression, Decorator, Directive,
        Expression, Function, FunctionBody, FunctionType, Hashbang, ImportDeclaration,
        MethodDefinitionKind, PrivateFieldExpression, Program, PropertyKey, RegExpFlags, Statement,
        StaticMemberExpression, StringLiteral, TSAsExpression, TSEnumDeclaration,
        TSGlobalDeclaration, TSImportEqualsDeclaration, TSInterfaceDeclaration,
        TSModuleDeclaration, TSType, TSTypeAliasDeclaration, TSTypeAnnotation,
        TSTypeParameterDeclaration, TSTypeParameterInstantiation, VariableDeclaration,
    },
};
use oxc_ast_visit::{Visit, walk};
use oxc_parser::Parser;
use oxc_span::{GetSpan, SourceType, Span};
use oxc_syntax::{
    operator::{
        AssignmentOperator, BinaryOperator, LogicalOperator, UnaryOperator, UpdateOperator,
    },
    scope::ScopeFlags,
};

/// The StrykerJS version whose mutants these are, named in every report.
pub(crate) const MUTANT_SET: &str = "stryker-js@10.1.1";

/// One mutant StrykerJS would make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LedgerMutant {
    /// Byte offsets of the mutated node in the source.
    pub span: Span,
    /// 1-based line and 1-based UTF-16 column, as `mutation.json` reports them.
    pub start: (u32, u32),
    pub end: (u32, u32),
    /// StrykerJS's `mutatorName`.
    pub mutator: &'static str,
    /// The descriptor (see the module documentation).
    pub replacement: String,
}

const ARITHMETIC_OPERATOR: &str = "ArithmeticOperator";
const ARRAY_DECLARATION: &str = "ArrayDeclaration";
const ARROW_FUNCTION: &str = "ArrowFunction";
const ASSIGNMENT_OPERATOR: &str = "AssignmentOperator";
const BLOCK_STATEMENT: &str = "BlockStatement";
const BOOLEAN_LITERAL: &str = "BooleanLiteral";
const CALL_EXPRESSION: &str = "CallExpression";
const CONDITIONAL_EXPRESSION: &str = "ConditionalExpression";
const EQUALITY_OPERATOR: &str = "EqualityOperator";
const LOGICAL_OPERATOR: &str = "LogicalOperator";
const METHOD_EXPRESSION: &str = "MethodExpression";
const OBJECT_LITERAL: &str = "ObjectLiteral";
const OPTIONAL_CHAINING: &str = "OptionalChaining";
const REGEX: &str = "Regex";
const STRING_LITERAL: &str = "StringLiteral";
const UNARY_OPERATOR: &str = "UnaryOperator";
const UPDATE_OPERATOR: &str = "UpdateOperator";

const STRYKER_WAS_HERE: &str = "Stryker was here!";
const REMOVED: &str = "removed";
const EMPTY: &str = "empty";

/// StrykerJS's mutants for `program`, parsed from `source`, in StrykerJS's
/// order. Runs on the original, untransformed program.
pub(crate) fn enumerate(program: &Program<'_>, source: &str) -> Vec<LedgerMutant> {
    let mut enumerator = Enumerator::new(program, source);
    enumerator.visit_program(program);
    let lines = enumerator.lines;
    enumerator
        .collected
        .into_iter()
        .filter(|mutant| !mutant.ignored && !mutant.removed && mutant.mutator != REGEX)
        .map(|mutant| LedgerMutant {
            span: mutant.span,
            start: lines.position(mutant.span.start),
            end: lines.position(mutant.span.end),
            mutator: mutant.mutator,
            replacement: mutant.replacement,
        })
        .collect()
}

/// The descriptor of a StrykerJS mutant, from its `mutatorName`, the original
/// code its location covers, and StrykerJS's `replacement` code. `None` for
/// Regex mutants and anything the grammar does not describe.
pub(crate) fn descriptor_from_stryker(
    mutator: &str,
    original: &str,
    replacement: &str,
) -> Option<String> {
    let text = without_leading_comments(replacement);
    let descriptor = match mutator {
        ARITHMETIC_OPERATOR | EQUALITY_OPERATOR | LOGICAL_OPERATOR => {
            with_expression(text, |expression| match expression.without_parentheses() {
                Expression::BinaryExpression(binary) => Some(binary.operator.as_str().to_owned()),
                Expression::LogicalExpression(logical) => {
                    Some(logical.operator.as_str().to_owned())
                }
                _ => None,
            })?
        }
        ASSIGNMENT_OPERATOR => {
            with_expression(text, |expression| match expression.without_parentheses() {
                Expression::AssignmentExpression(assignment) => {
                    Some(assignment.operator.as_str().to_owned())
                }
                _ => None,
            })?
        }
        UPDATE_OPERATOR => {
            with_expression(text, |expression| match expression.without_parentheses() {
                Expression::UpdateExpression(update) => Some(update.operator.as_str().to_owned()),
                _ => None,
            })?
        }
        UNARY_OPERATOR => match original.chars().next()? {
            '+' => "-".to_owned(),
            '-' => "+".to_owned(),
            '~' => REMOVED.to_owned(),
            _ => return None,
        },
        BOOLEAN_LITERAL => {
            if original.starts_with('!') {
                REMOVED.to_owned()
            } else if text == "true" || text == "false" {
                text.to_owned()
            } else {
                return None;
            }
        }
        CONDITIONAL_EXPRESSION => {
            if text == "true" || text == "false" {
                text.to_owned()
            } else if text
                .strip_prefix("for")
                .is_some_and(|rest| rest.trim_start().starts_with('('))
            {
                "false".to_owned()
            } else if starts_with_word(text, "case")
                || text
                    .strip_prefix("default")
                    .is_some_and(|rest| rest.trim_start().starts_with(':'))
            {
                EMPTY.to_owned()
            } else {
                return None;
            }
        }
        STRING_LITERAL => {
            if matches!(text, "\"\"" | "''" | "``") {
                EMPTY.to_owned()
            } else if text.len() >= 2 && &text[1..text.len() - 1] == STRYKER_WAS_HERE {
                STRYKER_WAS_HERE.to_owned()
            } else {
                return None;
            }
        }
        METHOD_EXPRESSION => {
            let name = with_expression(original.trim(), |expression| match link(expression) {
                Link::Call(call) => method_name(&call.callee).map(str::to_owned),
                _ => None,
            })?;
            method_replacement(&name)?.unwrap_or(REMOVED).to_owned()
        }
        OPTIONAL_CHAINING => {
            with_expression(original.trim(), |expression| match link(expression) {
                Link::Call(call) if call.optional => Some("()".to_owned()),
                Link::Static(member) if member.optional => {
                    Some(format!(".{}", member.property.name))
                }
                Link::Private(member) if member.optional => {
                    Some(format!(".#{}", member.field.name))
                }
                Link::Computed(member) if member.optional => Some("[]".to_owned()),
                _ => None,
            })?
        }
        ARROW_FUNCTION => (compact(text) == "()=>undefined").then(|| "undefined".to_owned())?,
        BLOCK_STATEMENT | OBJECT_LITERAL => (compact(text) == "{}").then(|| "{}".to_owned())?,
        ARRAY_DECLARATION => {
            let spaced = text.split_whitespace().collect::<Vec<_>>().join(" ");
            let normalized = spaced.replace("[ ", "[").replace(" ]", "]");
            [
                "[]",
                "[\"Stryker was here\"]",
                "Array()",
                "Array([])",
                "new Array()",
                "new Array([])",
            ]
            .contains(&normalized.as_str())
            .then_some(normalized)?
        }
        CALL_EXPRESSION => matches!(text, ";" | "").then(|| EMPTY.to_owned())?,
        _ => return None,
    };
    Some(descriptor)
}

/// A MethodExpression mutant's code: the call with its method renamed to
/// `descriptor`, or only its receiver where the call is `removed`.
pub(crate) fn method_replaced(original: &str, descriptor: &str) -> Option<String> {
    let text = original.trim();
    with_expression(text, |expression| {
        let Link::Call(call) = link(expression) else {
            return None;
        };
        let (receiver, name) = match link(&call.callee) {
            Link::Static(member) => (member.object.span(), member.property.span),
            Link::Computed(member) => (member.object.span(), member.expression.span()),
            _ => return None,
        };
        let slice = |from: u32, to: u32| text.get(from as usize..to as usize);
        if descriptor == REMOVED {
            slice(receiver.start, receiver.end).map(str::to_owned)
        } else {
            Some(format!(
                "{}{descriptor}{}",
                slice(0, name.start)?,
                text.get(name.end as usize..)?
            ))
        }
    })
}

/// StrykerJS prints the comments a cloned node carries: a `for` or `case`
/// replacement can start with the comments above it.
fn without_leading_comments(code: &str) -> &str {
    let mut text = code.trim();
    loop {
        if text.starts_with("//") {
            text = text
                .find(['\n', '\r', '\u{2028}', '\u{2029}'])
                .map_or("", |end| text[end..].trim_start());
        } else if let Some(block) = text.strip_prefix("/*") {
            text = block
                .find("*/")
                .map_or("", |end| block[end + 2..].trim_start());
        } else {
            return text;
        }
    }
}

fn starts_with_word(text: &str, word: &str) -> bool {
    text.strip_prefix(word).is_some_and(|rest| {
        !rest
            .chars()
            .next()
            .is_some_and(|next| next.is_alphanumeric() || next == '_' || next == '$')
    })
}

fn compact(text: &str) -> String {
    text.chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

/// Parses `text` as one expression, as TypeScript, TSX or JavaScript with JSX
/// in turn, and reads it with `read`.
fn with_expression<T>(text: &str, read: impl Fn(&Expression<'_>) -> Option<T>) -> Option<T> {
    let source_types = [
        SourceType::ts(),
        SourceType::tsx(),
        SourceType::mjs().with_jsx(true),
    ];
    for source_type in source_types {
        let allocator = Allocator::default();
        if let Ok(expression) = Parser::new(&allocator, text, source_type).parse_expression()
            && let Some(value) = read(&expression)
        {
            return Some(value);
        }
    }
    None
}

/// What an expression is once parentheses and an optional chain around it
/// are removed: `a?.b` is a chain around a member.
enum Link<'b, 'a> {
    Call(&'b CallExpression<'a>),
    Static(&'b StaticMemberExpression<'a>),
    Computed(&'b ComputedMemberExpression<'a>),
    Private(&'b PrivateFieldExpression<'a>),
    Other,
}

fn link<'b, 'a>(expression: &'b Expression<'a>) -> Link<'b, 'a> {
    match expression.without_parentheses() {
        Expression::CallExpression(call) => Link::Call(call),
        Expression::StaticMemberExpression(member) => Link::Static(member),
        Expression::ComputedMemberExpression(member) => Link::Computed(member),
        Expression::PrivateFieldExpression(member) => Link::Private(member),
        Expression::ChainExpression(chain) => match &chain.expression {
            ChainElement::CallExpression(call) => Link::Call(call),
            ChainElement::StaticMemberExpression(member) => Link::Static(member),
            ChainElement::ComputedMemberExpression(member) => Link::Computed(member),
            ChainElement::PrivateFieldExpression(member) => Link::Private(member),
            ChainElement::TSNonNullExpression(_) => Link::Other,
        },
        _ => Link::Other,
    }
}

/// MethodExpression's table: a method's replacement name, or `None` when the
/// call is replaced by its receiver.
fn method_replacement(name: &str) -> Option<Option<&'static str>> {
    Some(match name {
        "charAt" | "filter" | "reverse" | "slice" | "sort" | "substr" | "substring" | "trim" => {
            None
        }
        "endsWith" => Some("startsWith"),
        "startsWith" => Some("endsWith"),
        "every" => Some("some"),
        "some" => Some("every"),
        "toLocaleLowerCase" => Some("toLocaleUpperCase"),
        "toLocaleUpperCase" => Some("toLocaleLowerCase"),
        "toLowerCase" => Some("toUpperCase"),
        "toUpperCase" => Some("toLowerCase"),
        "trimEnd" => Some("trimStart"),
        "trimStart" => Some("trimEnd"),
        "min" => Some("max"),
        "max" => Some("min"),
        "setDate" => Some("setTime"),
        // `setUTCDate` also becomes `setTime`, but `setTime` turns back only
        // into `setDate`.
        "setTime" => Some("setDate"),
        "setFullYear" => Some("setMonth"),
        "setMonth" => Some("setFullYear"),
        "setHours" => Some("setMinutes"),
        "setMinutes" => Some("setHours"),
        "setSeconds" => Some("setMilliseconds"),
        "setMilliseconds" => Some("setSeconds"),
        "setUTCDate" => Some("setTime"),
        "setUTCFullYear" => Some("setUTCMonth"),
        "setUTCMonth" => Some("setUTCFullYear"),
        "setUTCHours" => Some("setUTCMinutes"),
        "setUTCMinutes" => Some("setUTCHours"),
        "setUTCSeconds" => Some("setUTCMilliseconds"),
        "setUTCMilliseconds" => Some("setUTCSeconds"),
        _ => return None,
    })
}

/// The method a call's callee names: `a.name(...)` or `a[name](...)`, through
/// parentheses and an optional chain.
fn method_name<'a>(callee: &Expression<'a>) -> Option<&'a str> {
    match link(callee) {
        Link::Static(member) => Some(member.property.name.as_str()),
        Link::Computed(member) => match member.expression.without_parentheses() {
            Expression::Identifier(identifier) => Some(identifier.name.as_str()),
            _ => None,
        },
        _ => None,
    }
}

fn is_identifier(expression: &Expression<'_>, name: &str) -> bool {
    matches!(expression.without_parentheses(), Expression::Identifier(identifier) if identifier.name == name)
}

/// A string literal or an untagged template, the operands StrykerJS takes for
/// string concatenation.
fn is_string_like(expression: &Expression<'_>) -> bool {
    matches!(
        expression.without_parentheses(),
        Expression::StringLiteral(_) | Expression::TemplateLiteral(_)
    )
}

/// ArithmeticOperator leaves `+` alone when it may join strings: when the
/// right operand, or the left one (the right operand of a left binary), is a
/// string literal or template.
fn joins_strings(binary: &BinaryExpression<'_>) -> bool {
    let left = match binary.left.without_parentheses() {
        Expression::BinaryExpression(left) => &left.right,
        left => left,
    };
    is_string_like(&binary.right) || is_string_like(left)
}

fn arithmetic_replacement(operator: BinaryOperator) -> Option<&'static str> {
    Some(match operator {
        BinaryOperator::Addition => "-",
        BinaryOperator::Subtraction => "+",
        BinaryOperator::Multiplication => "/",
        BinaryOperator::Division | BinaryOperator::Remainder => "*",
        _ => return None,
    })
}

fn equality_replacements(operator: BinaryOperator) -> &'static [&'static str] {
    match operator {
        BinaryOperator::LessThan => &["<=", ">="],
        BinaryOperator::LessEqualThan => &["<", ">"],
        BinaryOperator::GreaterThan => &[">=", "<="],
        BinaryOperator::GreaterEqualThan => &[">", "<"],
        BinaryOperator::Equality => &["!="],
        BinaryOperator::Inequality => &["=="],
        BinaryOperator::StrictEquality => &["!=="],
        BinaryOperator::StrictInequality => &["==="],
        _ => &[],
    }
}

/// The binary operators ConditionalExpression replaces with `true` and `false`.
fn is_boolean_operator(operator: BinaryOperator) -> bool {
    !equality_replacements(operator).is_empty()
}

fn assignment_replacement(operator: AssignmentOperator) -> Option<&'static str> {
    Some(match operator {
        AssignmentOperator::Addition => "-=",
        AssignmentOperator::Subtraction => "+=",
        AssignmentOperator::Multiplication => "/=",
        AssignmentOperator::Division | AssignmentOperator::Remainder => "*=",
        AssignmentOperator::ShiftLeft => ">>=",
        AssignmentOperator::ShiftRight => "<<=",
        AssignmentOperator::BitwiseAnd => "|=",
        AssignmentOperator::BitwiseOR => "&=",
        AssignmentOperator::LogicalAnd => "||=",
        AssignmentOperator::LogicalOr | AssignmentOperator::LogicalNullish => "&&=",
        _ => return None,
    })
}

/// Whether weapon-regex makes any level-1 mutant of `pattern`: an anchor to
/// remove, a character class or predefined class to negate, a quantifier to
/// remove, or a lookaround to negate. An approximation of weapon-regex's
/// parser; only "any or none" matters here.
fn regex_has_mutants(pattern: &str, unicode: bool) -> bool {
    let characters: Vec<char> = pattern.chars().collect();
    let mut index = 0;
    let mut previous_open_group = false;
    while index < characters.len() {
        let character = characters[index];
        let after_open_group = previous_open_group;
        previous_open_group = false;
        match character {
            '\\' => {
                match characters.get(index + 1) {
                    Some('d' | 'D' | 's' | 'S' | 'w' | 'W') => return true,
                    Some('p' | 'P') if unicode => return true,
                    _ => {}
                }
                index += 2;
                continue;
            }
            '[' | '^' | '$' | '*' | '+' => return true,
            '?' => {
                if !after_open_group {
                    return true;
                }
                match (characters.get(index + 1), characters.get(index + 2)) {
                    (Some('=' | '!'), _) | (Some('<'), Some('=' | '!')) => return true,
                    _ => {}
                }
            }
            '{' => {
                let rest: String = characters[index + 1..].iter().collect();
                if let Some(close) = rest.find('}') {
                    let inside = &rest[..close];
                    let (low, high) = inside.split_once(',').unwrap_or((inside, ""));
                    if !low.is_empty()
                        && low.chars().all(|digit| digit.is_ascii_digit())
                        && high.chars().all(|digit| digit.is_ascii_digit())
                    {
                        return true;
                    }
                }
            }
            '(' => previous_open_group = true,
            _ => {}
        }
        index += 1;
    }
    false
}

/// Line starts as Babel counts lines: `\n`, `\r\n`, `\r`, U+2028 and U+2029
/// each end one.
struct LineIndex<'s> {
    source: &'s str,
    starts: Vec<u32>,
}

impl<'s> LineIndex<'s> {
    fn new(source: &'s str) -> Self {
        let mut starts = vec![0];
        let mut characters = source.char_indices().peekable();
        while let Some((offset, character)) = characters.next() {
            let next = offset + character.len_utf8();
            match character {
                '\r' => {
                    if characters.peek().is_some_and(|(_, next)| *next == '\n') {
                        characters.next();
                        starts.push((next + 1) as u32);
                    } else {
                        starts.push(next as u32);
                    }
                }
                '\n' | '\u{2028}' | '\u{2029}' => starts.push(next as u32),
                _ => {}
            }
        }
        Self { source, starts }
    }

    fn line_index(&self, offset: u32) -> usize {
        self.starts.partition_point(|start| *start <= offset) - 1
    }

    /// The 1-based line of a byte offset.
    fn line(&self, offset: u32) -> u32 {
        self.line_index(offset) as u32 + 1
    }

    /// The 1-based line and 1-based UTF-16 column of a byte offset.
    fn position(&self, offset: u32) -> (u32, u32) {
        let line = self.line_index(offset);
        let start = self.starts[line] as usize;
        let column = self.source[start..offset as usize].encode_utf16().count();
        (line as u32 + 1, column as u32 + 1)
    }
}

/// A `// Stryker disable` or `// Stryker restore` comment.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DirectiveComment {
    disable: bool,
    next_line: bool,
    /// Lowercase mutator names; `all` matches every mutator.
    mutators: Vec<String>,
}

/// Comments with only whitespace between them, as Babel groups them, and the
/// offset of the token after them. Babel attaches the group as leading
/// comments to the outermost node starting at that token, and StrykerJS reads
/// the directives when it enters that node.
struct CommentRun {
    target: u32,
    directives: Vec<DirectiveComment>,
}

#[derive(Debug)]
struct Rule {
    disable: bool,
    line: Option<u32>,
    mutators: Vec<String>,
}

fn is_js_whitespace(character: char) -> bool {
    character == '\u{feff}' || (character.is_whitespace() && character != '\u{85}')
}

fn comment_value<'s>(source: &'s str, comment: &Comment) -> &'s str {
    let text = &source[comment.span.start as usize..comment.span.end as usize];
    if let Some(line) = text.strip_prefix("//") {
        line
    } else {
        text.strip_prefix("/*")
            .and_then(|block| block.strip_suffix("*/"))
            .unwrap_or("")
    }
}

/// StrykerJS's `/^\s?Stryker (disable|restore)(?: (next-line))? ([a-zA-Z, ]+)(?::(.+)?)?/`
/// on a comment's text.
fn parse_directive(value: &str) -> Option<DirectiveComment> {
    let mut rest = value;
    if let Some(first) = rest.chars().next()
        && is_js_whitespace(first)
    {
        rest = &rest[first.len_utf8()..];
    }
    let rest = rest.strip_prefix("Stryker ")?;
    let (disable, rest) = if let Some(rest) = rest.strip_prefix("disable") {
        (true, rest)
    } else {
        (false, rest.strip_prefix("restore")?)
    };
    let list = |text: &str| -> Option<String> {
        let text = text.strip_prefix(' ')?;
        let length = text
            .find(|character: char| {
                !(character.is_ascii_alphabetic() || character == ',' || character == ' ')
            })
            .unwrap_or(text.len());
        (length > 0).then(|| text[..length].to_owned())
    };
    let (next_line, mutators) = match rest.strip_prefix(" next-line").and_then(list) {
        Some(mutators) => (true, mutators),
        None => (false, list(rest)?),
    };
    Some(DirectiveComment {
        disable,
        next_line,
        mutators: mutators
            .split(',')
            .map(|mutator| mutator.trim().to_ascii_lowercase())
            .collect(),
    })
}

fn comment_runs(program: &Program<'_>, source: &str) -> Vec<CommentRun> {
    let mut comments: Vec<&Comment> = program.comments.iter().collect();
    comments.sort_by_key(|comment| comment.span.start);
    let mut runs = Vec::new();
    let mut index = 0;
    while index < comments.len() {
        let mut directives = Vec::new();
        let mut end;
        loop {
            let comment = comments[index];
            if let Some(directive) = parse_directive(comment_value(source, comment)) {
                directives.push(directive);
            }
            end = comment.span.end;
            index += 1;
            let adjacent = comments.get(index).is_some_and(|next| {
                source[end as usize..next.span.start as usize]
                    .chars()
                    .all(is_js_whitespace)
            });
            if !adjacent {
                break;
            }
        }
        if directives.is_empty() {
            continue;
        }
        let rest = &source[end as usize..];
        let skipped = rest
            .find(|character: char| !is_js_whitespace(character))
            .unwrap_or(rest.len());
        runs.push(CommentRun {
            target: end + skipped as u32,
            directives,
        });
    }
    runs
}

#[derive(Debug)]
struct Collected {
    span: Span,
    mutator: &'static str,
    replacement: String,
    ignored: bool,
    /// Taken out again by the CallExpression mutator's filter.
    removed: bool,
}

/// An expression or throw statement entered: the mutants before it, and the
/// nodes StrykerJS had skipped before it.
struct StatementFrame {
    first: usize,
    skipped: usize,
}

struct Enumerator<'s, 'a> {
    lines: LineIndex<'s>,
    stack: Vec<AstKind<'a>>,
    collected: Vec<Collected>,
    runs: Vec<CommentRun>,
    next_run: usize,
    rules: Vec<Rule>,
    /// Nodes StrykerJS enters and skips (types, imports, decorators and
    /// `declare`d code). StrykerJS's mutant filter counts a subtree's
    /// mutants from a stack it pushes on every enter and pops on every
    /// exit, and a skipped node is entered without being exited.
    skipped: usize,
    /// Inside a type StrykerJS walks (a `satisfies` or `<T>` assertion type).
    in_type: usize,
    statements: Vec<StatementFrame>,
    /// Calls that are links of an optional chain (Babel's
    /// `OptionalCallExpression`), by address.
    chain_calls: HashSet<usize>,
}

impl<'s, 'a> Enumerator<'s, 'a> {
    fn new(program: &Program<'_>, source: &'s str) -> Self {
        Self {
            lines: LineIndex::new(source),
            stack: Vec::new(),
            collected: Vec::new(),
            runs: comment_runs(program, source),
            next_run: 0,
            rules: Vec::new(),
            skipped: 0,
            in_type: 0,
            statements: Vec::new(),
            chain_calls: HashSet::new(),
        }
    }

    /// Reads the directives of comments attached to a node StrykerJS enters
    /// at `start`. Comments whose token starts no entered node are dropped.
    fn advance(&mut self, start: u32) {
        while let Some(run) = self.runs.get(self.next_run) {
            if run.target > start {
                break;
            }
            if run.target == start {
                let line = self.lines.line(start);
                for directive in &run.directives {
                    self.rules.push(Rule {
                        disable: directive.disable,
                        line: directive.next_line.then_some(line),
                        mutators: directive.mutators.clone(),
                    });
                }
            }
            self.next_run += 1;
        }
    }

    fn ignored(&self, mutator: &str, line: u32) -> bool {
        let mutator = mutator.to_ascii_lowercase();
        for rule in self.rules.iter().rev() {
            let line_matches = rule.line.is_none_or(|rule_line| rule_line == line);
            let mutator_matches = rule
                .mutators
                .iter()
                .any(|name| *name == mutator || name == "all");
            if line_matches && mutator_matches {
                return rule.disable;
            }
        }
        false
    }

    fn skip(&mut self, span: Span) {
        self.advance(span.start);
        self.skipped += 1;
    }

    fn emit(&mut self, span: Span, mutator: &'static str, replacement: impl Into<String>) {
        let ignored = self.ignored(mutator, self.lines.line(span.start));
        self.collected.push(Collected {
            span,
            mutator,
            replacement: replacement.into(),
            ignored,
            removed: false,
        });
    }

    /// The node's parent in Babel's AST, which has no parentheses and no
    /// chain wrapper.
    fn babel_parent(&self) -> Option<AstKind<'a>> {
        self.stack.iter().rev().skip(1).copied().find(|kind| {
            !matches!(
                kind,
                AstKind::ParenthesizedExpression(_) | AstKind::ChainExpression(_)
            )
        })
    }

    fn in_chain(&self, call: &CallExpression<'_>) -> bool {
        self.chain_calls
            .contains(&(std::ptr::from_ref(call) as usize))
    }

    fn mark_chain_calls(&mut self, chain: &ChainExpression<'_>) {
        let mut next = match &chain.expression {
            ChainElement::CallExpression(call) => {
                self.chain_calls
                    .insert(std::ptr::from_ref::<CallExpression>(call) as usize);
                Some(&call.callee)
            }
            ChainElement::TSNonNullExpression(non_null) => Some(&non_null.expression),
            ChainElement::StaticMemberExpression(member) => Some(&member.object),
            ChainElement::ComputedMemberExpression(member) => Some(&member.object),
            ChainElement::PrivateFieldExpression(member) => Some(&member.object),
        };
        while let Some(expression) = next {
            next = match expression {
                Expression::CallExpression(call) => {
                    self.chain_calls
                        .insert(std::ptr::from_ref::<CallExpression>(call) as usize);
                    Some(&call.callee)
                }
                Expression::TSNonNullExpression(non_null) => Some(&non_null.expression),
                expression => expression
                    .as_member_expression()
                    .map(|member| member.object()),
            };
        }
    }

    fn mutate(&mut self, kind: AstKind<'a>) {
        let span = kind.span();

        // ArithmeticOperator
        if let AstKind::BinaryExpression(binary) = kind
            && let Some(replacement) = arithmetic_replacement(binary.operator)
            && !joins_strings(binary)
        {
            self.emit(span, ARITHMETIC_OPERATOR, replacement);
        }

        // ArrayDeclaration
        match kind {
            AstKind::ArrayExpression(array) => {
                let replacement = if array.elements.is_empty() {
                    "[\"Stryker was here\"]"
                } else {
                    "[]"
                };
                self.emit(span, ARRAY_DECLARATION, replacement);
            }
            AstKind::CallExpression(call)
                if !self.in_chain(call) && is_identifier(&call.callee, "Array") =>
            {
                let replacement = if call.arguments.is_empty() {
                    "Array([])"
                } else {
                    "Array()"
                };
                self.emit(span, ARRAY_DECLARATION, replacement);
            }
            AstKind::NewExpression(new) if is_identifier(&new.callee, "Array") => {
                let replacement = if new.arguments.is_empty() {
                    "new Array([])"
                } else {
                    "new Array()"
                };
                self.emit(span, ARRAY_DECLARATION, replacement);
            }
            _ => {}
        }

        // ArrowFunction
        if let AstKind::ArrowFunctionExpression(arrow) = kind
            && arrow.expression
            && !arrow_body(arrow).is_some_and(|body| is_identifier(body, "undefined"))
        {
            self.emit(span, ARROW_FUNCTION, "undefined");
        }

        // BlockStatement
        match kind {
            AstKind::BlockStatement(block) if !block.body.is_empty() => {
                self.emit(span, BLOCK_STATEMENT, "{}");
            }
            AstKind::FunctionBody(body)
                if !body.statements.is_empty() && !self.is_constructor_body_kept(body) =>
            {
                self.emit(span, BLOCK_STATEMENT, "{}");
            }
            _ => {}
        }

        // BooleanLiteral
        match kind {
            AstKind::BooleanLiteral(literal) => {
                let replacement = if literal.value { "false" } else { "true" };
                self.emit(span, BOOLEAN_LITERAL, replacement);
            }
            AstKind::UnaryExpression(unary) if unary.operator == UnaryOperator::LogicalNot => {
                self.emit(span, BOOLEAN_LITERAL, REMOVED);
            }
            _ => {}
        }

        // ConditionalExpression
        self.conditional(kind, span);

        // CallExpression: a statement that is only a call, or throws a new
        // object, removed. Filtered on exit.
        match kind {
            AstKind::ExpressionStatement(statement)
                if matches!(
                    statement.expression.without_parentheses(),
                    Expression::CallExpression(call) if !matches!(call.callee, Expression::Super(_))
                ) =>
            {
                self.emit(span, CALL_EXPRESSION, EMPTY);
            }
            AstKind::ThrowStatement(statement)
                if matches!(
                    statement.argument.without_parentheses(),
                    Expression::NewExpression(_)
                ) =>
            {
                self.emit(span, CALL_EXPRESSION, EMPTY);
            }
            _ => {}
        }

        // EqualityOperator
        if let AstKind::BinaryExpression(binary) = kind {
            for replacement in equality_replacements(binary.operator) {
                self.emit(span, EQUALITY_OPERATOR, *replacement);
            }
        }

        // LogicalOperator
        if let AstKind::LogicalExpression(logical) = kind {
            let replacement = match logical.operator {
                LogicalOperator::Or | LogicalOperator::Coalesce => "&&",
                LogicalOperator::And => "||",
            };
            self.emit(span, LOGICAL_OPERATOR, replacement);
        }

        // MethodExpression
        if let AstKind::CallExpression(call) = kind
            && let Some(name) = method_name(&call.callee)
            && let Some(replacement) = method_replacement(name)
        {
            self.emit(span, METHOD_EXPRESSION, replacement.unwrap_or(REMOVED));
        }

        // ObjectLiteral
        if let AstKind::ObjectExpression(object) = kind
            && !object.properties.is_empty()
        {
            self.emit(span, OBJECT_LITERAL, "{}");
        }

        // StringLiteral
        match kind {
            AstKind::TemplateLiteral(template) => {
                let blank = template.quasis.len() == 1 && template.quasis[0].value.raw.is_empty();
                self.emit(
                    span,
                    STRING_LITERAL,
                    if blank { STRYKER_WAS_HERE } else { EMPTY },
                );
            }
            AstKind::StringLiteral(literal) if self.string_literal_mutable(literal) => {
                let replacement = if literal.value.is_empty() {
                    STRYKER_WAS_HERE
                } else {
                    EMPTY
                };
                self.emit(span, STRING_LITERAL, replacement);
            }
            _ => {}
        }

        // UnaryOperator
        if let AstKind::UnaryExpression(unary) = kind {
            let replacement = match unary.operator {
                UnaryOperator::UnaryPlus => Some("-"),
                UnaryOperator::UnaryNegation => Some("+"),
                UnaryOperator::BitwiseNot => Some(REMOVED),
                _ => None,
            };
            if let Some(replacement) = replacement {
                self.emit(span, UNARY_OPERATOR, replacement);
            }
        }

        // UpdateOperator
        if let AstKind::UpdateExpression(update) = kind {
            let replacement = match update.operator {
                UpdateOperator::Increment => "--",
                UpdateOperator::Decrement => "++",
            };
            self.emit(span, UPDATE_OPERATOR, replacement);
        }

        // Regex: not enumerated, but a regex with mutants keeps a statement
        // call from being its statement's sole mutant.
        match kind {
            AstKind::RegExpLiteral(literal) => {
                let flags = literal.regex.flags;
                let unicode = flags.contains(RegExpFlags::U) || flags.contains(RegExpFlags::V);
                if regex_has_mutants(literal.regex.pattern.text.as_str(), unicode) {
                    self.emit(span, REGEX, "");
                }
            }
            AstKind::StringLiteral(literal) => {
                if let Some(flags) = self.regexp_constructor_flags(literal) {
                    let unicode = flags.contains('u') || flags.contains('v');
                    if regex_has_mutants(literal.value.as_str(), unicode) {
                        self.emit(span, REGEX, "");
                    }
                }
            }
            _ => {}
        }

        // OptionalChaining
        match kind {
            AstKind::StaticMemberExpression(member) if member.optional => {
                self.emit(
                    span,
                    OPTIONAL_CHAINING,
                    format!(".{}", member.property.name),
                );
            }
            AstKind::PrivateFieldExpression(member) if member.optional => {
                self.emit(span, OPTIONAL_CHAINING, format!(".#{}", member.field.name));
            }
            AstKind::ComputedMemberExpression(member) if member.optional => {
                self.emit(span, OPTIONAL_CHAINING, "[]");
            }
            AstKind::CallExpression(call) if call.optional => {
                self.emit(span, OPTIONAL_CHAINING, "()");
            }
            _ => {}
        }

        // AssignmentOperator
        if let AstKind::AssignmentExpression(assignment) = kind
            && let Some(replacement) = assignment_replacement(assignment.operator)
        {
            let logical = matches!(
                assignment.operator,
                AssignmentOperator::LogicalAnd
                    | AssignmentOperator::LogicalOr
                    | AssignmentOperator::LogicalNullish
            );
            if logical || !is_string_like(&assignment.right) {
                self.emit(span, ASSIGNMENT_OPERATOR, replacement);
            }
        }
    }

    fn conditional(&mut self, kind: AstKind<'a>, span: Span) {
        let parent = self.babel_parent();
        let is_test = |test: Option<&Expression<'_>>| {
            test.is_some_and(|test| test.without_parentheses().span() == span)
        };
        if !matches!(
            kind,
            AstKind::ParenthesizedExpression(_) | AstKind::ChainExpression(_)
        ) {
            let loop_test = match parent {
                Some(AstKind::ForStatement(statement)) => is_test(statement.test.as_ref()),
                Some(AstKind::WhileStatement(statement)) => is_test(Some(&statement.test)),
                Some(AstKind::DoWhileStatement(statement)) => is_test(Some(&statement.test)),
                _ => false,
            };
            if loop_test {
                self.emit(span, CONDITIONAL_EXPRESSION, "false");
                return;
            }
            if let Some(AstKind::IfStatement(statement)) = parent
                && is_test(Some(&statement.test))
            {
                self.emit(span, CONDITIONAL_EXPRESSION, "true");
                self.emit(span, CONDITIONAL_EXPRESSION, "false");
                return;
            }
        }
        let boolean = match kind {
            AstKind::BinaryExpression(binary) => is_boolean_operator(binary.operator),
            AstKind::LogicalExpression(logical) => logical.operator != LogicalOperator::Coalesce,
            _ => false,
        };
        if boolean {
            match parent {
                // `true || y` is the `true` of the whole `||`, and `false && y`
                // the `false` of the whole `&&`.
                Some(AstKind::LogicalExpression(parent))
                    if parent.operator == LogicalOperator::Or =>
                {
                    self.emit(span, CONDITIONAL_EXPRESSION, "false");
                }
                Some(AstKind::LogicalExpression(parent))
                    if parent.operator == LogicalOperator::And =>
                {
                    self.emit(span, CONDITIONAL_EXPRESSION, "true");
                }
                _ => {
                    self.emit(span, CONDITIONAL_EXPRESSION, "true");
                    self.emit(span, CONDITIONAL_EXPRESSION, "false");
                }
            }
            return;
        }
        match kind {
            AstKind::ForStatement(statement) if statement.test.is_none() => {
                self.emit(span, CONDITIONAL_EXPRESSION, "false");
            }
            AstKind::SwitchCase(case) if !case.consequent.is_empty() => {
                self.emit(span, CONDITIONAL_EXPRESSION, EMPTY);
            }
            _ => {}
        }
    }

    /// StringLiteral leaves strings that name something alone: module
    /// sources, export names, JSX attribute values, expression statements
    /// (directives), literal types, property and class field keys,
    /// `import()`, `require()` and `Symbol()` arguments.
    fn string_literal_mutable(&self, literal: &StringLiteral<'_>) -> bool {
        let is_key = |key: &PropertyKey<'_>| key.span() == literal.span;
        match self.babel_parent() {
            Some(
                AstKind::ImportDeclaration(_)
                | AstKind::ExportNamedDeclaration(_)
                | AstKind::ExportAllDeclaration(_)
                | AstKind::ExportDefaultDeclaration(_)
                | AstKind::TSExternalModuleReference(_)
                | AstKind::JSXAttribute(_)
                | AstKind::ExpressionStatement(_)
                | AstKind::TSLiteralType(_)
                | AstKind::ImportExpression(_),
            ) => false,
            Some(AstKind::ObjectProperty(property)) => !is_key(&property.key),
            Some(AstKind::BindingProperty(property)) => !is_key(&property.key),
            Some(AstKind::AssignmentTargetPropertyProperty(property)) => !is_key(&property.name),
            Some(AstKind::PropertyDefinition(property)) => !is_key(&property.key),
            Some(AstKind::CallExpression(call)) => {
                self.in_chain(call)
                    || !(is_identifier(&call.callee, "require")
                        || is_identifier(&call.callee, "Symbol"))
            }
            _ => true,
        }
    }

    /// The flags of `new RegExp(literal, flags)` when `literal` is its
    /// pattern: empty when there are none, `None` when it is not a pattern.
    fn regexp_constructor_flags(&self, literal: &StringLiteral<'_>) -> Option<String> {
        let Some(AstKind::NewExpression(new)) = self.babel_parent() else {
            return None;
        };
        let is_pattern = new
            .arguments
            .first()
            .and_then(Argument::as_expression)
            .is_some_and(|first| first.without_parentheses().span() == literal.span);
        if !is_identifier(&new.callee, "RegExp") || !is_pattern {
            return None;
        }
        let flags = match new.arguments.get(1).and_then(Argument::as_expression) {
            Some(Expression::StringLiteral(flags)) => flags.value.to_string(),
            _ => String::new(),
        };
        Some(flags)
    }

    /// BlockStatement keeps a constructor's body when emptying it would
    /// break the class: when it calls `super()` and the class has parameter
    /// properties or initialized fields.
    fn is_constructor_body_kept(&self, body: &FunctionBody<'_>) -> bool {
        let depth = self.stack.len();
        if depth < 4 {
            return false;
        }
        let (
            AstKind::ClassBody(class),
            AstKind::MethodDefinition(method),
            AstKind::Function(function),
        ) = (
            self.stack[depth - 4],
            self.stack[depth - 3],
            self.stack[depth - 2],
        )
        else {
            return false;
        };
        if method.kind != MethodDefinitionKind::Constructor {
            return false;
        }
        let parameter_properties = function
            .params
            .items
            .iter()
            .any(|parameter| parameter.has_modifier());
        let initialized_fields = class.body.iter().any(|member| {
            matches!(
                member,
                ClassElement::PropertyDefinition(property)
                    if property.value.is_some()
                        && !matches!(property.key, PropertyKey::PrivateIdentifier(_))
            )
        });
        if !(parameter_properties || initialized_fields) {
            return false;
        }
        let mut finder = SuperCallFinder { found: false };
        finder.visit_function_body(body);
        finder.found
    }
}

/// An expression-bodied arrow's expression.
fn arrow_body<'b, 'a>(arrow: &'b ArrowFunctionExpression<'a>) -> Option<&'b Expression<'a>> {
    match arrow.body.statements.first() {
        Some(Statement::ExpressionStatement(statement)) => Some(&statement.expression),
        _ => None,
    }
}

struct SuperCallFinder {
    found: bool,
}

impl<'a> Visit<'a> for SuperCallFinder {
    fn visit_call_expression(&mut self, call: &CallExpression<'a>) {
        if matches!(call.callee, Expression::Super(_)) {
            self.found = true;
        }
        walk::walk_call_expression(self, call);
    }
}

impl<'a> Visit<'a> for Enumerator<'_, 'a> {
    fn enter_node(&mut self, kind: AstKind<'a>) {
        self.stack.push(kind);
        // Babel has no node for parentheses or a parameter list, and starts
        // an empty JSX expression after the comment it holds.
        if !matches!(
            kind,
            AstKind::ParenthesizedExpression(_)
                | AstKind::FormalParameters(_)
                | AstKind::Elision(_)
                | AstKind::JSXEmptyExpression(_)
        ) {
            self.advance(kind.span().start);
        }
        if let AstKind::ChainExpression(chain) = kind {
            self.mark_chain_calls(chain);
        }
        if matches!(
            kind,
            AstKind::ExpressionStatement(_) | AstKind::ThrowStatement(_)
        ) {
            self.statements.push(StatementFrame {
                first: self.collected.len(),
                skipped: self.skipped,
            });
        }
        if self.in_type == 0 {
            self.mutate(kind);
        }
    }

    fn leave_node(&mut self, kind: AstKind<'a>) {
        if matches!(
            kind,
            AstKind::ExpressionStatement(_) | AstKind::ThrowStatement(_)
        ) && let Some(frame) = self.statements.pop()
        {
            self.filter_statement_call(kind.span(), &frame);
        }
        self.stack.pop();
    }

    fn visit_hashbang(&mut self, _hashbang: &Hashbang<'a>) {}

    fn visit_directive(&mut self, directive: &Directive<'a>) {
        self.advance(directive.span.start);
    }

    fn visit_import_declaration(&mut self, declaration: &ImportDeclaration<'a>) {
        self.skip(declaration.span);
    }

    fn visit_ts_import_equals_declaration(&mut self, declaration: &TSImportEqualsDeclaration<'a>) {
        self.skip(declaration.span);
    }

    fn visit_decorator(&mut self, decorator: &Decorator<'a>) {
        self.skip(decorator.span);
    }

    fn visit_ts_type_annotation(&mut self, annotation: &TSTypeAnnotation<'a>) {
        self.skip(annotation.span);
    }

    fn visit_ts_type_parameter_instantiation(
        &mut self,
        instantiation: &TSTypeParameterInstantiation<'a>,
    ) {
        self.skip(instantiation.span);
    }

    fn visit_ts_type_parameter_declaration(
        &mut self,
        declaration: &TSTypeParameterDeclaration<'a>,
    ) {
        self.skip(declaration.span);
    }

    fn visit_ts_as_expression(&mut self, expression: &TSAsExpression<'a>) {
        self.skip(expression.span);
    }

    fn visit_ts_interface_declaration(&mut self, declaration: &TSInterfaceDeclaration<'a>) {
        self.skip(declaration.span);
    }

    fn visit_ts_type_alias_declaration(&mut self, declaration: &TSTypeAliasDeclaration<'a>) {
        self.skip(declaration.span);
    }

    fn visit_ts_enum_declaration(&mut self, declaration: &TSEnumDeclaration<'a>) {
        self.skip(declaration.span);
    }

    fn visit_variable_declaration(&mut self, declaration: &VariableDeclaration<'a>) {
        if declaration.declare {
            self.skip(declaration.span);
        } else {
            walk::walk_variable_declaration(self, declaration);
        }
    }

    fn visit_ts_module_declaration(&mut self, declaration: &TSModuleDeclaration<'a>) {
        if declaration.declare {
            self.skip(declaration.span);
        } else {
            walk::walk_ts_module_declaration(self, declaration);
        }
    }

    fn visit_ts_global_declaration(&mut self, declaration: &TSGlobalDeclaration<'a>) {
        if declaration.declare {
            self.skip(declaration.span);
        } else {
            walk::walk_ts_global_declaration(self, declaration);
        }
    }

    fn visit_function(&mut self, function: &Function<'a>, flags: ScopeFlags) {
        if function.r#type == FunctionType::TSDeclareFunction {
            self.skip(function.span);
        } else {
            walk::walk_function(self, function, flags);
        }
    }

    fn visit_ts_type(&mut self, ts_type: &TSType<'a>) {
        self.in_type += 1;
        walk::walk_ts_type(self, ts_type);
        self.in_type -= 1;
    }

    /// Babel's expression-bodied arrow holds its expression directly, with no
    /// block or statement around it.
    fn visit_arrow_function_expression(&mut self, arrow: &ArrowFunctionExpression<'a>) {
        if !arrow.expression {
            walk::walk_arrow_function_expression(self, arrow);
            return;
        }
        let kind = AstKind::ArrowFunctionExpression(self.alloc(arrow));
        self.enter_node(kind);
        if let Some(type_parameters) = &arrow.type_parameters {
            self.visit_ts_type_parameter_declaration(type_parameters);
        }
        self.visit_formal_parameters(&arrow.params);
        if let Some(return_type) = &arrow.return_type {
            self.visit_ts_type_annotation(return_type);
        }
        if let Some(body) = arrow_body(arrow) {
            self.visit_expression(body);
        }
        self.leave_node(kind);
    }
}

impl Enumerator<'_, '_> {
    /// StrykerJS keeps a statement's CallExpression mutant only when it is
    /// the one unignored mutant in the statement. Its count starts from the
    /// top of a stack that a skipped node pushed onto and never popped, so a
    /// statement with a skipped node inside counts from past its own mutant
    /// and keeps it whatever else is there.
    fn filter_statement_call(&mut self, statement: Span, frame: &StatementFrame) {
        let Some(call) = self.collected[frame.first..]
            .iter()
            .position(|mutant| mutant.mutator == CALL_EXPRESSION && mutant.span == statement)
            .map(|position| frame.first + position)
        else {
            return;
        };
        if self.collected[call].ignored || self.skipped != frame.skipped {
            return;
        }
        let in_scope = self.collected[frame.first..]
            .iter()
            .filter(|mutant| !mutant.ignored && !mutant.removed)
            .count();
        if in_scope != 1 {
            self.collected[call].removed = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use oxc_allocator::Allocator;
    use oxc_parser::Parser;
    use serde_json::Value;

    use super::*;
    use crate::js_instrumenter::project_source_type;

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/stryker-parity");

    fn enumerate_file(path: &Path, source: &str) -> Vec<LedgerMutant> {
        let allocator = Allocator::default();
        let source_type = project_source_type(path).expect("a JavaScript or TypeScript file");
        let parsed = Parser::new(&allocator, source, source_type).parse();
        assert!(
            parsed.errors.is_empty(),
            "{} does not parse: {:?}",
            path.display(),
            parsed.errors
        );
        enumerate(&parsed.program, source)
    }

    fn render(mutator: &str, start: (u32, u32), end: (u32, u32), replacement: &str) -> String {
        format!(
            "{}:{}-{}:{} {mutator} {replacement}",
            start.0, start.1, end.0, end.1
        )
    }

    fn pair(value: &Value) -> (u32, u32) {
        let pair = value.as_array().expect("a [line, column] pair");
        (
            pair[0].as_u64().expect("a line") as u32,
            pair[1].as_u64().expect("a column") as u32,
        )
    }

    /// The byte offset of a 1-based line and UTF-16 column.
    fn offset(lines: &LineIndex<'_>, (line, column): (u32, u32)) -> usize {
        let start = lines.starts[line as usize - 1] as usize;
        let mut units = 0;
        for (index, character) in lines.source[start..].char_indices() {
            if units >= column - 1 {
                return start + index;
            }
            units += character.len_utf16() as u32;
        }
        lines.source.len()
    }

    /// A line diff of two lists: `-` for StrykerJS's entries Supercov lacks,
    /// `+` for Supercov's entries StrykerJS lacks, with a line of context.
    fn diff(expected: &[String], actual: &[String]) -> String {
        let (n, m) = (expected.len(), actual.len());
        let mut lengths = vec![vec![0usize; m + 1]; n + 1];
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                lengths[i][j] = if expected[i] == actual[j] {
                    lengths[i + 1][j + 1] + 1
                } else {
                    lengths[i + 1][j].max(lengths[i][j + 1])
                };
            }
        }
        let mut lines = Vec::new();
        let (mut i, mut j) = (0, 0);
        while i < n || j < m {
            if i < n && j < m && expected[i] == actual[j] {
                lines.push((' ', &expected[i]));
                i += 1;
                j += 1;
            } else if j < m && (i == n || lengths[i][j + 1] >= lengths[i + 1][j]) {
                lines.push(('+', &actual[j]));
                j += 1;
            } else {
                lines.push(('-', &expected[i]));
                i += 1;
            }
        }
        let changed: Vec<bool> = lines.iter().map(|(sign, _)| *sign != ' ').collect();
        let mut output = String::new();
        for (index, (sign, line)) in lines.iter().enumerate() {
            let near = changed[index.saturating_sub(1)..=(index + 1).min(lines.len() - 1)]
                .iter()
                .any(|changed| *changed);
            if near {
                output.push_str(&format!("  {sign} {line}\n"));
            }
        }
        output
    }

    #[test]
    fn js_mutants_match_stryker_on_every_fixture() {
        let mut fixtures: Vec<_> = fs::read_dir(FIXTURES)
            .expect("the parity fixtures")
            .map(|entry| entry.expect("a fixture").path())
            .filter(|path| {
                path.extension().is_some_and(|extension| {
                    ["ts", "tsx", "js", "mjs"].contains(&extension.to_str().unwrap_or(""))
                })
            })
            .collect();
        fixtures.sort();
        assert!(fixtures.len() >= 6, "fixtures: {fixtures:?}");
        let mut failures = Vec::new();
        for path in &fixtures {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let source = fs::read_to_string(path).expect("a readable fixture");
            let golden_path = path.with_file_name(format!("{name}.stryker.json"));
            let golden: Value = serde_json::from_str(
                &fs::read_to_string(&golden_path).expect("a golden beside each fixture"),
            )
            .expect("a JSON golden");
            assert_eq!(golden["mutantSet"], MUTANT_SET, "{name}");
            let lines = LineIndex::new(&source);
            let mut expected = Vec::new();
            let mut descriptor_mismatches = Vec::new();
            for entry in golden["mutants"].as_array().expect("mutants") {
                let mutator = entry["mutator"].as_str().unwrap();
                let (start, end) = (pair(&entry["start"]), pair(&entry["end"]));
                let replacement = entry["replacement"].as_str().unwrap();
                expected.push(render(mutator, start, end, replacement));
                let original = &source[offset(&lines, start)..offset(&lines, end)];
                let text = entry["text"].as_str().unwrap();
                let descriptor = descriptor_from_stryker(mutator, original, text);
                if descriptor.as_deref() != Some(replacement) {
                    descriptor_mismatches.push(format!(
                        "  {mutator} {original:?} -> {text:?}: script {replacement:?}, Rust {descriptor:?}"
                    ));
                }
            }
            let mutants = enumerate_file(path, &source);
            let actual: Vec<String> = mutants
                .iter()
                .map(|mutant| {
                    render(
                        mutant.mutator,
                        mutant.start,
                        mutant.end,
                        &mutant.replacement,
                    )
                })
                .collect();
            for mutant in &mutants {
                assert_eq!(
                    lines.position(mutant.span.start),
                    mutant.start,
                    "{name}: span and start disagree"
                );
            }
            if expected != actual {
                failures.push(format!(
                    "{name}: StrykerJS {} mutants, Supercov {} (- StrykerJS only, + Supercov only)\n{}",
                    expected.len(),
                    actual.len(),
                    diff(&expected, &actual)
                ));
            }
            if !descriptor_mismatches.is_empty() {
                failures.push(format!(
                    "{name}: descriptor_from_stryker disagrees with the script\n{}",
                    descriptor_mismatches.join("\n")
                ));
            }
        }
        assert!(failures.is_empty(), "\n{}", failures.join("\n"));
    }

    fn descriptor(mutator: &str, original: &str, replacement: &str) -> Option<String> {
        descriptor_from_stryker(mutator, original, replacement)
    }

    #[test]
    fn js_mutants_descriptors_for_operators() {
        assert_eq!(
            descriptor("ArithmeticOperator", "a + b", "a - b").as_deref(),
            Some("-")
        );
        assert_eq!(
            descriptor("ArithmeticOperator", "a * b + c", "a * b - c").as_deref(),
            Some("-")
        );
        assert_eq!(
            descriptor("ArithmeticOperator", "a + b * c", "a - b * c").as_deref(),
            Some("-")
        );
        assert_eq!(
            descriptor("ArithmeticOperator", "(a + b) % c", "(a + b) * c").as_deref(),
            Some("*")
        );
        assert_eq!(
            descriptor("EqualityOperator", "a < b", "a <= b").as_deref(),
            Some("<=")
        );
        assert_eq!(
            descriptor("EqualityOperator", "x === \"<\"", "x !== \"<\"").as_deref(),
            Some("!==")
        );
        assert_eq!(
            descriptor("EqualityOperator", "f<T>(a) < b", "f<T>(a) >= b").as_deref(),
            Some(">=")
        );
        assert_eq!(
            descriptor("LogicalOperator", "a ?? b", "a && b").as_deref(),
            Some("&&")
        );
        assert_eq!(
            descriptor("LogicalOperator", "a || b && c", "a && b && c").as_deref(),
            Some("&&")
        );
        assert_eq!(
            descriptor("AssignmentOperator", "x ??= 1", "x &&= 1").as_deref(),
            Some("&&=")
        );
        assert_eq!(
            descriptor("AssignmentOperator", "a[i += 1] <<= 2", "a[i += 1] >>= 2").as_deref(),
            Some(">>=")
        );
        assert_eq!(
            descriptor("UpdateOperator", "i++", "i--").as_deref(),
            Some("--")
        );
        assert_eq!(
            descriptor("UpdateOperator", "--i", "++i").as_deref(),
            Some("++")
        );
        assert_eq!(
            descriptor("UnaryOperator", "-x", "+x").as_deref(),
            Some("+")
        );
        assert_eq!(
            descriptor("UnaryOperator", "+x", "-x").as_deref(),
            Some("-")
        );
        assert_eq!(
            descriptor("UnaryOperator", "~-x", "-x").as_deref(),
            Some("removed")
        );
    }

    #[test]
    fn js_mutants_descriptors_for_literals_and_statements() {
        assert_eq!(
            descriptor("BooleanLiteral", "true", "false").as_deref(),
            Some("false")
        );
        assert_eq!(
            descriptor("BooleanLiteral", "false", "true").as_deref(),
            Some("true")
        );
        assert_eq!(
            descriptor("BooleanLiteral", "!true", "true").as_deref(),
            Some("removed")
        );
        assert_eq!(
            descriptor("ConditionalExpression", "a < b", "true").as_deref(),
            Some("true")
        );
        assert_eq!(
            descriptor("ConditionalExpression", "a < b", "false").as_deref(),
            Some("false")
        );
        assert_eq!(
            descriptor("ConditionalExpression", "for (;;) {}", "for (; false;) {}").as_deref(),
            Some("false")
        );
        assert_eq!(
            descriptor("ConditionalExpression", "case 1:\n  go();", "case 1:").as_deref(),
            Some("empty")
        );
        assert_eq!(
            descriptor("ConditionalExpression", "default:\n  go();", "default:").as_deref(),
            Some("empty")
        );
        assert_eq!(
            descriptor(
                "ConditionalExpression",
                "for (;;) {}",
                "// Retries.\n/* Forever. */\nfor (; false;) {}"
            )
            .as_deref(),
            Some("false")
        );
        assert_eq!(
            descriptor("StringLiteral", "'a'", "\"\"").as_deref(),
            Some("empty")
        );
        assert_eq!(
            descriptor("StringLiteral", "`a${b}`", "``").as_deref(),
            Some("empty")
        );
        assert_eq!(
            descriptor("StringLiteral", "\"\"", "\"Stryker was here!\"").as_deref(),
            Some("Stryker was here!")
        );
        assert_eq!(
            descriptor("StringLiteral", "``", "`Stryker was here!`").as_deref(),
            Some("Stryker was here!")
        );
        assert_eq!(
            descriptor("ArrowFunction", "x => x", "() => undefined").as_deref(),
            Some("undefined")
        );
        assert_eq!(
            descriptor("BlockStatement", "{ go(); }", "{}").as_deref(),
            Some("{}")
        );
        assert_eq!(
            descriptor("ObjectLiteral", "{ a: 1 }", "{}").as_deref(),
            Some("{}")
        );
        assert_eq!(
            descriptor("ArrayDeclaration", "[1]", "[]").as_deref(),
            Some("[]")
        );
        assert_eq!(
            descriptor("ArrayDeclaration", "[]", "[\"Stryker was here\"]").as_deref(),
            Some("[\"Stryker was here\"]")
        );
        assert_eq!(
            descriptor("ArrayDeclaration", "Array()", "Array([])").as_deref(),
            Some("Array([])")
        );
        assert_eq!(
            descriptor("ArrayDeclaration", "new Array(3)", "new Array()").as_deref(),
            Some("new Array()")
        );
        assert_eq!(
            descriptor("CallExpression", "go();", ";").as_deref(),
            Some("empty")
        );
        assert_eq!(descriptor("Regex", "/a+/", "/a/"), None);
        assert_eq!(descriptor("Unknown", "a", "b"), None);
    }

    #[test]
    fn js_mutants_descriptors_for_methods_and_chains() {
        assert_eq!(
            descriptor("MethodExpression", "s.toLowerCase()", "s.toUpperCase()").as_deref(),
            Some("toUpperCase")
        );
        assert_eq!(
            descriptor("MethodExpression", "s.trim()", "s").as_deref(),
            Some("removed")
        );
        assert_eq!(
            descriptor("MethodExpression", "a.min(b).trim()", "a.min(b)").as_deref(),
            Some("removed")
        );
        assert_eq!(
            descriptor("MethodExpression", "xs?.filter<T>(f)", "xs").as_deref(),
            Some("removed")
        );
        assert_eq!(
            descriptor("MethodExpression", "d.setUTCDate(1)", "d.setTime(1)").as_deref(),
            Some("setTime")
        );
        assert_eq!(
            descriptor("MethodExpression", "d.setTime(1)", "d.setDate(1)").as_deref(),
            Some("setDate")
        );
        assert_eq!(
            descriptor("MethodExpression", "a[trim]()", "a").as_deref(),
            Some("removed")
        );
        assert_eq!(descriptor("MethodExpression", "s.push(1)", "s"), None);
        assert_eq!(
            descriptor("OptionalChaining", "a?.b", "a.b").as_deref(),
            Some(".b")
        );
        assert_eq!(
            descriptor("OptionalChaining", "a?.b?.c", "a?.b.c").as_deref(),
            Some(".c")
        );
        assert_eq!(
            descriptor("OptionalChaining", "a?.[0]", "a[0]").as_deref(),
            Some("[]")
        );
        assert_eq!(
            descriptor("OptionalChaining", "a.b?.(x)", "a.b(x)").as_deref(),
            Some("()")
        );
        assert_eq!(
            descriptor("OptionalChaining", "this.#x?.#y", "this.#x.#y").as_deref(),
            Some(".#y")
        );
    }

    #[test]
    fn js_mutants_directive_comments() {
        let parse = |text: &str| parse_directive(text);
        assert_eq!(
            parse(" Stryker disable next-line all"),
            Some(DirectiveComment {
                disable: true,
                next_line: true,
                mutators: vec!["all".into()]
            })
        );
        assert_eq!(
            parse(" Stryker restore StringLiteral, BooleanLiteral: reason"),
            Some(DirectiveComment {
                disable: false,
                next_line: false,
                mutators: vec!["stringliteral".into(), "booleanliteral".into()]
            })
        );
        assert_eq!(
            parse("Stryker disable next-line"),
            Some(DirectiveComment {
                disable: true,
                next_line: false,
                mutators: vec!["next".into()]
            })
        );
        assert_eq!(parse("  Stryker disable all"), None);
        assert_eq!(parse("* Stryker disable all"), None);
        assert_eq!(parse(" stryker disable all"), None);
    }

    #[test]
    fn js_mutants_regexes_with_level_one_mutants() {
        for pattern in [
            "^a", "a$", "[ab]", "\\d", "a*", "a+", "a?", "a{2}", "a{2,}", "(?=a)", "(?<!a)",
        ] {
            assert!(regex_has_mutants(pattern, false), "{pattern}");
        }
        for pattern in [
            "",
            "abc",
            "a|b",
            "(a)",
            "(?:a)",
            "(?<name>a)",
            "a.b",
            "\\.",
            "\\[",
            "a{x}",
        ] {
            assert!(!regex_has_mutants(pattern, false), "{pattern}");
        }
        assert!(regex_has_mutants("\\p{L}", true));
        assert!(!regex_has_mutants("\\p{L}", false));
    }

    #[test]
    fn js_mutants_columns_count_utf16_units() {
        let source = "const a = \"🎉\"; const b = \"x\";\r\nc\u{2028}d";
        let lines = LineIndex::new(source);
        let second = source.find("\"x\"").unwrap() as u32;
        assert_eq!(lines.position(second), (1, 27));
        assert_eq!(lines.position(source.rfind('c').unwrap() as u32), (2, 1));
        assert_eq!(lines.position(source.rfind('d').unwrap() as u32), (3, 1));
    }
}
