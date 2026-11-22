//! What mutant reads from a regexp's source with `regexp_parser`, and its
//! regexp mutators (`Mutant::Mutator::Regexp`).
//!
//! A pattern is parsed into regexp_parser's tree: a run of plain characters
//! is one `Literal` (its last character apart when a quantifier follows),
//! escapes, anchors and character types are their own expressions, groups and
//! alternations hold theirs, and a set holds its members. mutant mutates that
//! tree and prints each mutation back (`to_str`):
//!
//! - every quantified expression without its quantifier, and `*` as `+`
//!   (`*+` as `++`, `*?` as `+?`);
//! - `\d` and `\D`, `\w` and `\W`, `\s` and `\S`, `\h` and `\H`, `\b` and
//!   `\B`, `\X` and `\R` swapped; `^` as `\A`, `$` and `\Z` as `\z`;
//! - an alternation without each of its alternatives;
//! - a capturing group as `(?:...)`, and a named one also as `(?<_name>...)`;
//! - each of these inside the groups, alternatives and sets that hold them.
//!
//! A pattern this parser does not read (a conditional, a set with `&&`) has
//! no body mutations, and the parity goldens show them as mutant's alone.

/// A pattern that is an anchor and one literal run.
pub(crate) enum Anchored {
    /// `\Afoo`: `start_with?("foo")`.
    Start(String),
    /// `foo\z`: `end_with?("foo")`.
    End(String),
}

pub(crate) fn anchored_literal(body: &str) -> Option<Anchored> {
    let root = parse(body)?;
    match root.children.as_slice() {
        [first, second]
            if first.kind == Kind::Anchor
                && first.text == "\\A"
                && second.kind == Kind::Literal =>
        {
            Some(Anchored::Start(second.text.clone()))
        }
        [first, second]
            if first.kind == Kind::Literal
                && second.kind == Kind::Anchor
                && second.text == "\\z" =>
        {
            Some(Anchored::End(first.text.clone()))
        }
        _ => None,
    }
}

/// mutant's mutations of a regexp's body, printed, in its order.
pub(crate) fn mutations(body: &str) -> Vec<String> {
    let Some(root) = parse(body) else {
        return Vec::new();
    };
    mutate(&root)
        .into_iter()
        .filter(references_resolve)
        .map(|each| each.to_str())
        .collect()
}

/// Whether every back reference and subexpression call names a group the
/// regexp has, as Ruby requires of a regexp literal: a capture made
/// passive or a name changed leaves `\1` or `\k<x>` without one, and mutant,
/// whose mutations Ruby parses back, has none such.
fn references_resolve(root: &Expr) -> bool {
    fn walk<'e>(
        expression: &'e Expr,
        captures: &mut usize,
        names: &mut Vec<&'e str>,
        references: &mut Vec<&'e str>,
    ) {
        match expression.kind {
            Kind::Capture => *captures += 1,
            Kind::Named => names.push(
                expression
                    .text
                    .trim_start_matches("(?<")
                    .trim_end_matches('>'),
            ),
            Kind::Other if expression.text.starts_with('\\') => {
                references.push(&expression.text[1..])
            }
            _ => {}
        }
        for child in &expression.children {
            walk(child, captures, names, references);
        }
    }
    /// Whether `expression` calls the whole regexp (`\g<0>`).
    fn recurses(expression: &Expr) -> bool {
        matches!(expression.text.as_str(), "\\g<0>" | "\\g'0'")
            || expression.children.iter().any(recurses)
    }
    let (mut captures, mut names, mut references) = (0, Vec::new(), Vec::new());
    walk(root, &mut captures, &mut names, &mut references);
    // A call of the whole regexp ends only through an alternative that
    // makes none: Ruby refuses `b\g<0>` as never ending.
    fn ends(expression: &Expr) -> bool {
        (expression.kind == Kind::Alternation
            && expression.children.iter().any(|branch| !recurses(branch)))
            || expression.children.iter().any(ends)
    }
    let ends = ends(root);
    references.into_iter().all(|reference| {
        let number = |text: &str| text.parse::<usize>().ok();
        let (call, target) = match reference.as_bytes().first() {
            Some(b'k' | b'g') if reference.len() > 2 => (true, &reference[2..reference.len() - 1]),
            // `\1`-`\9` refer back; `\0` and longer digits past the groups are
            // octal escapes.
            Some(b'1'..=b'9')
                if reference.len() == 1 || number(reference).is_some_and(|n| n <= captures) =>
            {
                (false, reference)
            }
            _ => return true,
        };
        match number(target) {
            // `\g<0>` calls the whole regexp.
            Some(0) if call => ends,
            // A number names a capture, where the regexp has no named ones.
            Some(group) => names.is_empty() && group <= captures,
            None => names.contains(&target),
        }
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Root,
    /// One branch of an alternation.
    Alternative,
    Alternation,
    /// `(` (capturing), with `text` the opening.
    Capture,
    /// `(?<name>`, with `name`.
    Named,
    /// Any other group: `(?:`, `(?=`, `(?>`, `(?i:`, ...
    Group,
    /// `[...]`, with `negated`.
    Set,
    /// `a-z` in a set: two children.
    Range,
    Literal,
    /// `\d`, `\w`, ...
    CharType,
    /// `^`, `$`, `\A`, `\z`, `\Z`, `\b`, `\B`, `\G`.
    Anchor,
    /// Anything else, kept as written: `.`, escapes, back references,
    /// properties, POSIX classes, `(?i)`, comments.
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Expr {
    kind: Kind,
    /// A leaf's text, or a group's opening.
    text: String,
    children: Vec<Expr>,
    quantifier: Option<String>,
    negated: bool,
}

impl Expr {
    fn leaf(kind: Kind, text: impl Into<String>) -> Self {
        Self {
            kind,
            text: text.into(),
            children: Vec::new(),
            quantifier: None,
            negated: false,
        }
    }

    fn container(kind: Kind, text: impl Into<String>, children: Vec<Expr>) -> Self {
        Self {
            kind,
            text: text.into(),
            children,
            quantifier: None,
            negated: false,
        }
    }

    fn to_str(&self) -> String {
        let inner: String = match self.kind {
            Kind::Root | Kind::Alternative => self.children.iter().map(Expr::to_str).collect(),
            Kind::Alternation => self
                .children
                .iter()
                .map(Expr::to_str)
                .collect::<Vec<_>>()
                .join("|"),
            Kind::Capture | Kind::Named | Kind::Group => format!(
                "{}{})",
                self.text,
                self.children.iter().map(Expr::to_str).collect::<String>()
            ),
            Kind::Set => format!(
                "[{}{}]",
                if self.negated { "^" } else { "" },
                self.children.iter().map(Expr::to_str).collect::<String>()
            ),
            Kind::Range => self
                .children
                .iter()
                .map(Expr::to_str)
                .collect::<Vec<_>>()
                .join("-"),
            Kind::Literal | Kind::CharType | Kind::Anchor | Kind::Other => self.text.clone(),
        };
        match &self.quantifier {
            Some(quantifier) => format!("{inner}{quantifier}"),
            None => inner,
        }
    }
}

struct Parser<'a> {
    chars: Vec<char>,
    at: usize,
    _source: &'a str,
}

/// regexp_parser's tree for `body`, or none where it holds syntax not read
/// here.
fn parse(body: &str) -> Option<Expr> {
    let mut parser = Parser {
        chars: body.chars().collect(),
        at: 0,
        _source: body,
    };
    let children = parser.sequence()?;
    if parser.at != parser.chars.len() {
        return None;
    }
    Some(Expr::container(Kind::Root, "", children))
}

const CHAR_TYPES: &[char] = &['d', 'D', 'w', 'W', 's', 'S', 'h', 'H', 'R', 'X'];

impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.at + offset).copied()
    }

    /// A group's or the root's contents, with `|` making an alternation.
    fn sequence(&mut self) -> Option<Vec<Expr>> {
        let mut alternatives = vec![Vec::new()];
        while let Some(character) = self.peek() {
            match character {
                ')' => break,
                '|' => {
                    self.at += 1;
                    alternatives.push(Vec::new());
                }
                _ => {
                    let current = alternatives.last_mut().expect("an alternative");
                    self.atom(current)?;
                }
            }
        }
        if alternatives.len() == 1 {
            return alternatives.pop();
        }
        let alternatives = alternatives
            .into_iter()
            .map(|children| Expr::container(Kind::Alternative, "", children))
            .collect();
        Some(vec![Expr::container(Kind::Alternation, "", alternatives)])
    }

    /// A quantifier at the cursor: `*`, `+`, `?`, `{n}`, `{n,}`, `{,m}`,
    /// `{n,m}`, each maybe followed by `?` or `+`, and intervals after it
    /// (`a{2}{3}` is `(?:a{2}){3}`).
    fn quantifier_length(&self) -> usize {
        let base = match self.peek() {
            Some('*' | '+' | '?') => 1,
            _ => self.interval_length(self.at),
        };
        if base == 0 {
            return 0;
        }
        let mut length = match self.peek_at(base) {
            Some('?' | '+') => base + 1,
            _ => base,
        };
        loop {
            let interval = self.interval_length(self.at + length);
            if interval == 0 {
                return length;
            }
            length += interval;
        }
    }

    /// An interval's length at `start`: `{n}`, `{n,}`, `{,m}`, `{n,m}`; 0
    /// where none is.
    fn interval_length(&self, start: usize) -> usize {
        if self.chars.get(start) != Some(&'{') {
            return 0;
        }
        let mut index = start + 1;
        let mut digits = 0;
        let mut comma = false;
        while let Some(character) = self.chars.get(index) {
            match character {
                '0'..='9' => digits += 1,
                ',' if !comma => comma = true,
                '}' => break,
                _ => return 0,
            }
            index += 1;
        }
        if self.chars.get(index) != Some(&'}') || digits == 0 {
            return 0;
        }
        index + 1 - start
    }

    fn take_quantifier(&mut self) -> Option<String> {
        let length = self.quantifier_length();
        if length == 0 {
            return None;
        }
        let text: String = self.chars[self.at..self.at + length].iter().collect();
        self.at += length;
        Some(text)
    }

    fn atom(&mut self, into: &mut Vec<Expr>) -> Option<()> {
        let character = self.peek()?;
        let mut expression = match character {
            '(' => self.group()?,
            '[' => self.set()?,
            '\\' => self.escape(false)?,
            '^' | '$' => {
                self.at += 1;
                Expr::leaf(Kind::Anchor, character.to_string())
            }
            '.' => {
                self.at += 1;
                Expr::leaf(Kind::Other, ".")
            }
            '*' | '+' | '?' => return None,
            _ => {
                // A run of plain characters, its last one apart where a
                // quantifier follows it.
                let start = self.at;
                while let Some(next) = self.peek() {
                    if "()[]\\^$.|*+?".contains(next)
                        || (next == '{' && self.quantifier_length() > 0)
                    {
                        break;
                    }
                    self.at += 1;
                }
                if self.at == start {
                    // A `{` that starts no quantifier.
                    self.at += 1;
                }
                if self.quantifier_length() > 0 && self.at - start > 1 {
                    let run: String = self.chars[start..self.at - 1].iter().collect();
                    into.push(Expr::leaf(Kind::Literal, run));
                    let last: String = self.chars[self.at - 1..self.at].iter().collect();
                    Expr::leaf(Kind::Literal, last)
                } else {
                    Expr::leaf(
                        Kind::Literal,
                        self.chars[start..self.at].iter().collect::<String>(),
                    )
                }
            }
        };
        expression.quantifier = self.take_quantifier();
        into.push(expression);
        Some(())
    }

    fn escape(&mut self, in_set: bool) -> Option<Expr> {
        let next = self.peek_at(1)?;
        let start = self.at;
        self.at += 2;
        if CHAR_TYPES.contains(&next) {
            return Some(Expr::leaf(Kind::CharType, format!("\\{next}")));
        }
        if !in_set && matches!(next, 'A' | 'z' | 'Z' | 'b' | 'B' | 'G') {
            return Some(Expr::leaf(Kind::Anchor, format!("\\{next}")));
        }
        match next {
            'p' | 'P' | 'k' | 'g' | 'u' | 'x'
                if self.peek() == Some('{')
                    || self.peek() == Some('<')
                    || self.peek() == Some('\'') =>
            {
                let close = match self.peek() {
                    Some('{') => '}',
                    Some('<') => '>',
                    _ => '\'',
                };
                // Past the opening, which for `'` is the closing too.
                self.at += 1;
                while let Some(character) = self.peek() {
                    self.at += 1;
                    if character == close {
                        break;
                    }
                }
            }
            'x' => {
                for _ in 0..2 {
                    if self
                        .peek()
                        .is_some_and(|character| character.is_ascii_hexdigit())
                    {
                        self.at += 1;
                    }
                }
            }
            'c' => self.at += 1,
            'C' | 'M' => self.at += 2,
            '0'..='9' => {
                while self
                    .peek()
                    .is_some_and(|character| character.is_ascii_digit())
                {
                    self.at += 1;
                }
            }
            _ => {}
        }
        Some(Expr::leaf(
            Kind::Other,
            self.chars[start..self.at].iter().collect::<String>(),
        ))
    }

    fn group(&mut self) -> Option<Expr> {
        let start = self.at;
        self.at += 1;
        let (kind, opening) = if self.peek() == Some('?') {
            match self.peek_at(1)? {
                '#' => {
                    // A comment: kept whole.
                    while let Some(character) = self.peek() {
                        self.at += 1;
                        if character == ')' {
                            break;
                        }
                    }
                    return Some(Expr::leaf(
                        Kind::Other,
                        self.chars[start..self.at].iter().collect::<String>(),
                    ));
                }
                '<' if !matches!(self.peek_at(2), Some('=' | '!')) => {
                    let name_start = self.at + 2;
                    let mut index = name_start;
                    while self
                        .chars
                        .get(index)
                        .is_some_and(|character| *character != '>')
                    {
                        index += 1;
                    }
                    self.at = index + 1;
                    (
                        Kind::Named,
                        self.chars[start..self.at].iter().collect::<String>(),
                    )
                }
                '(' => return None,
                _ => {
                    // `(?:`, `(?=`, `(?!`, `(?<=`, `(?<!`, `(?>`, `(?~`,
                    // `(?imx-imx:` and the option switch `(?imx)`.
                    let mut index = self.at + 1;
                    while self.chars.get(index).is_some_and(|character| {
                        character.is_ascii_alphabetic() || *character == '-'
                    }) {
                        index += 1;
                    }
                    match self.chars.get(index) {
                        Some(')') => {
                            self.at = index + 1;
                            return Some(Expr::leaf(
                                Kind::Other,
                                self.chars[start..self.at].iter().collect::<String>(),
                            ));
                        }
                        Some(':') if index > self.at + 1 => self.at = index + 1,
                        _ => {
                            self.at += 2;
                            if matches!(self.chars.get(self.at - 1), Some('<')) {
                                self.at += 1;
                            }
                        }
                    }
                    (
                        Kind::Group,
                        self.chars[start..self.at].iter().collect::<String>(),
                    )
                }
            }
        } else {
            (Kind::Capture, "(".to_owned())
        };
        let children = self.sequence()?;
        if self.peek() != Some(')') {
            return None;
        }
        self.at += 1;
        Some(Expr::container(kind, opening, children))
    }

    fn set(&mut self) -> Option<Expr> {
        self.at += 1;
        let negated = self.peek() == Some('^');
        if negated {
            self.at += 1;
        }
        let mut members = Vec::new();
        let mut first = true;
        loop {
            let character = self.peek()?;
            if character == ']' && !first {
                self.at += 1;
                break;
            }
            first = false;
            if character == '&' && self.peek_at(1) == Some('&') {
                return None;
            }
            let member = match character {
                '[' if self.peek_at(1) == Some(':') => {
                    let start = self.at;
                    while let Some(next) = self.peek() {
                        self.at += 1;
                        if next == ']' {
                            break;
                        }
                    }
                    Expr::leaf(
                        Kind::Other,
                        self.chars[start..self.at].iter().collect::<String>(),
                    )
                }
                '[' => self.set()?,
                '\\' => self.escape(true)?,
                _ => {
                    self.at += 1;
                    Expr::leaf(Kind::Literal, character.to_string())
                }
            };
            // `a-z`: a range, unless the `-` ends the set.
            if self.peek() == Some('-')
                && self.peek_at(1).is_some_and(|next| next != ']')
                && matches!(member.kind, Kind::Literal | Kind::Other)
            {
                self.at += 1;
                let upper = match self.peek()? {
                    '\\' => self.escape(true)?,
                    other => {
                        self.at += 1;
                        Expr::leaf(Kind::Literal, other.to_string())
                    }
                };
                members.push(Expr::container(Kind::Range, "", vec![member, upper]));
                continue;
            }
            members.push(member);
        }
        let mut set = Expr::container(Kind::Set, "[", members);
        set.negated = negated;
        Some(set)
    }
}

/// The swap a character type or anchor takes, keeping its quantifier.
fn replacement(expression: &Expr) -> Option<(Kind, &'static str)> {
    if !matches!(expression.kind, Kind::CharType | Kind::Anchor) {
        return None;
    }
    Some(match expression.text.as_str() {
        "\\b" => (Kind::Anchor, "\\B"),
        "\\B" => (Kind::Anchor, "\\b"),
        "\\d" => (Kind::CharType, "\\D"),
        "\\D" => (Kind::CharType, "\\d"),
        "\\X" => (Kind::CharType, "\\R"),
        "\\R" => (Kind::CharType, "\\X"),
        "\\h" => (Kind::CharType, "\\H"),
        "\\H" => (Kind::CharType, "\\h"),
        "\\s" => (Kind::CharType, "\\S"),
        "\\S" => (Kind::CharType, "\\s"),
        "\\w" => (Kind::CharType, "\\W"),
        "\\W" => (Kind::CharType, "\\w"),
        "^" => (Kind::Anchor, "\\A"),
        "$" | "\\Z" => (Kind::Anchor, "\\z"),
        _ => return None,
    })
}

/// The quantifiers one written quantifier is, as Ruby reads it: `*?`, `+?`,
/// `{n,m}?` (reluctant) and `*+`, `++` (possessive) are one; an interval
/// followed by `+`, an exact one by `?` (`a{2}?` is `(?:a{2})?`) and
/// intervals one after another (`a{2}{3}`) are several.
fn quantifiers(quantifier: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut rest = quantifier;
    while !rest.is_empty() {
        let length = match rest.find('}') {
            Some(close) if rest.starts_with('{') => {
                let reluctant = rest[..close].contains(',') && rest[close + 1..].starts_with('?');
                close + 1 + usize::from(reluctant)
            }
            _ if parts.is_empty() && rest[1..].starts_with(['?', '+']) => 2,
            _ => 1,
        };
        parts.push(&rest[..length]);
        rest = &rest[length..];
    }
    parts
}

/// An expression's mutations: its class's mutators, in the order mutant
/// registers them, then the quantifier's.
fn mutate(expression: &Expr) -> Vec<Expr> {
    let mut out: Vec<Expr> = Vec::new();
    let emit = |each: Expr, out: &mut Vec<Expr>| {
        if each != *expression && !out.contains(&each) {
            out.push(each);
        }
    };
    match expression.kind {
        Kind::Alternation => {
            for index in 0..expression.children.len() {
                let mut copy = expression.clone();
                copy.children.remove(index);
                emit(copy, &mut out);
            }
        }
        Kind::Capture if !expression.children.is_empty() => {
            let mut copy = expression.clone();
            copy.kind = Kind::Group;
            copy.text = "(?:".into();
            copy.quantifier = None;
            emit(copy, &mut out);
        }
        Kind::Named => {
            let name = expression
                .text
                .trim_start_matches("(?<")
                .trim_end_matches('>');
            if !name.starts_with('_') && !expression.children.is_empty() {
                let mut passive = expression.clone();
                passive.kind = Kind::Group;
                passive.text = "(?:".into();
                passive.quantifier = None;
                emit(passive, &mut out);
                let mut renamed = expression.clone();
                renamed.text = format!("(?<_{name}>");
                renamed.quantifier = None;
                emit(renamed, &mut out);
            }
        }
        _ => {}
    }
    if let Some((kind, text)) = replacement(expression) {
        let mut swapped = Expr::leaf(kind, text);
        swapped.quantifier = expression.quantifier.clone();
        emit(swapped, &mut out);
    }
    // Recurse: each child's mutations, in a copy of this expression.
    if matches!(
        expression.kind,
        Kind::Root
            | Kind::Alternative
            | Kind::Alternation
            | Kind::Capture
            | Kind::Named
            | Kind::Group
            | Kind::Set
            | Kind::Range
    ) {
        for (index, child) in expression.children.iter().enumerate() {
            for mutated in mutate(child) {
                let mut copy = expression.clone();
                copy.children[index] = mutated;
                emit(copy, &mut out);
            }
        }
    }
    // Quantifier: removed, and `*` as `+`. Where it is several quantifiers
    // as Ruby reads it, each is removed apart.
    if let Some(quantifier) = &expression.quantifier {
        let parts = quantifiers(quantifier);
        if parts.len() == 1 {
            let mut removed = expression.clone();
            removed.quantifier = None;
            emit(removed, &mut out);
        }
        for index in 0..parts.len() {
            if parts.len() > 1 {
                let mut kept = parts.clone();
                kept.remove(index);
                let mut removed = expression.clone();
                removed.quantifier = Some(kept.concat());
                emit(removed, &mut out);
            }
        }
        let replaced = match quantifier.as_str() {
            "*" => Some("+"),
            "*+" => Some("++"),
            "*?" => Some("+?"),
            _ => None,
        };
        if let Some(replaced) = replaced {
            let mut copy = expression.clone();
            copy.quantifier = Some(replaced.to_owned());
            emit(copy, &mut out);
        }
    }
    out
}
