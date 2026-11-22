//! Ruby code for a parser tree: a mutant's replacement, and the copies a
//! strong pass switches between.
//!
//! A node converted from the source is printed as written where its text is
//! on one line; anything else is printed from its children, on one line, so
//! a switch never moves the lines after it. The code only has to parse back
//! to the same tree, up to parentheses (`(begin x)` for `x`) and an empty
//! body written `nil`; `tests` checks that for every fixture mutant.

use super::tree::{Child, N, syntax};

pub(crate) struct Printer<'s> {
    source: &'s [u8],
    /// The local variables of the method printed in: a call by one of
    /// their names takes parentheses, or it would read the variable.
    locals: std::collections::HashSet<String>,
}

/// The local variable names `method` declares: its parameters and what it
/// assigns.
pub(crate) fn locals(method: &N) -> std::collections::HashSet<String> {
    fn walk(node: &N, into: &mut std::collections::HashSet<String>) {
        if matches!(
            node.ty,
            "arg"
                | "optarg"
                | "restarg"
                | "kwarg"
                | "kwoptarg"
                | "kwrestarg"
                | "blockarg"
                | "shadowarg"
                | "lvasgn"
                | "match_var"
        ) && let Some(name) = node.child(0).sym()
        {
            into.insert(name.to_owned());
        }
        for child in node.children.iter().filter_map(Child::node) {
            walk(child, into);
        }
    }
    let mut found = std::collections::HashSet::new();
    walk(method, &mut found);
    found
}

/// The code that replaces `located`'s span with `replacement`, in `method`.
pub(crate) fn replacement(
    source: &[u8],
    method: &N,
    located: &N,
    replacement: &N,
) -> Option<String> {
    let printer = Printer {
        source,
        locals: locals(method),
    };
    printer.in_place_of(located, replacement)
}

/// One line of code for `node`, in `method`.
pub(crate) fn one_line(source: &[u8], method: &N, node: &N) -> Option<String> {
    Printer {
        source,
        locals: locals(method),
    }
    .expression(node)
}

/// Node types that are never an expression on their own: they are printed
/// in their own slot's syntax and never wrapped.
fn slot_only(ty: &str) -> bool {
    matches!(
        ty,
        "pair"
            | "args"
            | "arg"
            | "optarg"
            | "restarg"
            | "kwarg"
            | "kwoptarg"
            | "kwrestarg"
            | "blockarg"
            | "shadowarg"
            | "procarg0"
            | "mlhs"
            | "forward_arg"
            | "kwnilarg"
            | "when"
            | "in_pattern"
            | "resbody"
            | "splat"
            | "block_pass"
            | "kwargs"
            | "kwsplat"
            | "regopt"
            | "if_guard"
            | "unless_guard"
            | "forwarded_args"
            | "forwarded_restarg"
            | "forwarded_kwrestarg"
            | "match_rest"
            | "match_var"
            | "match_as"
            | "pin"
            | "array_pattern"
            | "hash_pattern"
            | "match_nil_pattern"
    )
}

fn identifier(name: &str) -> bool {
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|first| first.is_alphabetic() || first == '_' || !first.is_ascii())
        && name.chars().all(|character| {
            character.is_alphanumeric() || character == '_' || !character.is_ascii()
        })
}

fn method_name(name: &str) -> bool {
    let base = name
        .strip_suffix('?')
        .or_else(|| name.strip_suffix('!'))
        .or_else(|| name.strip_suffix('='))
        .unwrap_or(name);
    identifier(base)
}

const OPERATORS: &[&str] = &[
    "!=", "!~", "%", "&", "*", "**", "+", "-", "/", "<", "<<", "<=", "<=>", "==", "===", "=~", ">",
    ">=", ">>", "^", "|",
];

/// A symbol's literal.
fn symbol(name: &str) -> String {
    if method_name(name)
        || OPERATORS.contains(&name)
        || matches!(name, "!" | "[]" | "[]=" | "+@" | "-@" | "~" | "~@")
        || name.strip_prefix("@@").is_some_and(identifier)
        || name.strip_prefix('@').is_some_and(identifier)
        || name.strip_prefix('$').is_some_and(identifier)
    {
        format!(":{name}")
    } else {
        format!(":{}", quoted(name.as_bytes()))
    }
}

/// A string's literal, every byte that is not plain printable ASCII
/// escaped.
fn quoted(bytes: &[u8]) -> String {
    let mut out = String::from("\"");
    out.push_str(&escaped(bytes, b'"'));
    out.push('"');
    out
}

fn escaped(bytes: &[u8], delimiter: u8) -> String {
    let mut out = String::new();
    let text = String::from_utf8(bytes.to_vec()).ok();
    match text {
        Some(text) => {
            let mut characters = text.chars().peekable();
            while let Some(character) = characters.next() {
                match character {
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\t' => out.push_str("\\t"),
                    '\r' => out.push_str("\\r"),
                    '\u{1b}' => out.push_str("\\e"),
                    '#' if matches!(characters.peek(), Some('{' | '$' | '@')) => {
                        out.push_str("\\#")
                    }
                    character if character as u32 == u32::from(delimiter) => {
                        out.push('\\');
                        out.push(character);
                    }
                    character if (character as u32) < 0x20 || character as u32 == 0x7f => {
                        out.push_str(&format!("\\x{:02X}", character as u32));
                    }
                    character => out.push(character),
                }
            }
        }
        None => {
            for byte in bytes {
                out.push_str(&format!("\\x{byte:02X}"));
            }
        }
    }
    out
}

/// Whether `node`'s span holds a heredoc's opening, whose body is outside it.
pub(crate) fn holds_heredoc(source: &[u8], node: &N) -> bool {
    node.has(syntax::HEREDOC)
        || node.span.is_some_and(|(start, end)| {
            std::str::from_utf8(&source[start..end]).is_ok_and(opens_heredoc)
        })
}

/// Whether `text` holds a heredoc's opening (`<<~EOS`, `<<-"X"`, `<<ID`).
fn opens_heredoc(text: &str) -> bool {
    let bytes = text.as_bytes();
    (0..bytes.len().saturating_sub(2)).any(|at| {
        if &bytes[at..at + 2] != b"<<" {
            return false;
        }
        let mut next = at + 2;
        if matches!(bytes.get(next), Some(b'~' | b'-')) {
            next += 1;
        }
        matches!(
            bytes.get(next),
            Some(b'"' | b'\'' | b'`' | b'A'..=b'Z' | b'a'..=b'z' | b'_')
        )
    })
}

/// A rational literal from its inspected value `(n/d)`: `nr`, or in
/// decimals where `d` divides a power of ten (`(7/2)` is `3.5r`), as a
/// literal written with a point is.
fn rational(text: &str) -> Option<String> {
    let (numerator, denominator) = text
        .trim_start_matches('(')
        .trim_end_matches(')')
        .split_once('/')?;
    let (numerator, denominator) = (
        numerator.parse::<i128>().ok()?,
        denominator.parse::<i128>().ok()?,
    );
    if denominator == 1 {
        return Some(format!("{numerator}r"));
    }
    let mut scale: i128 = 1;
    let mut places = 0;
    while scale % denominator != 0 {
        scale = scale.checked_mul(10)?;
        places += 1;
    }
    let scaled = numerator.unsigned_abs() * (scale / denominator).unsigned_abs();
    let sign = if numerator < 0 { "-" } else { "" };
    let whole = scaled / scale.unsigned_abs();
    let fraction = scaled % scale.unsigned_abs();
    Some(format!("{sign}{whole}.{fraction:0places$}r"))
}

/// A float's literal: Rust's `{:?}` always writes a point or an exponent
/// (`1.0`, `1e16`), as Ruby's literal needs.
fn float(bits: u64) -> String {
    format!("{:?}", f64::from_bits(bits))
}

impl Printer<'_> {
    fn slice(&self, node: &N) -> Option<String> {
        // An `elsif`'s text is no `if` of its own.
        if node.has(syntax::SYNTHETIC | syntax::WORD | syntax::HEREDOC | syntax::ELSIF) {
            return None;
        }
        let (start, end) = node.span?;
        let text = std::str::from_utf8(self.source.get(start..end)?).ok()?;
        // A heredoc's opening without its body is no code.
        (!text.contains('\n') && !opens_heredoc(text)).then(|| text.to_owned())
    }

    /// `replacement` written where `located` was.
    fn in_place_of(&self, located: &N, replacement: &N) -> Option<String> {
        // A `%w` word holds no code: the array around it is printed instead.
        // Nor can a heredoc's opening stand for it, or a span holding one.
        if located.has(syntax::WORD | syntax::HEREDOC) || self.holds_heredoc(located) {
            return None;
        }
        if located.ty == "if" && located.has(syntax::ELSIF) {
            // An `elsif` is its `if`'s else branch.
            return match replacement.ty {
                "if" if !replacement.has(syntax::TERNARY) || replacement.span.is_none() => {
                    let condition = self.expression(replacement.node(0)?)?;
                    let then_branch = self.statements(replacement.child(1))?;
                    match replacement.child(2) {
                        Child::Nil => Some(format!("elsif {condition} then {then_branch}")),
                        else_branch => Some(format!(
                            "elsif {condition} then {then_branch} else {}",
                            self.statements(else_branch)?
                        )),
                    }
                }
                _ => Some(format!("else {}", self.statement(replacement)?)),
            };
        }
        if matches!(replacement.ty, "def" | "defs") {
            return self.expression(replacement);
        }
        // Slots with their own syntax.
        if located.has(syntax::REGEXP_CONTENT) && replacement.ty == "str" {
            let Child::Str(bytes) = replacement.child(0) else {
                return None;
            };
            let text = String::from_utf8_lossy(bytes);
            return Some(if located.has(syntax::PERCENT_REGEXP) {
                text.into_owned()
            } else {
                text.replace('/', "\\/")
            });
        }
        if located.has(syntax::STRING_CONTENT) && replacement.ty == "str" {
            let Child::Str(bytes) = replacement.child(0) else {
                return None;
            };
            return Some(escaped(bytes, b'"'));
        }
        if located.ty == "sym" && located.has(syntax::LABEL) && replacement.ty == "sym" {
            // The label's name; its colon stays.
            let name = replacement.child(0).sym()?;
            return Some(if method_name(name) {
                name.to_owned()
            } else {
                quoted(name.as_bytes())
            });
        }
        if located.has(syntax::TARGET) {
            return self.target(replacement);
        }
        if matches!(replacement.ty, "lvasgn" | "ivasgn" | "gvasgn" | "cvasgn")
            && replacement.children.len() == 1
        {
            // An operator assignment's target.
            return self.target(replacement);
        }
        if located.ty == "regopt" && replacement.ty == "regopt" {
            return Some(
                replacement
                    .children
                    .iter()
                    .filter_map(Child::sym)
                    .collect::<String>(),
            );
        }
        if located.ty == "args" || replacement.ty == "args" {
            return self.arguments_list(replacement, located.has(syntax::BLOCK_PARAMETERS));
        }
        if matches!(located.ty, "rescue" | "ensure") && located.has(syntax::SYNTHETIC) {
            // A body with its clauses, as a method or `begin` holds it.
            return match replacement.ty {
                "rescue" => self.rescue_body(replacement),
                "ensure" => self.ensure_body(replacement),
                _ => self.statements(&Child::Node(std::rc::Rc::new(replacement.clone()))),
            };
        }
        if located.ty == "begin" && located.has(syntax::SYNTHETIC) {
            return self.statements(&Child::Node(std::rc::Rc::new(replacement.clone())));
        }
        if slot_only(replacement.ty) || slot_only(located.ty) {
            return self.slot(replacement);
        }
        let text = self.expression(replacement)?;
        // `{` after a command's name opens a block.
        if text.starts_with('{') && !located.has(syntax::STATEMENT) {
            return Some(format!("({text})"));
        }
        if located.has(syntax::STATEMENT) || self.atomic(replacement) {
            Some(text)
        } else {
            Some(format!("({text})"))
        }
    }

    fn holds_heredoc(&self, node: &N) -> bool {
        holds_heredoc(self.source, node)
    }

    /// A node in its own slot's syntax.
    fn slot(&self, node: &N) -> Option<String> {
        // A guard's span is its condition's: its keyword is printed here.
        if !matches!(node.ty, "if_guard" | "unless_guard")
            && let Some(text) = self.slice(node)
        {
            return Some(text);
        }
        match node.ty {
            "pair" => {
                let key = node.node(0)?;
                let value = self.argument(node.node(1)?)?;
                if node.has(syntax::LABEL) && key.ty == "sym" {
                    let name = key.child(0).sym()?;
                    if method_name(name) {
                        return Some(format!("{name}: {value}"));
                    }
                    return Some(format!("{}: {value}", quoted(name.as_bytes())));
                }
                Some(format!("{} => {value}", self.argument(key)?))
            }
            "arg" | "kwarg" | "restarg" | "kwrestarg" | "blockarg" | "shadowarg" | "optarg"
            | "kwoptarg" | "procarg0" | "mlhs" | "forward_arg" | "kwnilarg" => self.parameter(node),
            "args" => self.arguments_list(node, true),
            "when" => {
                let (body, conditions) = node.children.split_last()?;
                let conditions = conditions
                    .iter()
                    .map(|condition| self.argument(condition.node()?))
                    .collect::<Option<Vec<_>>>()?;
                let body = self.statements(body)?;
                Some(format!("when {} then {body}", conditions.join(", ")))
            }
            "in_pattern" => {
                let pattern = self.pattern(node.node(0)?)?;
                let guard = match node.node(1) {
                    Some(guard) => format!(" {}", self.slot(guard)?),
                    None => String::new(),
                };
                let body = self.statements(node.child(2))?;
                Some(format!("in {pattern}{guard} then {body}"))
            }
            "if_guard" => Some(format!("if {}", self.expression(node.node(0)?)?)),
            "unless_guard" => Some(format!("unless {}", self.expression(node.node(0)?)?)),
            "resbody" => self.resbody(node),
            "splat" => match node.node(0) {
                Some(inner) => Some(format!("*{}", self.operand(inner)?)),
                None => Some("*".into()),
            },
            "kwsplat" => Some(format!("**{}", self.operand(node.node(0)?)?)),
            "block_pass" => match node.node(0) {
                Some(inner) => Some(format!("&{}", self.operand(inner)?)),
                None => Some("&".into()),
            },
            "kwargs" => {
                let pairs = node
                    .children
                    .iter()
                    .map(|pair| self.slot(pair.node()?))
                    .collect::<Option<Vec<_>>>()?;
                Some(pairs.join(", "))
            }
            "forwarded_args" => Some("...".into()),
            "forwarded_restarg" => Some("*".into()),
            "forwarded_kwrestarg" => Some("**".into()),
            "regopt" => Some(node.children.iter().filter_map(Child::sym).collect()),
            "match_rest" | "match_var" | "match_as" | "pin" | "array_pattern" | "hash_pattern"
            | "match_nil_pattern" => self.pattern(node),
            _ => self.expression(node),
        }
    }

    fn parameter(&self, node: &N) -> Option<String> {
        let name = || node.child(0).sym().map(str::to_owned);
        Some(match node.ty {
            "arg" | "shadowarg" => name()?,
            "optarg" => format!("{} = {}", name()?, self.argument(node.node(1)?)?),
            "restarg" => format!("*{}", name().unwrap_or_default()),
            "kwarg" => format!("{}:", name()?),
            "kwoptarg" => format!("{}: {}", name()?, self.argument(node.node(1)?)?),
            "kwrestarg" => format!("**{}", name().unwrap_or_default()),
            "blockarg" => format!("&{}", name().unwrap_or_default()),
            "forward_arg" => "...".into(),
            "kwnilarg" => "**nil".into(),
            "procarg0" => {
                if node.children.len() == 1 {
                    self.parameter(node.node(0)?)?
                } else {
                    let inner = node
                        .children
                        .iter()
                        .map(|child| self.parameter(child.node()?))
                        .collect::<Option<Vec<_>>>()?;
                    format!("({})", inner.join(", "))
                }
            }
            "mlhs" => {
                let inner = node
                    .children
                    .iter()
                    .map(|child| self.parameter(child.node()?))
                    .collect::<Option<Vec<_>>>()?;
                format!("({})", inner.join(", "))
            }
            _ => return None,
        })
    }

    /// Parameters, as a block's `|...|` or a method's `(...)`.
    fn arguments_list(&self, node: &N, block: bool) -> Option<String> {
        let mut plain = Vec::new();
        let mut shadow = Vec::new();
        for child in &node.children {
            let child = child.node()?;
            if child.ty == "shadowarg" {
                shadow.push(self.parameter(child)?);
            } else {
                plain.push(self.parameter(child)?);
            }
        }
        let shadow = if shadow.is_empty() {
            String::new()
        } else {
            format!("; {}", shadow.join(", "))
        };
        if block {
            if plain.is_empty() && shadow.is_empty() {
                return Some(String::new());
            }
            // `|a,|`: one plain parameter, not the block's whole argument.
            let comma = if node.node(0).is_some_and(|first| first.ty == "arg") && plain.len() == 1 {
                ","
            } else {
                ""
            };
            return Some(format!("|{}{comma}{shadow}|", plain.join(", ")));
        }
        Some(format!("({}{shadow})", plain.join(", ")))
    }

    /// A statement list's code: statements joined with `;`, `nil` for none.
    fn statements(&self, child: &Child) -> Option<String> {
        match child {
            Child::Nil => Some("nil".into()),
            Child::Node(node) if node.ty == "begin" && node.has(syntax::SYNTHETIC) => {
                let parts = node
                    .children
                    .iter()
                    .map(|statement| self.statement(statement.node()?))
                    .collect::<Option<Vec<_>>>()?;
                Some(parts.join("; "))
            }
            Child::Node(node)
                if node.ty == "begin" && node.span.is_none() && !node.children.is_empty() =>
            {
                // A list a mutation made (`emit_concat`, a prepended
                // assignment): its statements, each a statement.
                let parts = node
                    .children
                    .iter()
                    .map(|statement| self.statement(statement.node()?))
                    .collect::<Option<Vec<_>>>()?;
                Some(parts.join("; "))
            }
            Child::Node(node) => self.statement(node),
            _ => None,
        }
    }

    fn statement(&self, node: &N) -> Option<String> {
        match node.ty {
            "rescue"
                if node.has(syntax::SYNTHETIC)
                    || node.span.is_none()
                        && node.child(1).node().is_some_and(|resbody| {
                            !resbody.child(0).is_nil() || !resbody.child(1).is_nil()
                        }) =>
            {
                Some(format!("begin; {}; end", self.rescue_body(node)?))
            }
            "ensure" => Some(format!("begin; {}; end", self.ensure_body(node)?)),
            "begin" if node.span.is_none() && node.children.len() > 1 => Some(format!(
                "({})",
                self.statements(&Child::Node(std::rc::Rc::new(node.clone())))?
            )),
            _ => self.expression(node),
        }
    }

    fn resbody(&self, node: &N) -> Option<String> {
        let mut head = String::from("rescue");
        if let Some(exceptions) = node.node(0) {
            let list = exceptions
                .children
                .iter()
                .map(|exception| self.argument(exception.node()?))
                .collect::<Option<Vec<_>>>()?;
            head.push(' ');
            head.push_str(&list.join(", "));
        }
        if let Some(target) = node.node(1) {
            head.push_str(" => ");
            head.push_str(&self.target(target)?);
        }
        Some(format!("{head}; {}", self.statements(node.child(2))?))
    }

    /// A rescue's body and clauses, as a method or `begin` holds them.
    fn rescue_body(&self, node: &N) -> Option<String> {
        let (else_branch, rest) = node.children.split_last()?;
        let (body, clauses) = rest.split_first()?;
        let mut parts = Vec::new();
        if !body.is_nil() {
            parts.push(self.statements(body)?);
        }
        for clause in clauses {
            parts.push(self.resbody(clause.node()?)?);
        }
        if !else_branch.is_nil() {
            parts.push(format!("else; {}", self.statements(else_branch)?));
        }
        Some(parts.join("; "))
    }

    fn ensure_body(&self, node: &N) -> Option<String> {
        let body = match node.node(0) {
            Some(body) if body.ty == "rescue" => self.rescue_body(body)?,
            _ => self.statements(node.child(0))?,
        };
        Some(format!(
            "{body}; ensure; {}",
            self.statements(node.child(1))?
        ))
    }

    /// Whether the code of `node` can be a receiver or an operand as it is.
    pub(crate) fn atomic(&self, node: &N) -> bool {
        match node.ty {
            "lvar" | "ivar" | "gvar" | "cvar" | "const" | "cbase" | "self" | "nil" | "true"
            | "false" | "str" | "dstr" | "sym" | "dsym" | "xstr" | "regexp" | "array" | "index"
            | "nth_ref" | "back_ref" | "zsuper" | "kwbegin" | "defined?" | "lambda"
            | "__FILE__" | "__LINE__" | "__ENCODING__" => true,
            "int" | "rational" | "complex" => match node.child(0) {
                Child::Int(value) => *value >= 0,
                Child::Raw(text) => !text.starts_with("(-") && !text.contains("(0-"),
                Child::BigInt(digits) => !digits.starts_with('-'),
                _ => false,
            },
            "float" => match node.child(0) {
                Child::Float(bits) => {
                    f64::from_bits(*bits) >= 0.0 && f64::from_bits(*bits).is_sign_positive()
                }
                _ => false,
            },
            "begin" => {
                node.span.is_some() && !node.has(syntax::SYNTHETIC)
                    || node.span.is_none() && node.children.is_empty()
            }
            "hash" => !node.has(syntax::SYNTHETIC),
            "send" | "csend" => {
                let selector = node.child(1).sym().unwrap_or_default();
                if OPERATORS.contains(&selector)
                    || matches!(selector, "!" | "-@" | "+@" | "~" | "[]=")
                {
                    return false;
                }
                if selector.ends_with('=') && !node.child(0).is_nil() {
                    return false;
                }
                match node.span {
                    // A call printed here has its arguments in parentheses.
                    None => true,
                    Some(_) => match self.slice(node) {
                        Some(text) => {
                            node.children.len() == 2 || text.ends_with(')') || text.ends_with(']')
                        }
                        None => false,
                    },
                }
            }
            "block" | "numblock" | "itblock" => {
                node.has(syntax::BRACES)
                    && node
                        .node(0)
                        .is_some_and(|call| call.ty == "lambda" || self.atomic(call))
            }
            "super" | "yield" => {
                node.span.is_none() || self.slice(node).is_some_and(|text| text.ends_with(')'))
            }
            _ => false,
        }
    }

    /// `node` as a receiver or operand.
    fn operand(&self, node: &N) -> Option<String> {
        let text = self.expression(node)?;
        Some(if self.atomic(node) {
            text
        } else {
            format!("({text})")
        })
    }

    /// `node` as an argument of a call, an element of a literal, a value.
    fn argument(&self, node: &N) -> Option<String> {
        if slot_only(node.ty) {
            return self.slot(node);
        }
        let text = self.expression(node)?;
        let wrap = match node.ty {
            "if" => !node.has(syntax::TERNARY),
            "while" | "until" | "while_post" | "until_post" | "and" | "or" | "rescue" | "masgn"
            | "ensure" | "return" | "next" | "break" | "match_pattern" | "match_pattern_p" => true,
            "send" | "csend" | "super" | "yield" => !self.atomic(node) && !self.binary(node),
            "block" => !self.atomic(node),
            _ => false,
        };
        Some(if wrap { format!("({text})") } else { text })
    }

    fn binary(&self, node: &N) -> bool {
        node.child(1)
            .sym()
            .is_some_and(|selector| OPERATORS.contains(&selector) || selector == "!")
    }

    fn arguments(&self, children: &[Child]) -> Option<Vec<String>> {
        children
            .iter()
            .map(|child| self.argument(child.node()?))
            .collect()
    }

    /// An assignment target.
    fn target(&self, node: &N) -> Option<String> {
        match node.ty {
            "lvasgn" | "ivasgn" | "gvasgn" | "cvasgn" => node.child(0).sym().map(str::to_owned),
            "send" | "csend" => {
                let receiver = self.operand(node.node(0)?)?;
                let name = node.child(1).sym()?;
                let name = name.strip_suffix('=').unwrap_or(name);
                let dot = if node.ty == "csend" { "&." } else { "." };
                Some(format!("{receiver}{dot}{name}"))
            }
            "indexasgn" | "index" => {
                let receiver = self.operand(node.node(0)?)?;
                let indices = self.arguments(&node.children[1..])?;
                Some(format!("{receiver}[{}]", indices.join(", ")))
            }
            "mlhs" => {
                let inner = node
                    .children
                    .iter()
                    .map(|child| self.target(child.node()?))
                    .collect::<Option<Vec<_>>>()?;
                // `(a,)`: one target is still a nested assignment.
                let comma = if inner.len() == 1 { "," } else { "" };
                Some(format!("({}{comma})", inner.join(", ")))
            }
            "splat" => match node.node(0) {
                Some(inner) => Some(format!("*{}", self.target(inner)?)),
                None => Some("*".into()),
            },
            _ => None,
        }
    }

    fn pattern(&self, node: &N) -> Option<String> {
        if let Some(text) = self.slice(node) {
            return Some(text);
        }
        match node.ty {
            "match_var" => node.child(0).sym().map(str::to_owned),
            "match_as" => Some(format!(
                "{} => {}",
                self.pattern(node.node(0)?)?,
                self.pattern(node.node(1)?)?
            )),
            "match_alt" => Some(format!(
                "{} | {}",
                self.pattern(node.node(0)?)?,
                self.pattern(node.node(1)?)?
            )),
            "pin" => Some(format!("^{}", self.operand(node.node(0)?)?)),
            "match_rest" => match node.node(0) {
                Some(inner) => Some(format!("*{}", self.pattern(inner)?)),
                None => Some("*".into()),
            },
            "match_nil_pattern" => Some("**nil".into()),
            "array_pattern" => {
                let inner = node
                    .children
                    .iter()
                    .map(|child| self.pattern(child.node()?))
                    .collect::<Option<Vec<_>>>()?;
                Some(format!("[{}]", inner.join(", ")))
            }
            "hash_pattern" => {
                let inner = node
                    .children
                    .iter()
                    .map(|child| {
                        let child = child.node()?;
                        match child.ty {
                            "pair" => {
                                let key = child.node(0)?.child(0).sym()?.to_owned();
                                Some(format!("{key}: {}", self.pattern(child.node(1)?)?))
                            }
                            "match_var" => Some(format!("{}:", child.child(0).sym()?)),
                            "match_rest" => match child.node(0) {
                                Some(inner) => Some(format!("**{}", self.pattern(inner)?)),
                                None => Some("**".into()),
                            },
                            "match_nil_pattern" => Some("**nil".into()),
                            _ => None,
                        }
                    })
                    .collect::<Option<Vec<_>>>()?;
                Some(format!("{{{}}}", inner.join(", ")))
            }
            _ => self.expression(node),
        }
    }

    /// A method's code; a singleton method's children start at `offset`.
    fn def(&self, node: &N, receiver: String, offset: usize) -> Option<String> {
        let parameters = self.arguments_list(node.node(offset + 1)?, false)?;
        Some(format!(
            "def {receiver}{}{parameters}; {}; end",
            node.child(offset).sym()?,
            self.statements(node.child(offset + 2))?
        ))
    }

    /// A call's code: receiver, selector and arguments.
    fn call(&self, node: &N) -> Option<String> {
        let selector = node.child(1).sym()?;
        let receiver = node.node(0);
        let arguments = &node.children[2..];
        let dot = if node.ty == "csend" { "&." } else { "." };
        if node.ty == "send"
            && let Some(receiver) = receiver
        {
            if OPERATORS.contains(&selector) && arguments.len() == 1 {
                let right = arguments[0].node()?;
                return Some(format!(
                    "{} {selector} {}",
                    self.operand(receiver)?,
                    self.operand(right)?
                ));
            }
            if arguments.is_empty() {
                match selector {
                    "!" => return Some(format!("!{}", self.operand(receiver)?)),
                    "-@" => return Some(format!("-{}", self.operand(receiver)?)),
                    "+@" => return Some(format!("+{}", self.operand(receiver)?)),
                    "~" | "~@" => return Some(format!("~{}", self.operand(receiver)?)),
                    _ => {}
                }
            }
            if selector == "[]" {
                return Some(format!(
                    "{}[{}]",
                    self.operand(receiver)?,
                    self.arguments(arguments)?.join(", ")
                ));
            }
        }
        let mut head = String::new();
        if let Some(receiver) = receiver {
            head.push_str(&self.operand(receiver)?);
            head.push_str(dot);
        }
        // `a.b = c`; `a.[]=(i)` is called by name.
        if selector.ends_with('=')
            && method_name(selector)
            && receiver.is_some()
            && arguments.len() == 1
        {
            let name = &selector[..selector.len() - 1];
            return Some(format!(
                "{head}{name} = {}",
                self.argument(arguments[0].node()?)?
            ));
        }
        if method_name(selector) {
            head.push_str(selector);
        } else if receiver.is_some() {
            // An operator called by name, `a.+(b)`, `a.!`.
            head.push_str(selector);
        } else {
            return None;
        }
        if arguments.is_empty() {
            // `Integer` alone would be the constant, `x` alone a local.
            let constant_like = receiver.is_none()
                && (selector.starts_with(|first: char| first.is_uppercase())
                    || self.locals.contains(selector));
            return Some(if constant_like {
                format!("{head}()")
            } else {
                head
            });
        }
        Some(format!("{head}({})", self.arguments(arguments)?.join(", ")))
    }

    fn block(&self, node: &N) -> Option<String> {
        let call = node.node(0)?;
        let body = self.statements(node.child(2))?;
        let parameters = match node.child(1) {
            Child::Node(arguments) => self.arguments_list(arguments, call.ty != "lambda")?,
            _ => String::new(),
        };
        if call.ty == "lambda" {
            return Some(format!("->{parameters} {{ {body} }}"));
        }
        let head = self.expression(call)?;
        let head = if self.atomic(call) {
            head
        } else {
            format!("({head})")
        };
        let parameters = if parameters.is_empty() {
            String::new()
        } else {
            format!(" {parameters}")
        };
        if node.has(syntax::BRACES) || node.span.is_none() {
            Some(format!("{head} {{{parameters} {body} }}"))
        } else {
            Some(format!("{head} do{parameters} {body} end"))
        }
    }

    /// One line of code for an expression.
    pub(crate) fn expression(&self, node: &N) -> Option<String> {
        if let Some(text) = self.slice(node) {
            return Some(text);
        }
        let children = &node.children;
        Some(match node.ty {
            "nil" | "true" | "false" | "self" | "__FILE__" | "__LINE__" | "__ENCODING__"
            | "redo" | "retry" => node.ty.to_owned(),
            "zsuper" => "super".into(),
            "int" => match node.child(0) {
                Child::Int(value) => value.to_string(),
                Child::BigInt(digits) => digits.clone(),
                _ => return None,
            },
            "float" => match node.child(0) {
                Child::Float(bits) => float(*bits),
                _ => return None,
            },
            "rational" => match node.child(0) {
                Child::Raw(text) => rational(text)?,
                _ => return None,
            },
            "complex" => match node.child(0) {
                Child::Raw(text) => {
                    let inner = text.trim_start_matches('(').trim_end_matches(')');
                    let imaginary = inner
                        .strip_prefix("0+")
                        .or_else(|| inner.strip_prefix('0'))?;
                    imaginary.to_owned()
                }
                _ => return None,
            },
            "str" => match node.child(0) {
                Child::Str(bytes) => quoted(bytes),
                _ => return None,
            },
            "sym" => symbol(node.child(0).sym()?),
            "dstr" | "dsym" | "xstr" => {
                let mut inner = String::new();
                for part in children {
                    let part = part.node()?;
                    match part.ty {
                        "str" => {
                            let Child::Str(bytes) = part.child(0) else {
                                return None;
                            };
                            inner.push_str(&escaped(
                                bytes,
                                if node.ty == "xstr" { b'`' } else { b'"' },
                            ));
                        }
                        "begin" => {
                            let body = if part.children.is_empty() {
                                String::new()
                            } else if part.children.len() == 1 {
                                self.statement(part.node(0)?)?
                            } else {
                                self.statements(&Child::Node(std::rc::Rc::new(N {
                                    ty: "begin",
                                    children: part.children.clone(),
                                    span: None,
                                    syntax: syntax::SYNTHETIC,
                                })))?
                            };
                            inner.push_str(&format!("#{{{body}}}"));
                        }
                        _ => inner.push_str(&format!("#{{{}}}", self.expression(part)?)),
                    }
                }
                match node.ty {
                    "dstr" => format!("\"{inner}\""),
                    "dsym" => format!(":\"{inner}\""),
                    _ => format!("`{inner}`"),
                }
            }
            "regexp" => {
                let mut inner = String::new();
                let mut options = String::new();
                for part in children {
                    let part = part.node()?;
                    match part.ty {
                        "str" => {
                            let Child::Str(bytes) = part.child(0) else {
                                return None;
                            };
                            inner.push_str(&String::from_utf8_lossy(bytes).replace('/', "\\/"));
                        }
                        "regopt" => options = part.children.iter().filter_map(Child::sym).collect(),
                        "begin" => inner.push_str(&format!(
                            "#{{{}}}",
                            self.statements(&part.children.first().cloned().unwrap_or(Child::Nil))?
                        )),
                        _ => inner.push_str(&format!("#{{{}}}", self.expression(part)?)),
                    }
                }
                format!("/{inner}/{options}")
            }
            "array" => format!("[{}]", self.arguments(children)?.join(", ")),
            "hash" => {
                let pairs = children
                    .iter()
                    .map(|pair| self.slot(pair.node()?))
                    .collect::<Option<Vec<_>>>()?;
                if pairs.is_empty() {
                    "{}".into()
                } else {
                    format!("{{ {} }}", pairs.join(", "))
                }
            }
            "irange" | "erange" => {
                let operator = if node.ty == "irange" { ".." } else { "..." };
                let left = match node.node(0) {
                    Some(left) => self.operand(left)?,
                    None => String::new(),
                };
                let right = match node.node(1) {
                    Some(right) => self.operand(right)?,
                    None => String::new(),
                };
                format!("{left}{operator}{right}")
            }
            "lvar" | "ivar" | "gvar" | "cvar" | "back_ref" => node.child(0).sym()?.to_owned(),
            "nth_ref" => match node.child(0) {
                Child::Int(number) => format!("${number}"),
                _ => return None,
            },
            "const" => {
                let name = node.child(1).sym()?;
                match node.node(0) {
                    Some(scope) if scope.ty == "cbase" => format!("::{name}"),
                    Some(scope) => format!("{}::{name}", self.operand(scope)?),
                    None => name.to_owned(),
                }
            }
            "cbase" => "::".into(),
            "send" | "csend" => self.call(node)?,
            "index" => {
                let receiver = self.operand(node.node(0)?)?;
                format!("{receiver}[{}]", self.arguments(&children[1..])?.join(", "))
            }
            "indexasgn" => {
                let receiver = self.operand(node.node(0)?)?;
                let (value, indices) = children[1..].split_last()?;
                let indices = self.arguments(indices)?;
                let value = self.argument(value.node()?)?;
                format!("{receiver}[{}] = {value}", indices.join(", "))
            }
            "lvasgn" | "ivasgn" | "gvasgn" | "cvasgn" => {
                format!(
                    "{} = {}",
                    node.child(0).sym()?,
                    self.argument(node.node(1)?)?
                )
            }
            "op_asgn" => format!(
                "{} {}= {}",
                self.target(node.node(0)?)?,
                node.child(1).sym()?,
                self.argument(node.node(2)?)?
            ),
            "or_asgn" => format!(
                "{} ||= {}",
                self.target(node.node(0)?)?,
                self.argument(node.node(1)?)?
            ),
            "and_asgn" => format!(
                "{} &&= {}",
                self.target(node.node(0)?)?,
                self.argument(node.node(1)?)?
            ),
            "masgn" => {
                let targets = node.node(0)?;
                let left = targets
                    .children
                    .iter()
                    .map(|child| self.target(child.node()?))
                    .collect::<Option<Vec<_>>>()?;
                let right = node.node(1)?;
                let right = if right.ty == "array" && right.has(syntax::SYNTHETIC) {
                    self.arguments(&right.children)?.join(", ")
                } else {
                    self.argument(right)?
                };
                // `a, = b`: one target is still a multiple assignment.
                let comma = if left.len() == 1 { "," } else { "" };
                format!("{}{comma} = {right}", left.join(", "))
            }
            "and" => format!(
                "{} && {}",
                self.operand(node.node(0)?)?,
                self.operand(node.node(1)?)?
            ),
            "or" => format!(
                "{} || {}",
                self.operand(node.node(0)?)?,
                self.operand(node.node(1)?)?
            ),
            "if" => {
                let condition = self.expression(node.node(0)?)?;
                match (node.child(1), node.child(2)) {
                    (Child::Node(then_branch), Child::Node(else_branch))
                        if node.has(syntax::TERNARY)
                            && then_branch.ty != "begin"
                            && else_branch.ty != "begin" =>
                    {
                        format!(
                            "{} ? {} : {}",
                            self.operand(node.node(0)?)?,
                            self.operand(then_branch)?,
                            self.operand(else_branch)?
                        )
                    }
                    (then_branch, Child::Nil) => {
                        format!("if {condition} then {} end", self.statements(then_branch)?)
                    }
                    (Child::Nil, else_branch) => {
                        format!(
                            "if {condition} then else {} end",
                            self.statements(else_branch)?
                        )
                    }
                    (then_branch, else_branch) => format!(
                        "if {condition} then {} else {} end",
                        self.statements(then_branch)?,
                        self.statements(else_branch)?
                    ),
                }
            }
            "while" | "until" => format!(
                "{} {} do {} end",
                node.ty,
                self.expression(node.node(0)?)?,
                match node.child(1) {
                    Child::Nil => String::new(),
                    body => self.statements(body)?,
                }
            ),
            "while_post" | "until_post" => {
                let keyword = node.ty.trim_end_matches("_post");
                format!(
                    "{} {keyword} {}",
                    self.expression(node.node(1)?)?,
                    self.expression(node.node(0)?)?
                )
            }
            "for" => format!(
                "for {} in {} do {} end",
                self.target(node.node(0)?)?,
                self.expression(node.node(1)?)?,
                self.statements(node.child(2))?
            ),
            "case" | "case_match" => {
                let mut text = String::from("case");
                if let Some(subject) = node.node(0) {
                    text.push(' ');
                    text.push_str(&self.expression(subject)?);
                    // `case x in y` would read `x in y` as a test.
                    text.push(';');
                }
                let (else_branch, rest) = children.split_last()?;
                for (position, clause) in rest.get(1..)?.iter().enumerate() {
                    text.push_str(if position == 0 && node.node(0).is_some() {
                        " "
                    } else {
                        "; "
                    });
                    text.push_str(&self.slot(clause.node()?)?);
                }
                if !else_branch.is_nil() {
                    text.push_str("; else ");
                    text.push_str(&self.statements(else_branch)?);
                }
                text.push_str("; end");
                text
            }
            "begin" => {
                if children.is_empty() {
                    "()".into()
                } else {
                    let inner = children
                        .iter()
                        .map(|child| self.statement(child.node()?))
                        .collect::<Option<Vec<_>>>()?;
                    format!("({})", inner.join("; "))
                }
            }
            "kwbegin" => {
                let inner = children
                    .iter()
                    .map(|child| {
                        let child = child.node()?;
                        match child.ty {
                            "rescue" => self.rescue_body(child),
                            "ensure" => self.ensure_body(child),
                            _ => self.statement(child),
                        }
                    })
                    .collect::<Option<Vec<_>>>()?;
                format!("begin; {}; end", inner.join("; "))
            }
            "rescue" => {
                let modifier = node.children.len() == 3
                    && node.node(1).is_some_and(|resbody| {
                        resbody.child(0).is_nil() && resbody.child(1).is_nil()
                    })
                    && !node.child(0).is_nil();
                if modifier {
                    let resbody = node.node(1)?;
                    format!(
                        "{} rescue {}",
                        self.operand(node.node(0)?)?,
                        self.operand(resbody.node(2).map(|value| value.as_ref()).unwrap_or(&N {
                            ty: "nil",
                            children: Vec::new(),
                            span: None,
                            syntax: 0,
                        }))?
                    )
                } else {
                    format!("begin; {}; end", self.rescue_body(node)?)
                }
            }
            "ensure" => format!("begin; {}; end", self.ensure_body(node)?),
            "return" | "next" | "break" => {
                if children.is_empty() {
                    node.ty.to_owned()
                } else {
                    format!("{} {}", node.ty, self.arguments(children)?.join(", "))
                }
            }
            "yield" => {
                if children.is_empty() {
                    "yield".into()
                } else {
                    format!("yield({})", self.arguments(children)?.join(", "))
                }
            }
            "super" => format!("super({})", self.arguments(children)?.join(", ")),
            "defined?" => format!("defined?({})", self.expression(node.node(0)?)?),
            "block" | "numblock" | "itblock" => self.block(node)?,
            "lambda" => "lambda".into(),
            "match_pattern_p" => format!(
                "{} in {}",
                self.operand(node.node(0)?)?,
                self.pattern(node.node(1)?)?
            ),
            "match_pattern" => format!(
                "{} => {}",
                self.operand(node.node(0)?)?,
                self.pattern(node.node(1)?)?
            ),
            "match_with_lvasgn" => format!(
                "{} =~ {}",
                self.expression(node.node(0)?)?,
                self.operand(node.node(1)?)?
            ),
            "def" => self.def(node, String::new(), 0)?,
            "defs" => {
                // `def self.m`, `def Const.m`; any other receiver in
                // parentheses.
                let receiver = node.node(0)?;
                let text = self.expression(receiver)?;
                let receiver = match receiver.ty {
                    "self" | "lvar" | "ivar" | "gvar" | "cvar" | "const" => text,
                    _ => format!("({text})"),
                };
                self.def(node, format!("{receiver}."), 1)?
            }
            "alias" => format!(
                "alias {} {}",
                self.expression(node.node(0)?)?,
                self.expression(node.node(1)?)?
            ),
            "undef" => format!("undef {}", self.arguments(children)?.join(", ")),
            _ => self.slot_or_none(node)?,
        })
    }

    fn slot_or_none(&self, node: &N) -> Option<String> {
        if slot_only(node.ty) {
            self.slot(node)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Names, symbols, strings and numbers print as Ruby reads them back.
    #[test]
    fn ruby_literals_print_as_ruby_reads_them() {
        assert!(method_name("caf\u{e9}") && method_name("\u{e9}t\u{e9}?") && method_name("_a!"));
        assert!(!method_name("1a") && !method_name("a-b") && !method_name(""));
        for (name, literal) in [
            ("a", ":a"),
            ("a=", ":a="),
            ("+", ":+"),
            ("!", ":!"),
            ("[]=", ":[]="),
            ("-@", ":-@"),
            ("@@count", ":@@count"),
            ("@count", ":@count"),
            ("$count", ":$count"),
            ("a b", ":\"a b\""),
            ("@", ":\"@\""),
        ] {
            assert_eq!(symbol(name), literal);
        }
        assert_eq!(
            quoted(b"\\\n\t\r\x1b#{#$#@#a\x01\x7f\"\xc3\xa9"),
            "\"\\\\\\n\\t\\r\\e\\#{\\#$\\#@#a\\x01\\x7F\\\"\u{e9}\""
        );
        assert_eq!(escaped(b"it's", b'\''), "it\\'s");
        assert_eq!(escaped(&[b'a', 0xff], b'"'), "\\x61\\xFF");
        for (value, literal) in [
            ("(1/1)", Some("1r")),
            ("(7/2)", Some("3.5r")),
            ("(-7/2)", Some("-3.5r")),
            ("(5/4)", Some("1.25r")),
            ("(1/3)", None),
            ("(x/2)", None),
            ("(1/x)", None),
            ("1", None),
        ] {
            assert_eq!(rational(value).as_deref(), literal, "{value}");
        }
        for (value, literal) in [
            (1.0, "1.0"),
            (0.5, "0.5"),
            (1e20, "1e20"),
            (-2.5e-7, "-2.5e-7"),
        ] {
            assert_eq!(float(f64::to_bits(value)), literal);
        }
    }
}
