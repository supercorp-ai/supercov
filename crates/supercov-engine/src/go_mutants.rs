//! Go mutants: the ones Gremlins (`go-gremlins/gremlins`, pinned at 0.6.0)
//! would make, listed from the program as written.
//!
//! Gremlins mutates one token at a time: the operator of a binary or unary
//! expression, an assignment's operator, `++` and `--`, `break` and
//! `continue`. Its table (`internal/engine/mappings.go`) says which mutators
//! apply to each token and what each makes of it, and it walks the syntax
//! tree in order (`ast.Inspect`), trying a token's mutators in the table's
//! order. This module keeps that table and that order over tree-sitter's
//! tree, so the list compares with Gremlins' own: a mutant's id carries the
//! token's position as Gremlins reports it (1-based line, 1-based byte
//! column) and Gremlins' mutator name, and `scripts/gremlins-parity.mjs`
//! writes the goldens the tests below compare against.
//!
//! Gremlins also has `INVERT_BITWISE` and `INVERT_BWASSIGN`, off by
//! default there; they are off here too (`MUTATORS`). Of the rest, all are
//! on, the ones Gremlins leaves off by default among them.

use tree_sitter::Node;

use crate::go_instrumenter::{GoInstrumenterError, parse, stable_obligation_id};
use crate::js_mutant_links::{CandidateMutant, MutantOperator};

/// The mutant set every Go report names: Gremlins' version and the
/// mutators listed.
pub const MUTANT_SET: &str = "gremlins@0.6.0";

/// The mutators listed, by Gremlins' names. Gremlins' own flags turn on the
/// same set: `--invert-assignments --invert-logical --invert-loopctrl
/// --remove-self-assignments` beside its defaults.
pub const MUTATORS: &[&str] = &[
    ARITHMETIC_BASE,
    CONDITIONALS_BOUNDARY,
    CONDITIONALS_NEGATION,
    INCREMENT_DECREMENT,
    INVERT_ASSIGNMENTS,
    INVERT_LOGICAL,
    INVERT_LOOPCTRL,
    INVERT_NEGATIVES,
    REMOVE_SELF_ASSIGNMENTS,
];

pub const ARITHMETIC_BASE: &str = "ARITHMETIC_BASE";
pub const CONDITIONALS_BOUNDARY: &str = "CONDITIONALS_BOUNDARY";
pub const CONDITIONALS_NEGATION: &str = "CONDITIONALS_NEGATION";
pub const INCREMENT_DECREMENT: &str = "INCREMENT_DECREMENT";
pub const INVERT_ASSIGNMENTS: &str = "INVERT_ASSIGNMENTS";
pub const INVERT_LOGICAL: &str = "INVERT_LOGICAL";
pub const INVERT_LOOPCTRL: &str = "INVERT_LOOPCTRL";
pub const INVERT_NEGATIVES: &str = "INVERT_NEGATIVES";
pub const REMOVE_SELF_ASSIGNMENTS: &str = "REMOVE_SELF_ASSIGNMENTS";

/// Whether `mutator` is one of Gremlins' (and so a Go mutant's).
pub fn is_go_mutator(mutator: &str) -> bool {
    MUTATORS.contains(&mutator)
}

/// Each mutator Gremlins applies to `token`, in its table's order, with
/// what the token becomes; only the listed mutators.
///
/// `INVERT_ASSIGNMENTS` makes `%=` of `%=`: Gremlins' table maps it to
/// itself. The mutant is the program as written, and is listed as Gremlins
/// lists it; it is equivalent by construction (`identical-token`).
fn mutations(token: &str) -> &'static [(&'static str, &'static str)] {
    match token {
        "+" => &[(ARITHMETIC_BASE, "-")],
        "+=" => &[(INVERT_ASSIGNMENTS, "-="), (REMOVE_SELF_ASSIGNMENTS, "=")],
        "&=" | "&^=" | "|=" | "<<=" | ">>=" | "^=" => &[(REMOVE_SELF_ASSIGNMENTS, "=")],
        "break" => &[(INVERT_LOOPCTRL, "continue")],
        "continue" => &[(INVERT_LOOPCTRL, "break")],
        "--" => &[(INCREMENT_DECREMENT, "++")],
        "++" => &[(INCREMENT_DECREMENT, "--")],
        "==" => &[(CONDITIONALS_NEGATION, "!=")],
        "!=" => &[(CONDITIONALS_NEGATION, "==")],
        ">=" => &[(CONDITIONALS_BOUNDARY, ">"), (CONDITIONALS_NEGATION, "<")],
        ">" => &[(CONDITIONALS_BOUNDARY, ">="), (CONDITIONALS_NEGATION, "<=")],
        "<=" => &[(CONDITIONALS_BOUNDARY, "<"), (CONDITIONALS_NEGATION, ">")],
        "<" => &[(CONDITIONALS_BOUNDARY, "<="), (CONDITIONALS_NEGATION, ">=")],
        "&&" => &[(INVERT_LOGICAL, "||")],
        "||" => &[(INVERT_LOGICAL, "&&")],
        "*" => &[(ARITHMETIC_BASE, "/")],
        "*=" => &[(INVERT_ASSIGNMENTS, "/="), (REMOVE_SELF_ASSIGNMENTS, "=")],
        "/" => &[(ARITHMETIC_BASE, "*")],
        "/=" => &[(INVERT_ASSIGNMENTS, "*="), (REMOVE_SELF_ASSIGNMENTS, "=")],
        "%" => &[(ARITHMETIC_BASE, "*")],
        "%=" => &[(INVERT_ASSIGNMENTS, "%="), (REMOVE_SELF_ASSIGNMENTS, "=")],
        "-" => &[(INVERT_NEGATIVES, "+"), (ARITHMETIC_BASE, "+")],
        "-=" => &[(INVERT_ASSIGNMENTS, "+="), (REMOVE_SELF_ASSIGNMENTS, "=")],
        _ => &[],
    }
}

/// What kind of node a mutated token is the operator of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SiteKind {
    Binary,
    Unary,
    Assignment,
    IncDec,
    Branch,
}

/// One token Gremlins mutates, and its mutants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoSite {
    pub kind: SiteKind,
    /// The node the token is the operator of: the expression or statement.
    pub node: (usize, usize),
    /// The token's bytes.
    pub token: (usize, usize),
    pub operator: String,
    /// `(mutator, replacement)`, in Gremlins' order.
    pub mutants: Vec<(&'static str, &'static str)>,
}

/// Every token of `source` Gremlins mutates, in its order.
pub fn sites(tree: &tree_sitter::Tree, source: &str) -> Vec<GoSite> {
    let mut found = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if let Some(site) = site(node, source) {
            found.push(site);
        }
        let mut cursor = node.walk();
        let children = node.named_children(&mut cursor).collect::<Vec<_>>();
        stack.extend(children.into_iter().rev());
    }
    found
}

fn site(node: Node, source: &str) -> Option<GoSite> {
    let (kind, token) = match node.kind() {
        "binary_expression" => (SiteKind::Binary, node.child_by_field_name("operator")?),
        "unary_expression" => (SiteKind::Unary, node.child_by_field_name("operator")?),
        "assignment_statement" => (SiteKind::Assignment, node.child_by_field_name("operator")?),
        "inc_statement" | "dec_statement" => (
            SiteKind::IncDec,
            // The operand, then the token: the token is the last child.
            node.child(node.child_count().checked_sub(1)?)?,
        ),
        "break_statement" | "continue_statement" => (SiteKind::Branch, node.child(0)?),
        _ => return None,
    };
    let operator = source[token.byte_range()].to_owned();
    let mutants = match (kind, operator.as_str()) {
        // A unary expression's own table: `-x` and `+x` only (`!`, `^`,
        // `&`, `*`, `<-` have no listed mutator).
        (SiteKind::Unary, "-") => mutations("-").to_vec(),
        (SiteKind::Unary, "+") => mutations("+").to_vec(),
        (SiteKind::Unary, _) => Vec::new(),
        (_, operator) => mutations(operator).to_vec(),
    };
    if mutants.is_empty() {
        return None;
    }
    Some(GoSite {
        kind,
        node: (node.start_byte(), node.end_byte()),
        token: (token.start_byte(), token.end_byte()),
        operator,
        mutants,
    })
}

/// Each mutant of `source` in its list's order (the order
/// `candidate_mutants` lists them, the index a switch carries): its token's
/// bytes, its mutator and what the token becomes.
pub fn listed_tokens(
    source: &str,
) -> Result<Vec<((usize, usize), &'static str, &'static str)>, GoInstrumenterError> {
    let tree = parse(source)?;
    Ok(sites(&tree, source)
        .into_iter()
        .flat_map(|site| {
            site.mutants
                .into_iter()
                .map(move |(mutator, replacement)| (site.token, mutator, replacement))
        })
        .collect())
}

/// A listed mutant's id: its file, its token's span, mutator and
/// replacement.
pub fn mutant_id(
    file: &str,
    source: &str,
    token: (usize, usize),
    mutator: &str,
    replacement: &str,
) -> String {
    let (start, end) = (position(source, token.0), position(source, token.1));
    format!(
        "{file}:{}:{}-{}:{}:{mutator}:{replacement}",
        start.0, start.1, end.0, end.1
    )
}

/// 1-based line and 1-based byte column of `offset`, as Go reports a
/// position.
pub fn position(source: &str, offset: usize) -> (u32, u32) {
    let before = &source[..offset];
    let line = before.matches('\n').count() + 1;
    let column = offset - before.rfind('\n').map_or(0, |at| at + 1) + 1;
    (line as u32, column as u32)
}

/// Whether coverage gives this node a statement point: a statement in a
/// statement list (`go_instrumenter`'s rule).
fn statement_point(node: Node) -> bool {
    crate::go_instrumenter::is_statement(node.kind())
        && crate::go_instrumenter::in_statement_position(node)
}

fn function_point(node: Node) -> bool {
    matches!(
        node.kind(),
        "function_declaration" | "method_declaration" | "func_literal"
    ) && node.child_by_field_name("body").is_some()
}

/// The statement and function points around `node`, by id.
fn points(file: &str, node: Node) -> (Option<String>, Option<String>) {
    let mut statement = None;
    let mut function = None;
    let mut at = Some(node);
    while let Some(ancestor) = at {
        if statement.is_none() && function.is_none() && statement_point(ancestor) {
            statement = Some(stable_obligation_id(
                "go",
                file,
                "statement",
                ancestor.start_byte(),
                ancestor.end_byte(),
            ));
        }
        if function.is_none() && function_point(ancestor) {
            function = Some(stable_obligation_id(
                "go",
                file,
                "function",
                ancestor.start_byte(),
                ancestor.end_byte(),
            ));
        }
        at = ancestor.parent();
    }
    (statement, function)
}

/// `file`'s mutants, linked to the statement and function points coverage
/// gives the code around them.
pub fn candidate_mutants(
    file: &str,
    source: &str,
) -> Result<Vec<CandidateMutant>, GoInstrumenterError> {
    let tree = parse(source)?;
    Ok(candidates_of(file, source, &tree))
}

pub(crate) fn candidates_of(
    file: &str,
    source: &str,
    tree: &tree_sitter::Tree,
) -> Vec<CandidateMutant> {
    let mut listed = Vec::new();
    for site in sites(tree, source) {
        let node = tree
            .root_node()
            .descendant_for_byte_range(site.node.0, site.node.1)
            .expect("a site is a node of this tree");
        // The innermost node with exactly that range may be the site's own
        // parent's (a statement of one expression); points are read from
        // the token up either way.
        let token = tree
            .root_node()
            .descendant_for_byte_range(site.token.0, site.token.1)
            .unwrap_or(node);
        let (statement, function) = points(file, token);
        let original = source[site.node.0..site.node.1].to_owned();
        let start = position(source, site.token.0);
        let end = position(source, site.token.1);
        for (mutator, replacement) in &site.mutants {
            listed.push(CandidateMutant {
                id: String::new(),
                mutator: (*mutator).to_owned(),
                replacement: (*replacement).to_owned(),
                start,
                end,
                original: original.clone(),
                operator: Some(MutantOperator {
                    text: site.operator.clone(),
                    at: site.token.0 - site.node.0,
                }),
                value_used: false,
                statement: statement.clone(),
                function: function.clone(),
                decision: None,
                branch: None,
                chain: None,
                compare: None,
                probe: None,
                arrow: None,
                equivalent: (site.operator == *replacement).then(|| {
                    crate::js_mutant_links::StaticNote {
                        rule: IDENTICAL_TOKEN.to_owned(),
                        reason: format!(
                            "Gremlins replaces `{}` with `{replacement}`: the program as written",
                            site.operator
                        ),
                    }
                }),
                untestable: None,
                is_static: false,
                unplaceable: None,
            });
        }
    }
    listed
}

/// The static rule for a mutant whose token Gremlins replaces with itself
/// (`%=` under `INVERT_ASSIGNMENTS`).
pub const IDENTICAL_TOKEN: &str = "identical-token";

#[cfg(test)]
mod tests {
    use super::*;

    fn listed(source: &str) -> Vec<(u32, u32, String, String)> {
        candidate_mutants("a.go", source)
            .unwrap()
            .into_iter()
            .map(|m| (m.start.0, m.start.1, m.mutator, m.replacement))
            .collect()
    }

    #[test]
    fn each_token_gets_gremlins_mutators_in_its_table_order() {
        let source = "package p\n\nfunc f(a, b int) int {\n\tif a < b && -a > 0 {\n\t\ta -= b\n\t}\n\tfor {\n\t\ta++\n\t\tbreak\n\t}\n\treturn a % b\n}\n";
        let mutants = listed(source);
        let names = mutants
            .iter()
            .map(|(line, column, mutator, replacement)| {
                format!("{line}:{column} {mutator} {replacement}")
            })
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "4:11 INVERT_LOGICAL ||",
                "4:7 CONDITIONALS_BOUNDARY <=",
                "4:7 CONDITIONALS_NEGATION >=",
                "4:17 CONDITIONALS_BOUNDARY >=",
                "4:17 CONDITIONALS_NEGATION <=",
                "4:14 INVERT_NEGATIVES +",
                "4:14 ARITHMETIC_BASE +",
                "5:5 INVERT_ASSIGNMENTS +=",
                "5:5 REMOVE_SELF_ASSIGNMENTS =",
                "8:4 INCREMENT_DECREMENT --",
                "9:3 INVERT_LOOPCTRL continue",
                "11:11 ARITHMETIC_BASE *",
            ]
        );
    }

    #[test]
    fn a_mutant_links_to_the_statement_and_function_coverage_gives_it() {
        let source = "package p\n\nvar x = 1 + 2\n\nfunc f(a int) int {\n\treturn a + 1\n}\n";
        let mutants = candidate_mutants("a.go", source).unwrap();
        // Package level: no statement or function point is around it.
        assert_eq!(mutants[0].statement, None);
        assert_eq!(mutants[0].function, None);
        let returned = &mutants[1];
        let at = source.find("return").unwrap();
        assert_eq!(
            returned.statement.as_deref(),
            Some(stable_obligation_id("go", "a.go", "statement", at, at + 12).as_str())
        );
        let function = source.find("func").unwrap();
        assert_eq!(
            returned.function.as_deref(),
            Some(
                stable_obligation_id("go", "a.go", "function", function, source.len() - 1).as_str()
            )
        );
        assert_eq!(returned.original, "a + 1");
        assert_eq!(returned.operator.as_ref().unwrap().at, 2);
    }

    #[test]
    fn the_token_gremlins_maps_to_itself_is_equivalent() {
        let mutants = candidate_mutants("a.go", "package p\n\nfunc f(a int) { a %= 2 }\n").unwrap();
        assert_eq!(mutants[0].mutator, INVERT_ASSIGNMENTS);
        assert_eq!(mutants[0].replacement, "%=");
        assert_eq!(
            mutants[0]
                .equivalent
                .as_ref()
                .map(|note| note.rule.as_str()),
            Some(IDENTICAL_TOKEN)
        );
        assert!(mutants[1].equivalent.is_none());
    }

    #[test]
    fn unary_operators_without_a_listed_mutator_are_left() {
        assert!(listed("package p\n\nfunc f(a *int, b bool, c chan int) { _ = *a; _ = !b; _ = <-c; _ = ^*a }\n").is_empty());
        assert_eq!(
            listed("package p\n\nfunc f(a int) int { return +a }\n"),
            [(3, 28, ARITHMETIC_BASE.to_owned(), "-".to_owned())]
        );
    }

    #[test]
    fn positions_are_go_s_byte_columns() {
        assert_eq!(position("ab\n\tcé d", 7), (2, 5));
        assert_eq!(position("x", 0), (1, 1));
    }

    /// Every mutant this lists for the non-test `.go` files under `module`,
    /// as `scripts/gremlins-parity.mjs` writes Gremlins' (`file:line:column
    /// MUTATOR`), sorted.
    fn listed_under(module: &std::path::Path) -> Vec<String> {
        let mut listed = Vec::new();
        let mut directories = vec![module.to_path_buf()];
        while let Some(directory) = directories.pop() {
            for entry in std::fs::read_dir(&directory).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    directories.push(path);
                    continue;
                }
                let name = path.to_string_lossy();
                if !name.ends_with(".go") || name.ends_with("_test.go") {
                    continue;
                }
                let relative = path
                    .strip_prefix(module)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                let source = std::fs::read_to_string(&path).unwrap();
                for mutant in candidate_mutants(&relative, &source).unwrap() {
                    listed.push(format!(
                        "{relative}:{}:{} {}",
                        mutant.start.0, mutant.start.1, mutant.mutator
                    ));
                }
            }
        }
        listed.sort();
        listed
    }

    #[test]
    fn the_list_is_gremlins_own_on_the_parity_fixture() {
        let assets =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("test-assets/go-mutants");
        let golden: Vec<String> =
            serde_json::from_slice(&std::fs::read(assets.join("gremlins.json")).unwrap()).unwrap();
        assert_eq!(listed_under(&assets.join("fixture")), golden);
    }

    /// `SUPERCOV_GREMLINS_PARITY=<module>=<golden>[,...]`: the same check
    /// on modules outside the repository, with goldens the script wrote.
    #[test]
    #[ignore = "reads modules and goldens named by SUPERCOV_GREMLINS_PARITY"]
    fn the_list_is_gremlins_own_on_named_modules() {
        let named = std::env::var("SUPERCOV_GREMLINS_PARITY").unwrap();
        for pair in named.split(',') {
            let (module, golden) = pair.split_once('=').unwrap();
            let golden: Vec<String> =
                serde_json::from_slice(&std::fs::read(golden).unwrap()).unwrap();
            assert_eq!(
                listed_under(std::path::Path::new(module)),
                golden,
                "{module}"
            );
        }
    }
}
