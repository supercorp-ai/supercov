//! The Regex mutants StrykerJS 10.1.1 makes: weapon-regex 2.0.6's level-1
//! mutants of a regular expression's pattern, in its order, with its text.
//!
//! weapon-regex parses the pattern (its JavaScript flavor) into a tree and
//! walks it in pre-order. At each node it tries its level-1 mutators in their
//! order -- removing a `^` child, removing a `$` child, negating a character
//! class, negating a predefined class (`\d`), negating a Unicode property
//! class (`\p{..}`), removing a quantifier, negating a lookaround -- and each
//! mutant is the whole pattern built again with that one node changed. A
//! pattern it cannot parse has no mutants. `tests/fixtures/regex-mutants.json`
//! holds weapon-regex's own answers for the patterns of a public corpus, and
//! the tests here compare the two pattern by pattern.

/// One node of a parsed pattern: its span in the pattern's characters and
/// what it is.
#[derive(Debug)]
struct Node {
    start: usize,
    end: usize,
    kind: Kind,
}

#[derive(Debug)]
enum Kind {
    /// Two or more items one after another.
    Concat(Vec<Node>),
    /// Alternatives, any of them possibly empty (`None`).
    Or(Vec<Option<Node>>),
    /// `^`.
    Bol,
    /// `$`.
    Eol,
    /// `[...]` or `[^...]`: whether negated, and its items.
    Class(bool, Vec<Node>),
    /// `\d` and the like: the letter.
    Predefined(char),
    /// `\p{..}` or `\P{..}` under the `u` or `v` flag.
    Unicode,
    /// An atom and its quantifier: the quantifier's start.
    Quantified(Box<Node>, usize),
    /// A group: the length of its opening (`(`, `(?:`, `(?<name>`), its
    /// content.
    Group(usize, Box<Node>),
    /// A lookaround: the position of its `=` or `!`, its content.
    Lookaround(usize, Box<Node>),
    /// Anything else (a character, an escape, `.`, a boundary).
    Other,
}

struct Parser<'p> {
    pattern: &'p [char],
    at: usize,
    unicode: bool,
}

type Parsed<T> = Result<T, ()>;

impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.pattern.get(self.at).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.pattern.get(self.at + offset).copied()
    }

    /// Alternatives up to a `)` or the end.
    fn disjunction(&mut self) -> Parsed<Node> {
        let start = self.at;
        let mut alternatives = vec![self.alternative()?];
        while self.peek() == Some('|') {
            self.at += 1;
            alternatives.push(self.alternative()?);
        }
        if alternatives.len() == 1 {
            return alternatives.pop().flatten().ok_or(());
        }
        Ok(Node {
            start,
            end: self.at,
            kind: Kind::Or(alternatives),
        })
    }

    fn alternative(&mut self) -> Parsed<Option<Node>> {
        let start = self.at;
        let mut items = Vec::new();
        while let Some(character) = self.peek() {
            if character == '|' || character == ')' {
                break;
            }
            let atom = self.atom()?;
            items.push(self.quantified(atom)?);
        }
        Ok(match items.len() {
            0 => None,
            1 => items.pop(),
            _ => Some(Node {
                start,
                end: self.at,
                kind: Kind::Concat(items),
            }),
        })
    }

    /// The quantifier at the cursor: its length, or none.
    fn quantifier(&self) -> Parsed<Option<usize>> {
        let length = match self.peek() {
            Some('*' | '+' | '?') => 1,
            Some('{') => {
                let mut index = self.at + 1;
                let digits = |index: &mut usize| {
                    let first = *index;
                    while self.pattern.get(*index).is_some_and(char::is_ascii_digit) {
                        *index += 1;
                    }
                    (*index > first).then(|| {
                        self.pattern[first..*index]
                            .iter()
                            .collect::<String>()
                            .parse::<u64>()
                            .unwrap_or(u64::MAX)
                    })
                };
                let Some(low) = digits(&mut index) else {
                    return Ok(None);
                };
                let mut high = Some(low);
                if self.pattern.get(index) == Some(&',') {
                    index += 1;
                    high = digits(&mut index);
                }
                if self.pattern.get(index) != Some(&'}') {
                    return Ok(None);
                }
                if high.is_some_and(|high| high < low) {
                    return Err(());
                }
                index + 1 - self.at
            }
            _ => return Ok(None),
        };
        let lazy = usize::from(self.pattern.get(self.at + length) == Some(&'?'));
        Ok(Some(length + lazy))
    }

    fn quantified(&mut self, atom: Node) -> Parsed<Node> {
        let Some(length) = self.quantifier()? else {
            return Ok(atom);
        };
        let quantifier = self.at;
        self.at += length;
        // A quantifier on a quantifier is no pattern.
        if self.quantifier()?.is_some() {
            return Err(());
        }
        Ok(Node {
            start: atom.start,
            end: self.at,
            kind: Kind::Quantified(Box::new(atom), quantifier),
        })
    }

    fn atom(&mut self) -> Parsed<Node> {
        let start = self.at;
        let character = self.peek().ok_or(())?;
        let kind = match character {
            '^' => {
                self.at += 1;
                Kind::Bol
            }
            '$' => {
                self.at += 1;
                Kind::Eol
            }
            '(' => return self.group(),
            '[' => return self.class(),
            '\\' => self.escape(false)?,
            '*' | '+' | '?' => return Err(()),
            '{' if self.quantifier()?.is_some() => return Err(()),
            _ => {
                self.at += 1;
                Kind::Other
            }
        };
        Ok(Node {
            start,
            end: self.at,
            kind,
        })
    }

    fn group(&mut self) -> Parsed<Node> {
        let start = self.at;
        self.at += 1;
        let mut lookaround = None;
        if self.peek() == Some('?') {
            match (self.peek_at(1), self.peek_at(2)) {
                (Some(':'), _) => self.at += 2,
                (Some('=' | '!'), _) => {
                    lookaround = Some(self.at + 1);
                    self.at += 2;
                }
                (Some('<'), Some('=' | '!')) => {
                    lookaround = Some(self.at + 2);
                    self.at += 3;
                }
                (Some('<'), Some(_)) => {
                    self.at += 2;
                    while self.peek().is_some_and(|c| c != '>') {
                        self.at += 1;
                    }
                    if self.peek() != Some('>') {
                        return Err(());
                    }
                    self.at += 1;
                }
                _ => return Err(()),
            }
        }
        let opening = self.at - start;
        // An empty group is no pattern weapon-regex reads.
        if matches!(self.peek(), Some(')') | None) {
            return Err(());
        }
        let content = self.disjunction()?;
        if self.peek() != Some(')') {
            return Err(());
        }
        self.at += 1;
        Ok(Node {
            start,
            end: self.at,
            kind: match lookaround {
                Some(at) => Kind::Lookaround(at, Box::new(content)),
                None => Kind::Group(opening, Box::new(content)),
            },
        })
    }

    fn class(&mut self) -> Parsed<Node> {
        let start = self.at;
        self.at += 1;
        let negated = self.peek() == Some('^');
        if negated {
            self.at += 1;
        }
        let mut items = Vec::new();
        loop {
            match self.peek() {
                None => return Err(()),
                Some(']') => break,
                Some('\\') => {
                    let item = self.at;
                    let kind = self.escape(true)?;
                    items.push(Node {
                        start: item,
                        end: self.at,
                        kind,
                    });
                }
                Some(_) => {
                    items.push(Node {
                        start: self.at,
                        end: self.at + 1,
                        kind: Kind::Other,
                    });
                    self.at += 1;
                }
            }
        }
        self.at += 1;
        Ok(Node {
            start,
            end: self.at,
            kind: Kind::Class(negated, items),
        })
    }

    /// An escape at the cursor, in or out of a class.
    fn escape(&mut self, _in_class: bool) -> Parsed<Kind> {
        let letter = self.peek_at(1).ok_or(())?;
        match letter {
            // `\v` too, read as a vertical whitespace class (its `\V` is
            // not one).
            'd' | 'D' | 's' | 'S' | 'w' | 'W' | 'v' => {
                self.at += 2;
                Ok(Kind::Predefined(letter))
            }
            'p' | 'P' if self.unicode => {
                if self.peek_at(2) != Some('{') {
                    return Err(());
                }
                let mut index = self.at + 3;
                while self.pattern.get(index).is_some_and(|c| *c != '}') {
                    index += 1;
                }
                if self.pattern.get(index) != Some(&'}') {
                    return Err(());
                }
                self.at = index + 1;
                Ok(Kind::Unicode)
            }
            'u' if self.unicode && self.peek_at(2) == Some('{') => {
                let mut index = self.at + 3;
                while self.pattern.get(index).is_some_and(|c| *c != '}') {
                    index += 1;
                }
                self.at = index.min(self.pattern.len().saturating_sub(1)) + 1;
                Ok(Kind::Other)
            }
            'k' if self.peek_at(2) == Some('<') => {
                let mut index = self.at + 3;
                while self.pattern.get(index).is_some_and(|c| *c != '>') {
                    index += 1;
                }
                self.at = (index + 1).min(self.pattern.len());
                Ok(Kind::Other)
            }
            _ => {
                self.at += 2;
                Ok(Kind::Other)
            }
        }
    }
}

/// The pattern built again from `node`, with `edit` applied: what weapon-regex
/// builds from its tree with one node changed. Everything not on the way to
/// the changed node is its own text.
struct Edit {
    /// The node changed, by its span.
    target: (usize, usize),
    /// Its new text; `None` removes it from its parent.
    replacement: Option<String>,
}

fn text(pattern: &[char], start: usize, end: usize) -> String {
    pattern[start..end].iter().collect()
}

/// `node` built, or `None` where `edit` removes it.
fn build(pattern: &[char], node: &Node, edit: &Edit) -> Option<String> {
    if (node.start, node.end) == edit.target {
        return edit.replacement.clone();
    }
    if !(node.start <= edit.target.0 && edit.target.1 <= node.end) {
        return Some(text(pattern, node.start, node.end));
    }
    Some(match &node.kind {
        Kind::Concat(items) => items
            .iter()
            .filter_map(|item| build(pattern, item, edit))
            .collect(),
        Kind::Or(alternatives) => alternatives
            .iter()
            .filter_map(|alternative| match alternative {
                None => Some(String::new()),
                Some(alternative) => build(pattern, alternative, edit),
            })
            .collect::<Vec<_>>()
            .join("|"),
        Kind::Quantified(atom, quantifier) => format!(
            "{}{}",
            build(pattern, atom, edit).unwrap_or_default(),
            text(pattern, *quantifier, node.end)
        ),
        Kind::Group(opening, content) => format!(
            "{}{})",
            text(pattern, node.start, node.start + opening),
            build(pattern, content, edit).unwrap_or_default()
        ),
        Kind::Lookaround(at, content) => format!(
            "{}{})",
            text(pattern, node.start, at + 1),
            build(pattern, content, edit).unwrap_or_default()
        ),
        Kind::Class(negated, items) => {
            let opening = if *negated { 2 } else { 1 };
            let mut built = text(pattern, node.start, node.start + opening);
            for item in items {
                built.push_str(&build(pattern, item, edit).unwrap_or_default());
            }
            built.push(']');
            built
        }
        Kind::Bol | Kind::Eol | Kind::Predefined(_) | Kind::Unicode | Kind::Other => {
            text(pattern, node.start, node.end)
        }
    })
}

/// The direct children of a node, as weapon-regex's removals see them.
fn children(node: &Node) -> Vec<&Node> {
    match &node.kind {
        Kind::Concat(items) | Kind::Class(_, items) => items.iter().collect(),
        Kind::Or(alternatives) => alternatives.iter().flatten().collect(),
        Kind::Quantified(atom, _) => vec![atom],
        Kind::Group(_, content) | Kind::Lookaround(_, content) => vec![content],
        _ => Vec::new(),
    }
}

/// Each node's level-1 edits, in pre-order and weapon-regex's mutator order.
fn edits(pattern: &[char], node: &Node, found: &mut Vec<Edit>) {
    let removal = |kind: fn(&Kind) -> bool, found: &mut Vec<Edit>| {
        for child in children(node) {
            if !matches!(node.kind, Kind::Class(..)) && kind(&child.kind) {
                found.push(Edit {
                    target: (child.start, child.end),
                    replacement: None,
                });
            }
        }
    };
    removal(|kind| matches!(kind, Kind::Bol), found);
    removal(|kind| matches!(kind, Kind::Eol), found);
    let target = (node.start, node.end);
    match &node.kind {
        Kind::Class(negated, _) => {
            let inner = text(pattern, node.start + if *negated { 2 } else { 1 }, node.end);
            found.push(Edit {
                target,
                replacement: Some(if *negated {
                    format!("[{inner}")
                } else {
                    format!("[^{inner}")
                }),
            });
        }
        Kind::Predefined(letter) => {
            let swapped = if letter.is_ascii_lowercase() {
                letter.to_ascii_uppercase()
            } else {
                letter.to_ascii_lowercase()
            };
            found.push(Edit {
                target,
                replacement: Some(format!("\\{swapped}")),
            });
        }
        Kind::Unicode => {
            let original = text(pattern, node.start, node.end);
            let swapped = if original.starts_with("\\p") {
                original.replacen("\\p", "\\P", 1)
            } else {
                original.replacen("\\P", "\\p", 1)
            };
            found.push(Edit {
                target,
                replacement: Some(swapped),
            });
        }
        Kind::Quantified(atom, _) => found.push(Edit {
            target,
            replacement: Some(text(pattern, atom.start, atom.end)),
        }),
        Kind::Lookaround(at, _) => {
            let mut swapped = text(pattern, node.start, node.end);
            let offset = at - node.start;
            let flipped = if pattern[*at] == '=' { "!" } else { "=" };
            let byte = swapped
                .char_indices()
                .nth(offset)
                .map_or(swapped.len(), |(byte, _)| byte);
            swapped.replace_range(byte..byte + 1, flipped);
            found.push(Edit {
                target,
                replacement: Some(swapped),
            });
        }
        _ => {}
    }
    for child in children(node) {
        edits(pattern, child, found);
    }
}

/// weapon-regex's level-1 mutants of `pattern` under `flags`, as StrykerJS
/// asks for them: each a whole pattern. None where it cannot parse it, and
/// none that are empty.
pub(crate) fn mutants(pattern: &str, flags: &str) -> Vec<String> {
    let characters = pattern.chars().collect::<Vec<_>>();
    if characters.is_empty() {
        return Vec::new();
    }
    let mut parser = Parser {
        pattern: &characters,
        at: 0,
        unicode: flags.contains('u') || flags.contains('v'),
    };
    let Ok(root) = parser.disjunction() else {
        return Vec::new();
    };
    if parser.at != characters.len() {
        return Vec::new();
    }
    let mut found = Vec::new();
    edits(&characters, &root, &mut found);
    found
        .iter()
        .filter_map(|edit| build(&characters, &root, edit))
        .filter(|built| !built.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// weapon-regex's own answers for a corpus of patterns (each npm package
    /// the repository installs, regex literals and `new RegExp` strings), and
    /// a few made to reach its edges.
    #[test]
    fn mutants_are_weapon_regex_s() {
        let corpus: Vec<serde_json::Value> =
            serde_json::from_str(include_str!("../tests/fixtures/regex-mutants.json")).unwrap();
        let mut differences = Vec::new();
        for entry in &corpus {
            let pattern = entry["pattern"].as_str().unwrap();
            let flags = entry["flags"].as_str().unwrap_or("");
            let expected = entry["mutants"]
                .as_array()
                .map(|mutants| {
                    mutants
                        .iter()
                        .map(|mutant| mutant.as_str().unwrap().to_owned())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let actual = mutants(pattern, flags);
            if actual != expected {
                differences.push(format!("{pattern:?} {flags:?}: {actual:?} != {expected:?}"));
            }
        }
        assert!(
            differences.is_empty(),
            "{} of {}:\n{}",
            differences.len(),
            corpus.len(),
            differences[..differences.len().min(40)].join("\n")
        );
    }
}
