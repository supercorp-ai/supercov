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
//! column) and its mutator (Gremlins' name, or StrykerJS's where the mutation is one of
//! StrykerJS's: `gremlins_name` maps back), and `scripts/gremlins-parity.mjs`
//! writes the goldens the tests below compare against.
//!
//! Every Gremlins mutator is listed, the ones Gremlins leaves off by
//! default among them.

use tree_sitter::Node;

use crate::go_instrumenter::{GoInstrumenterError, parse, stable_obligation_id};
use crate::js_mutant_links::{CandidateMutant, MutantOperator};

/// The mutant set every Go report names: Gremlins' version and the
/// mutators listed.
pub const MUTANT_SET: &str = "gremlins@0.6.0";

/// The mutators listed. Where a Gremlins mutator makes the mutation a
/// StrykerJS mutator makes, the mutant carries StrykerJS's name, which the
/// weak rules are keyed by (`docs/design/mutation-drivers.md`); Gremlins'
/// own name is `gremlins_name`'s. Two have no StrykerJS twin and keep
/// Gremlins' names: `INVERT_NEGATIVES` (whose `+` for `-` is also
/// `ARITHMETIC_BASE`'s, so the two cannot share a name) and
/// `INVERT_LOOPCTRL`. Gremlins' flags list the same set:
/// `--invert-assignments --invert-logical --invert-loopctrl
/// --remove-self-assignments --invert-bitwise --invert-bwassign` beside its
/// defaults: all of them.
pub const MUTATORS: &[&str] = &[
    ARITHMETIC_OPERATOR,
    ASSIGNMENT_OPERATOR,
    EQUALITY_OPERATOR,
    INVERT_BITWISE,
    INVERT_BWASSIGN,
    INVERT_LOOPCTRL,
    INVERT_NEGATIVES,
    LOGICAL_OPERATOR,
    UNARY_OPERATOR,
    UPDATE_OPERATOR,
];

/// `ARITHMETIC_BASE` of a binary operator.
pub const ARITHMETIC_OPERATOR: &str = "ArithmeticOperator";
/// `INVERT_ASSIGNMENTS` and `REMOVE_SELF_ASSIGNMENTS`.
pub const ASSIGNMENT_OPERATOR: &str = "AssignmentOperator";
/// `CONDITIONALS_BOUNDARY` and `CONDITIONALS_NEGATION`.
pub const EQUALITY_OPERATOR: &str = "EqualityOperator";
/// `INVERT_LOGICAL`.
pub const LOGICAL_OPERATOR: &str = "LogicalOperator";
/// `ARITHMETIC_BASE` of a unary `-` or `+`.
pub const UNARY_OPERATOR: &str = "UnaryOperator";
/// `INCREMENT_DECREMENT`.
pub const UPDATE_OPERATOR: &str = "UpdateOperator";
pub const INVERT_LOOPCTRL: &str = "INVERT_LOOPCTRL";
/// `&` `|` `^` `&^` `<<` `>>`, binary and unary, and a type union's `|`:
/// StrykerJS has no bitwise mutator, so Gremlins' name.
pub const INVERT_BITWISE: &str = "INVERT_BITWISE";
/// `&=` `|=` `^=` `&^=` `<<=` `>>=`.
pub const INVERT_BWASSIGN: &str = "INVERT_BWASSIGN";
pub const INVERT_NEGATIVES: &str = "INVERT_NEGATIVES";

/// Whether `mutator` is one a Go mutant can carry.
pub fn is_go_mutator(mutator: &str) -> bool {
    MUTATORS.contains(&mutator)
}

/// The Gremlins mutator a Go mutant is: from its mutator, the token as
/// written (`operator`) and what it becomes.
pub fn gremlins_name(mutator: &str, operator: &str, replacement: &str) -> &'static str {
    match mutator {
        ARITHMETIC_OPERATOR | UNARY_OPERATOR => "ARITHMETIC_BASE",
        ASSIGNMENT_OPERATOR if replacement == "=" => "REMOVE_SELF_ASSIGNMENTS",
        ASSIGNMENT_OPERATOR => "INVERT_ASSIGNMENTS",
        EQUALITY_OPERATOR => {
            let boundary = matches!(
                (operator, replacement),
                ("<", "<=") | ("<=", "<") | (">", ">=") | (">=", ">")
            );
            if boundary {
                "CONDITIONALS_BOUNDARY"
            } else {
                "CONDITIONALS_NEGATION"
            }
        }
        LOGICAL_OPERATOR => "INVERT_LOGICAL",
        UPDATE_OPERATOR => "INCREMENT_DECREMENT",
        INVERT_NEGATIVES => "INVERT_NEGATIVES",
        INVERT_BITWISE => "INVERT_BITWISE",
        INVERT_BWASSIGN => "INVERT_BWASSIGN",
        _ => "INVERT_LOOPCTRL",
    }
}

/// Each mutator Gremlins applies to `token`, in its table's order, with
/// what the token becomes; only the listed mutators.
///
/// `INVERT_ASSIGNMENTS` makes `%=` of `%=`: Gremlins' table maps it to
/// itself. The mutant is the program as written, and is listed as Gremlins
/// lists it; it is equivalent by construction (`identical-token`).
fn mutations(token: &str) -> &'static [(&'static str, &'static str)] {
    match token {
        "+" => &[(ARITHMETIC_OPERATOR, "-")],
        "+=" => &[(ASSIGNMENT_OPERATOR, "-="), (ASSIGNMENT_OPERATOR, "=")],
        "&=" => &[(ASSIGNMENT_OPERATOR, "="), (INVERT_BWASSIGN, "|=")],
        "|=" => &[(ASSIGNMENT_OPERATOR, "="), (INVERT_BWASSIGN, "&=")],
        "^=" => &[(ASSIGNMENT_OPERATOR, "="), (INVERT_BWASSIGN, "&=")],
        "&^=" => &[(ASSIGNMENT_OPERATOR, "="), (INVERT_BWASSIGN, "&=")],
        "<<=" => &[(ASSIGNMENT_OPERATOR, "="), (INVERT_BWASSIGN, ">>=")],
        ">>=" => &[(ASSIGNMENT_OPERATOR, "="), (INVERT_BWASSIGN, "<<=")],
        "&" => &[(INVERT_BITWISE, "|")],
        "|" => &[(INVERT_BITWISE, "&")],
        "^" => &[(INVERT_BITWISE, "&")],
        "&^" => &[(INVERT_BITWISE, "&")],
        "<<" => &[(INVERT_BITWISE, ">>")],
        ">>" => &[(INVERT_BITWISE, "<<")],
        "break" => &[(INVERT_LOOPCTRL, "continue")],
        "continue" => &[(INVERT_LOOPCTRL, "break")],
        "--" => &[(UPDATE_OPERATOR, "++")],
        "++" => &[(UPDATE_OPERATOR, "--")],
        "==" => &[(EQUALITY_OPERATOR, "!=")],
        "!=" => &[(EQUALITY_OPERATOR, "==")],
        ">=" => &[(EQUALITY_OPERATOR, ">"), (EQUALITY_OPERATOR, "<")],
        ">" => &[(EQUALITY_OPERATOR, ">="), (EQUALITY_OPERATOR, "<=")],
        "<=" => &[(EQUALITY_OPERATOR, "<"), (EQUALITY_OPERATOR, ">")],
        "<" => &[(EQUALITY_OPERATOR, "<="), (EQUALITY_OPERATOR, ">=")],
        "&&" => &[(LOGICAL_OPERATOR, "||")],
        "||" => &[(LOGICAL_OPERATOR, "&&")],
        "*" => &[(ARITHMETIC_OPERATOR, "/")],
        "*=" => &[(ASSIGNMENT_OPERATOR, "/="), (ASSIGNMENT_OPERATOR, "=")],
        "/" => &[(ARITHMETIC_OPERATOR, "*")],
        "/=" => &[(ASSIGNMENT_OPERATOR, "*="), (ASSIGNMENT_OPERATOR, "=")],
        "%" => &[(ARITHMETIC_OPERATOR, "*")],
        "%=" => &[(ASSIGNMENT_OPERATOR, "%="), (ASSIGNMENT_OPERATOR, "=")],
        "-" => &[(INVERT_NEGATIVES, "+"), (ARITHMETIC_OPERATOR, "+")],
        "-=" => &[(ASSIGNMENT_OPERATOR, "+="), (ASSIGNMENT_OPERATOR, "=")],
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
        found.extend(union_sites(node));
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
        (SiteKind::Unary, "-") => vec![(INVERT_NEGATIVES, "+"), (UNARY_OPERATOR, "+")],
        (SiteKind::Unary, "+") => vec![(UNARY_OPERATOR, "-")],
        // `&x` and `^x`: Gremlins' table is the binary one's.
        (SiteKind::Unary, "&") => vec![(INVERT_BITWISE, "|")],
        (SiteKind::Unary, "^") => vec![(INVERT_BITWISE, "&")],
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

/// A listed mutant's token bytes, mutator and replacement.
pub type ListedToken = ((usize, usize), &'static str, &'static str);

/// Each mutant of `source` in its list's order (the order
/// `candidate_mutants` lists them, the index a switch carries): its token's
/// bytes, its mutator and what the token becomes.
pub fn listed_tokens(source: &str) -> Result<Vec<ListedToken>, GoInstrumenterError> {
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

/// A type union's `|`s (`~int | ~float64`, in a constraint): go/ast reads
/// `A | B | C` as `(A | B) | C`, binary expressions Gremlins mutates, the
/// last `|` first. The mutant is never a program Go accepts.
fn union_sites(node: Node) -> Vec<GoSite> {
    if node.kind() != "type_elem" {
        return Vec::new();
    }
    let mut cursor = node.walk();
    let children = node.children(&mut cursor).collect::<Vec<_>>();
    children
        .iter()
        .enumerate()
        .filter(|(_, child)| child.kind() == "|")
        .rev()
        .map(|(at, bar)| GoSite {
            kind: SiteKind::Binary,
            node: (node.start_byte(), children[at + 1].end_byte()),
            token: (bar.start_byte(), bar.end_byte()),
            operator: "|".to_owned(),
            mutants: vec![(INVERT_BITWISE, "&")],
        })
        .collect()
}

/// 1-based line and 1-based byte column of `offset`, as Go reports a
/// position.
pub fn position(source: &str, offset: usize) -> (u32, u32) {
    let before = &source[..offset];
    let line = before.matches('\n').count() + 1;
    let column = offset - before.rfind('\n').map_or(0, |at| at + 1) + 1;
    (line as u32, column as u32)
}

/// Whether a measured run with mutants puts weak mutation's value probe on
/// this comparison: a relational one (`<`, `<=`, `>`, `>=`, whose boundary
/// mutants only equal operands tell apart) inside a function body. The
/// probe is a generic call over the operands, so not where Go needs a
/// constant (a constant declaration, an array length), nor where an operand
/// takes its type from the comparison (an untyped shift, `1 << n`), nor
/// where both operands are literals, which the call would type alone.
pub fn comparison_probe(node: Node, source: &str) -> bool {
    if node.kind() != "binary_expression" {
        return false;
    }
    let Some(operator) = node.child_by_field_name("operator") else {
        return false;
    };
    if !matches!(&source[operator.byte_range()], "<" | "<=" | ">" | ">=") {
        return false;
    }
    let mut in_function = false;
    let mut at = node.parent();
    while let Some(ancestor) = at {
        match ancestor.kind() {
            "const_declaration" | "array_type" | "type_declaration" => return false,
            "function_declaration" | "method_declaration" | "func_literal" => in_function = true,
            _ => {}
        }
        at = ancestor.parent();
    }
    let literal = |side: &str| {
        node.child_by_field_name(side).is_some_and(|operand| {
            matches!(
                operand.kind(),
                "int_literal"
                    | "float_literal"
                    | "rune_literal"
                    | "imaginary_literal"
                    | "interpreted_string_literal"
                    | "raw_string_literal"
            )
        })
    };
    let shifted = {
        let mut stack = vec![node];
        let mut found = false;
        while let Some(next) = stack.pop() {
            if next.kind() == "binary_expression"
                && next
                    .child_by_field_name("operator")
                    .is_some_and(|op| matches!(&source[op.byte_range()], "<<" | ">>"))
            {
                found = true;
                break;
            }
            let mut cursor = next.walk();
            stack.extend(next.named_children(&mut cursor));
        }
        found
    };
    in_function && !shifted && !(literal("left") && literal("right"))
}

/// Whether a mutant of the token at `token` can change what `go vet`
/// reports among the checks `go test` runs. Of them (atomic, bools,
/// buildtag, directive, errorsas, ifaceassert, nilfunc, printf, stdversion,
/// stringintconv, tests), only two read what a token mutation changes:
/// `bools`, of the operands of a chain of `&&` or `||` (`x != a || x != b`),
/// and `atomic`, of an assignment of an `atomic.Add` call to its own
/// operand (`x += atomic.AddInt64(&x, 1)` to `x = ...`). The rest read
/// types, formats and names a mutation leaves as they were.
pub fn vet_candidate(source: &str, token: (usize, usize)) -> bool {
    let Ok(tree) = parse(source) else {
        return false;
    };
    let Some(node) = tree.root_node().descendant_for_byte_range(token.0, token.1) else {
        return false;
    };
    let mut at = node.parent();
    while let Some(ancestor) = at {
        match ancestor.kind() {
            "binary_expression" => {
                let logical = ancestor
                    .child_by_field_name("operator")
                    .is_some_and(|op| matches!(&source[op.byte_range()], "&&" | "||"));
                if logical {
                    return true;
                }
            }
            "parenthesized_expression" | "unary_expression" => {}
            "assignment_statement" => {
                return source[ancestor.byte_range()].contains("atomic.");
            }
            _ => return false,
        }
        at = ancestor.parent();
    }
    false
}

/// The site a comparison probe at `operator` records under.
pub fn comparison_site(file: &str, source: &str, operator: usize) -> String {
    let (line, column) = position(source, operator);
    format!("{file}:{line}:{column}")
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
    Ok(candidates_of(
        file,
        source,
        &tree,
        &std::collections::HashSet::new(),
    ))
}

/// `candidate_mutants`, each comparison linked to the value probe the run
/// put on it (`probed`, by site): only a probe's records say a comparison
/// never ran.
pub fn candidate_mutants_probed(
    file: &str,
    source: &str,
    probed: &std::collections::HashSet<String>,
) -> Result<Vec<CandidateMutant>, GoInstrumenterError> {
    let tree = parse(source)?;
    Ok(candidates_of(file, source, &tree, probed))
}

pub(crate) fn candidates_of(
    file: &str,
    source: &str,
    tree: &tree_sitter::Tree,
    probed: &std::collections::HashSet<String>,
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
        let compare =
            Some(comparison_site(file, source, site.token.0)).filter(|site| probed.contains(site));
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
                compare: compare.clone(),
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

/// A run's Go mutants listed from its sources after the run: each file in
/// `recorded` whose statement and function points, instrumented again, are
/// the ones the run recorded (else it changed since, and is named so).
pub(crate) fn list_mutants(
    root: &std::path::Path,
    recorded: &std::collections::BTreeMap<String, std::collections::BTreeSet<String>>,
) -> crate::mutant_list::ListedMutants {
    let files = recorded.keys().cloned().collect::<Vec<_>>();
    let listed = crate::threads::parallel_map(&files, |file| {
        let source = std::fs::read_to_string(root.join(file)).ok()?;
        let obligations =
            crate::go_instrumenter::build_go_obligations(file, &source, &mut 0, &mut 0).ok()?;
        let points = obligations
            .manifest
            .points
            .iter()
            .map(|point| point.id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        (Some(&points) == recorded.get(file)).then(|| candidate_mutants(file, &source).ok())?
    });
    let mut mutants = std::collections::BTreeMap::new();
    let mut changed = Vec::new();
    for (file, listed) in files.into_iter().zip(listed) {
        match listed {
            Some(list) if list.is_empty() => {}
            Some(list) => {
                mutants.insert(file, list);
            }
            None => changed.push(file),
        }
    }
    crate::mutant_list::ListedMutants {
        mutants: crate::mutant_list::MutantsFile::listed(GO, MUTANT_SET, mutants),
        changed,
    }
}

/// The language name a list gives Go files.
pub const GO: &str = "go";

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
            .map(|m| {
                let operator = m.operator.as_ref().unwrap().text.clone();
                (
                    m.start.0,
                    m.start.1,
                    gremlins_name(&m.mutator, &operator, &m.replacement).to_owned(),
                    m.replacement,
                )
            })
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
        assert_eq!(mutants[0].mutator, ASSIGNMENT_OPERATOR);
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
        assert!(
            listed("package p\n\nfunc f(a *int, b bool, c chan int) { _ = *a; _ = !b; _ = <-c }\n")
                .is_empty()
        );
        assert_eq!(
            listed("package p\n\nfunc f(a int) int { return +a }\n"),
            [(3, 28, "ARITHMETIC_BASE".to_owned(), "-".to_owned())]
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
                    let operator = &mutant.operator.as_ref().unwrap().text;
                    listed.push(format!(
                        "{relative}:{}:{} {}",
                        mutant.start.0,
                        mutant.start.1,
                        gremlins_name(&mutant.mutator, operator, &mutant.replacement)
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
