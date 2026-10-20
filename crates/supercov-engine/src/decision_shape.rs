//! A decision's boolean shape, read back from its source and its conditions:
//! which vectors short-circuit evaluation can produce. The run that would show
//! a condition deciding the result is one of them; one made by flipping a
//! condition in a run that was seen often is not (`a` true in `a || b` cannot
//! make the decision false).

use crate::coverage_analysis::McdcVector;

/// Decisions wider than this are not enumerated.
const MAX_CONDITIONS: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Shape {
    Condition(usize),
    Not(Box<Shape>),
    And(Box<Shape>, Box<Shape>),
    Or(Box<Shape>, Box<Shape>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Token {
    Condition(usize),
    Not,
    And,
    Or,
    Open,
    Close,
}

/// `rest` after `keyword` standing as a word of its own.
fn word<'s>(rest: &'s str, keyword: &str) -> Option<&'s str> {
    rest.strip_prefix(keyword).filter(|after| {
        after
            .chars()
            .next()
            .is_none_or(|character| !(character.is_alphanumeric() || character == '_'))
    })
}

/// The tokens of `source`: each condition, in order, where its text stands,
/// and the operators between them in any of the languages' spellings.
fn tokens(source: &str, conditions: &[&str]) -> Option<Vec<Token>> {
    let mut tokens = Vec::new();
    let mut rest = source;
    let mut next = 0;
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        // A condition's own text first: it may begin with `!` or `(`.
        if let Some(condition) = conditions.get(next)
            && !condition.is_empty()
            && let Some(after) = rest.strip_prefix(condition)
        {
            tokens.push(Token::Condition(next));
            next += 1;
            rest = after;
            continue;
        }
        let (token, after) = if let Some(after) = rest.strip_prefix("&&") {
            (Token::And, after)
        } else if let Some(after) = rest.strip_prefix("||") {
            (Token::Or, after)
        } else if let Some(after) = word(rest, "and") {
            (Token::And, after)
        } else if let Some(after) = word(rest, "or") {
            (Token::Or, after)
        } else if let Some(after) = word(rest, "not") {
            (Token::Not, after)
        } else if let Some(after) = rest
            .strip_prefix('!')
            .filter(|after| !after.starts_with('='))
        {
            (Token::Not, after)
        } else if let Some(after) = rest.strip_prefix('(') {
            (Token::Open, after)
        } else if let Some(after) = rest.strip_prefix(')') {
            (Token::Close, after)
        } else {
            return None;
        };
        tokens.push(token);
        rest = after;
    }
    (next == conditions.len()).then_some(tokens)
}

/// `or` binds loosest, then `and`, then `not`.
struct Parser {
    tokens: Vec<Token>,
    at: usize,
}

impl Parser {
    fn peek(&self) -> Option<Token> {
        self.tokens.get(self.at).copied()
    }

    fn or(&mut self) -> Option<Shape> {
        let mut left = self.and()?;
        while self.peek() == Some(Token::Or) {
            self.at += 1;
            left = Shape::Or(Box::new(left), Box::new(self.and()?));
        }
        Some(left)
    }

    fn and(&mut self) -> Option<Shape> {
        let mut left = self.not()?;
        while self.peek() == Some(Token::And) {
            self.at += 1;
            left = Shape::And(Box::new(left), Box::new(self.not()?));
        }
        Some(left)
    }

    fn not(&mut self) -> Option<Shape> {
        match self.peek()? {
            Token::Not => {
                self.at += 1;
                Some(Shape::Not(Box::new(self.not()?)))
            }
            Token::Open => {
                self.at += 1;
                let inner = self.or()?;
                (self.peek() == Some(Token::Close)).then(|| self.at += 1)?;
                Some(inner)
            }
            Token::Condition(index) => {
                self.at += 1;
                Some(Shape::Condition(index))
            }
            Token::And | Token::Or | Token::Close => None,
        }
    }
}

fn parse(source: &str, conditions: &[&str]) -> Option<Shape> {
    let mut parser = Parser {
        tokens: tokens(source, conditions)?,
        at: 0,
    };
    let shape = parser.or()?;
    (parser.at == parser.tokens.len()).then_some(shape)
}

/// The outcome, recording each condition evaluated on the way.
fn evaluate(shape: &Shape, values: &[bool], seen: &mut [Option<bool>]) -> bool {
    match shape {
        Shape::Condition(index) => {
            seen[*index] = Some(values[*index]);
            values[*index]
        }
        Shape::Not(inner) => !evaluate(inner, values, seen),
        Shape::And(left, right) => evaluate(left, values, seen) && evaluate(right, values, seen),
        Shape::Or(left, right) => evaluate(left, values, seen) || evaluate(right, values, seen),
    }
}

/// Every vector the decision can produce, or `None` when its source does not
/// read as conditions joined by `&&`, `||`, `!`, `and`, `or`, `not` and
/// parentheses.
pub fn possible_vectors(source: &str, conditions: &[&str]) -> Option<Vec<McdcVector>> {
    if conditions.is_empty() || conditions.len() > MAX_CONDITIONS {
        return None;
    }
    let shape = parse(source, conditions)?;
    let mut vectors = Vec::<McdcVector>::new();
    for assignment in 0..(1_u32 << conditions.len()) {
        let values = (0..conditions.len())
            .map(|index| assignment & (1 << index) != 0)
            .collect::<Vec<_>>();
        let mut seen = vec![None; conditions.len()];
        let outcome = evaluate(&shape, &values, &mut seen);
        let vector = McdcVector {
            values: seen,
            outcome,
        };
        if !vectors.contains(&vector) {
            vectors.push(vector);
        }
    }
    Some(vectors)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(vector: &McdcVector) -> String {
        let mut text = vector
            .values
            .iter()
            .map(|value| match value {
                None => '-',
                Some(false) => 'F',
                Some(true) => 'T',
            })
            .collect::<String>();
        text.push_str(if vector.outcome { " T" } else { " F" });
        text
    }

    #[test]
    fn an_or_chain_produces_only_what_short_circuiting_allows() {
        let vectors = possible_vectors("a || b || c", &["a", "b", "c"]).unwrap();
        let mut texts = vectors.iter().map(text).collect::<Vec<_>>();
        texts.sort();
        assert_eq!(texts, ["FFF F", "FFT T", "FT- T", "T-- T"]);
    }

    #[test]
    fn languages_spell_the_operators_their_own_way() {
        for (source, conditions) in [
            ("signedIn && !expired", vec!["signedIn", "!expired"]),
            ("member and not banned", vec!["member", "banned"]),
            ("(a || b) && !(c)", vec!["a", "b", "c"]),
            (
                "user?.admin || flags[\"beta\"] != null",
                vec!["user?.admin", "flags[\"beta\"] != null"],
            ),
        ] {
            assert!(possible_vectors(source, &conditions).is_some(), "{source}");
        }
        // A source that is not conditions and operators is not guessed at.
        assert!(possible_vectors("a ? b : c", &["a", "b", "c"]).is_none());
    }

    #[test]
    fn a_keyword_inside_a_longer_name_is_not_an_operator() {
        // `android` and `or_else` begin with `and` and `or` but are names.
        assert!(possible_vectors("a && android", &["a", "b"]).is_none());
        assert!(possible_vectors("a && or_else", &["a", "b"]).is_none());
        assert!(possible_vectors("a and b", &["a", "b"]).is_some());
    }

    #[test]
    fn an_empty_condition_text_never_matches_the_source() {
        // An empty condition would match anywhere and loop without consuming
        // the source; it leaves the source unread instead.
        assert!(possible_vectors("a && b", &["a", ""]).is_none());
    }

    #[test]
    fn a_decision_with_no_conditions_or_too_many_is_not_enumerated() {
        assert!(possible_vectors("", &[]).is_none());
        let names = (0..=MAX_CONDITIONS)
            .map(|index| format!("c{index}"))
            .collect::<Vec<_>>();
        let conditions = names.iter().map(String::as_str).collect::<Vec<_>>();
        assert!(possible_vectors(&names.join(" || "), &conditions).is_none());
        assert!(
            possible_vectors(&names[1..].join(" || "), &conditions[1..]).is_some(),
            "sixteen conditions are still enumerated"
        );
    }
}
