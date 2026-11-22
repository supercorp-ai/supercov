//! What mutant reads from a regexp's source with `regexp_parser`.
//!
//! mutant turns `x =~ /\Afoo/` into `x.start_with?("foo")` and
//! `x.match?(/foo\z/)` into `x.end_with?("foo")`: where the pattern parses
//! to exactly an anchor and one literal run. regexp_parser makes one
//! `Literal` of a run of characters that are not metacharacters, escapes or
//! quantified, so that is what is recognized here.
//!
//! mutant also mutates a regexp's body (removing quantifiers, alternatives,
//! swapping character types). Those mutations are not enumerated yet:
//! `mutations` lists none, and the parity goldens count them apart.

/// A pattern that is an anchor and one literal run.
pub(crate) enum Anchored {
    /// `\Afoo`: `start_with?("foo")`.
    Start(String),
    /// `foo\z`: `end_with?("foo")`.
    End(String),
}

fn literal_run(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|character| !"\\^$.|?*+()[]{}".contains(character))
}

pub(crate) fn anchored_literal(body: &str) -> Option<Anchored> {
    if let Some(rest) = body.strip_prefix("\\A")
        && literal_run(rest)
    {
        return Some(Anchored::Start(rest.to_owned()));
    }
    if let Some(rest) = body.strip_suffix("\\z")
        && literal_run(rest)
    {
        return Some(Anchored::End(rest.to_owned()));
    }
    None
}

/// mutant's mutations of a regexp's body: none enumerated yet.
pub(crate) fn mutations(_body: &str) -> Vec<String> {
    Vec::new()
}
