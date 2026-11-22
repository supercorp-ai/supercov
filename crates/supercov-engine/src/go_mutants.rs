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
    collect_sites(tree.root_node(), source, &mut found);
    found
}

/// `node`'s sites, then its children's, in order (`ast.Inspect`'s).
fn collect_sites(node: Node, source: &str, found: &mut Vec<GoSite>) {
    found.extend(site(node, source));
    found.extend(union_sites(node));
    node.named_children(&mut node.walk())
        .for_each(|child| collect_sites(child, source, found));
}

/// The site `node` is the operator's node of, if any.
pub(crate) fn site_of(node: Node, source: &str) -> Option<GoSite> {
    site(node, source)
}

/// Whether reading `node` again gives what reading it once did, and does
/// nothing else: a name, a field of one, an element at a name or a literal.
pub(crate) fn reads_alike(node: Node, source: &str) -> bool {
    match node.kind() {
        "identifier" | "field_identifier" | "int_literal" => &source[node.byte_range()] != "_",
        "selector_expression" => node
            .child_by_field_name("operand")
            .is_some_and(|operand| reads_alike(operand, source)),
        "index_expression" => {
            node.child_by_field_name("operand")
                .is_some_and(|operand| reads_alike(operand, source))
                && node
                    .child_by_field_name("index")
                    .is_some_and(|index| reads_alike(index, source))
        }
        "parenthesized_expression" => node
            .named_child(0)
            .is_some_and(|inner| reads_alike(inner, source)),
        _ => false,
    }
}

fn site(node: Node, source: &str) -> Option<GoSite> {
    // A tree with no error gives each of these its operator or keyword.
    let kind = match node.kind() {
        "binary_expression" => SiteKind::Binary,
        "unary_expression" => SiteKind::Unary,
        "assignment_statement" => SiteKind::Assignment,
        "inc_statement" | "dec_statement" => SiteKind::IncDec,
        "break_statement" | "continue_statement" => SiteKind::Branch,
        _ => return None,
    };
    let token = match kind {
        SiteKind::Binary | SiteKind::Unary | SiteKind::Assignment => {
            node.child_by_field_name("operator")
        }
        // The operand, then the token: the token is the last child.
        SiteKind::IncDec => node.child(node.child_count() - 1),
        SiteKind::Branch => node.child(0),
    }
    .expect("a parsed node has its operator");
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
    let relational = node.kind() == "binary_expression"
        && operator_of(node, source).is_some_and(|op| matches!(op, "<" | "<=" | ">" | ">="));
    if !relational {
        return false;
    }
    let ancestors = std::iter::successors(node.parent(), Node::parent).collect::<Vec<_>>();
    let constant = ancestors.iter().any(|ancestor| {
        matches!(
            ancestor.kind(),
            "const_declaration" | "array_type" | "type_declaration"
        )
    });
    let in_function = ancestors.iter().any(|ancestor| {
        matches!(
            ancestor.kind(),
            "function_declaration" | "method_declaration" | "func_literal"
        )
    });
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
    let typed_alone = literal("left") && literal("right");
    in_function && !(constant || typed_alone || shifts(node, source))
}

/// A binary node's operator.
fn operator_of<'s>(node: Node, source: &'s str) -> Option<&'s str> {
    node.child_by_field_name("operator")
        .map(|operator| &source[operator.byte_range()])
}

/// Whether `node` is or holds a shift.
fn shifts(node: Node, source: &str) -> bool {
    (node.kind() == "binary_expression"
        && operator_of(node, source).is_some_and(|op| matches!(op, "<<" | ">>")))
        || node
            .named_children(&mut node.walk())
            .any(|child| shifts(child, source))
}

/// Whether a mutant of the token at `token` can change what `go vet`
/// reports among the checks `go test` runs. Of them (atomic, bools,
/// buildtag, directive, errorsas, ifaceassert, nilfunc, printf, stdversion,
/// stringintconv, tests), only two read what a token mutation changes:
/// `bools`, of the operands of a chain of `&&` or `||` (`x != a || x != b`),
/// and `atomic`, of an assignment of an `atomic.Add` call to its own
/// operand (`x += atomic.AddInt64(&x, 1)` to `x = ...`). The rest read
/// types, formats and names a mutation leaves as they were. `source`
/// parses: it is the file a mutant was listed from.
pub fn vet_candidate(source: &str, token: (usize, usize)) -> bool {
    let tree = parse(source).expect("a listed file parses");
    let node = tree
        .root_node()
        .descendant_for_byte_range(token.0, token.1)
        .expect("a listed token is in its file");
    // Up through the expressions around the token: the first that is not
    // a parenthesis or a negation says.
    std::iter::successors(node.parent(), Node::parent)
        .find(|ancestor| {
            let through = match ancestor.kind() {
                "parenthesized_expression" | "unary_expression" => true,
                "binary_expression" => {
                    !operator_of(*ancestor, source).is_some_and(|op| matches!(op, "&&" | "||"))
                }
                _ => false,
            };
            !through
        })
        .is_some_and(|ancestor| match ancestor.kind() {
            "binary_expression" => true,
            "assignment_statement" => source[ancestor.byte_range()].contains("atomic."),
            _ => false,
        })
}

/// The site a comparison probe at `operator` records under.
pub fn comparison_site(file: &str, source: &str, operator: usize) -> String {
    let (line, column) = position(source, operator);
    format!("{file}:{line}:{column}")
}

/// Whether a comparison's operands are worked out from a clock reading
/// (`Now()`, `UnixNano()`, `Since(`, `Until(`) or a random one (`rand.`),
/// in the function around it: either directly, or through the names its
/// assignments give them, as far as they go (`nano := timeNow().UnixNano()`,
/// `now := nano >> 8`, `last = now`). `clock`, `random`, or none.
fn reading(node: Node, source: &str) -> Option<&'static str> {
    let function = std::iter::successors(node.parent(), Node::parent)
        .find(|ancestor| function_point(*ancestor))?;
    let mut assignments = Vec::new();
    collect_assignments(function, source, &mut assignments);
    let operands = &source[node.byte_range()];
    [
        ("clock", &["Now()", "UnixNano()", "Since(", "Until("][..]),
        ("random", &["rand."][..]),
    ]
    .into_iter()
    .find(|(_, seeds)| {
        let seeded = |text: &str| seeds.iter().any(|seed| text.contains(seed));
        let mut tainted = std::collections::BTreeSet::<String>::new();
        // Each round taints the names a tainted value is assigned to; a
        // round that adds none is the last.
        while {
            let reached = assignments
                .iter()
                .filter(|(_, value)| seeded(value) || mentions(value, &tainted))
                .flat_map(|(names, _)| names.iter().cloned())
                .collect::<Vec<_>>();
            let before = tainted.len();
            tainted.extend(reached);
            tainted.len() > before
        } {}
        seeded(operands) || mentions(operands, &tainted)
    })
    .map(|(rule, _)| rule)
}

/// Each assignment and short declaration in `node`: its names and its
/// values' text.
fn collect_assignments(node: Node, source: &str, found: &mut Vec<(Vec<String>, String)>) {
    if matches!(
        node.kind(),
        "assignment_statement" | "short_var_declaration"
    ) {
        let names = field(node, "left")
            .named_children(&mut node.walk())
            .map(|name| source[name.byte_range()].to_owned())
            .collect();
        found.push((names, source[field(node, "right").byte_range()].to_owned()));
    }
    node.named_children(&mut node.walk())
        .for_each(|child| collect_assignments(child, source, found));
}

fn field<'t>(node: Node<'t>, name: &str) -> Node<'t> {
    node.child_by_field_name(name)
        .expect("a parsed node has its field")
}

/// Whether `text` names any of `names` as a whole word.
fn mentions(text: &str, names: &std::collections::BTreeSet<String>) -> bool {
    let words = text
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .collect::<std::collections::HashSet<_>>();
    names.iter().any(|name| words.contains(name.as_str()))
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
    let ancestors = std::iter::successors(Some(node), Node::parent).collect::<Vec<_>>();
    let function = ancestors
        .iter()
        .position(|ancestor| function_point(*ancestor));
    // The statement inside the innermost function, not one around it.
    let statement = ancestors[..function.unwrap_or(ancestors.len())]
        .iter()
        .find(|ancestor| statement_point(**ancestor));
    let id = |kind: &str, at: &Node| {
        stable_obligation_id("go", file, kind, at.start_byte(), at.end_byte())
    };
    (
        statement.map(|at| id("statement", at)),
        function.map(|at| id("function", &ancestors[at])),
    )
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
        let probed_here =
            Some(comparison_site(file, source, site.token.0)).filter(|site| probed.contains(site));
        let relational = matches!(site.operator.as_str(), "<" | "<=" | ">" | ">=")
            && site.kind == SiteKind::Binary;
        let compare = probed_here.clone().filter(|_| relational);
        // An operation's probe, where the run put one there: its swap's
        // value at `#0`, a removed self-assignment's at `#1`.
        let operation = probed_here.filter(|_| !relational);
        // A comparison of a clock or random reading: this run's values never
        // being equal says nothing of the next run's.
        let untestable = relational
            .then(|| reading(node, source))
            .flatten()
            .map(|rule| crate::js_mutant_links::StaticNote {
                rule: rule.to_owned(),
                reason: format!(
                    "an operand is worked out from a {rule} reading in its function: another run's values may meet where this run's did not"
                ),
            });
        let original = source[site.node.0..site.node.1].to_owned();
        let start = position(source, site.token.0);
        let end = position(source, site.token.1);
        site.mutants.iter().for_each(|(mutator, replacement)| {
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
                effects: false,
                statement: statement.clone(),
                function: function.clone(),
                decision: None,
                branch: None,
                chain: None,
                compare: compare.clone(),
                probe: operation
                    .as_ref()
                    .map(|base| format!("{base}#{}", u8::from(*replacement == "="))),
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
                untestable: untestable.clone(),
                is_static: false,
                unplaceable: None,
            });
        });
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
    fn comparisons_of_clock_or_random_readings_are_untestable() {
        let source = "package p\n\nvar limit = 3 < 4\n\nfunc f(last int64, n int) bool {\n\tnano := timeNow().UnixNano()\n\tnow := nano >> 8\n\tvar seen int64\n\tseen = now\n\tif seen <= last {\n\t\treturn true\n\t}\n\tif rand.Intn(9) > n {\n\t\treturn false\n\t}\n\treturn n < 2\n}\n";
        let rules = candidate_mutants("a.go", source)
            .unwrap()
            .into_iter()
            .filter(|mutant| {
                let operator = mutant.operator.as_ref().map_or("", |op| op.text.as_str());
                gremlins_name(&mutant.mutator, operator, &mutant.replacement)
                    == "CONDITIONALS_BOUNDARY"
            })
            .map(|mutant| (mutant.start.0, mutant.untestable.map(|note| note.rule)))
            .collect::<Vec<_>>();
        assert_eq!(
            rules,
            [
                (3, None),
                (10, Some("clock".to_owned())),
                (13, Some("random".to_owned())),
                (16, None),
            ]
        );
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
        // A run of every test, ignored ones too, names none.
        let Ok(named) = std::env::var("SUPERCOV_GREMLINS_PARITY") else {
            return;
        };
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

    /// The binary node of `source` whose operator is the first `operator`
    /// after `after`.
    fn binary<'t>(
        tree: &'t tree_sitter::Tree,
        source: &str,
        after: &str,
        operator: &str,
    ) -> Node<'t> {
        let start = source.find(after).unwrap();
        let at = start + source[start..].find(operator).unwrap();
        let mut node = tree
            .root_node()
            .descendant_for_byte_range(at, at + operator.len())
            .unwrap();
        while node.kind() != "binary_expression" {
            node = node.parent().unwrap();
        }
        node
    }

    #[test]
    fn a_comparison_is_probed_only_where_a_generic_call_can_stand() {
        let source = "package p\n\nconst c = 1 < 2\n\nvar v = 1 < 2\n\ntype T [2]int\n\nfunc f(a, b int) bool {\n\t_ = func() bool { return a < b }\n\t_ = 1 < 2\n\t_ = a<<1 > b\n\t_ = a == b\n\treturn a >= 1\n}\n\nfunc (T) m(a int) bool { return a <= 2 }\n";
        let tree = parse(source).unwrap();
        let probed = |after: &str, operator: &str| {
            comparison_probe(binary(&tree, source, after, operator), source)
        };
        assert!(!probed("const c", "<"));
        assert!(!probed("var v", "<"));
        assert!(probed("return a < b", "<"));
        assert!(!probed("_ = 1 < 2", "<"));
        assert!(!probed("a<<1 >", ">"));
        assert!(!probed("a == b", "=="));
        assert!(probed("a >= 1", ">="));
        assert!(probed("a <= 2", "<="));
        assert!(!comparison_probe(tree.root_node(), source));
        // An array length is no place for one either.
        let source = "package p\n\nfunc f() { var a [len(\"ab\") + 1]int; _ = a }\n";
        let tree = parse(source).unwrap();
        let node = binary(&tree, source, "len", "+");
        assert!(!comparison_probe(node, source));
    }

    #[test]
    fn only_a_logical_chain_or_an_atomic_assignment_is_vetted() {
        let source = "package p\n\nimport \"sync/atomic\"\n\nfunc f(x int, n *int64) bool {\n\ty := x + 1\n\ty += int(atomic.AddInt64(n, 1))\n\ty -= 1\n\treturn !(x != 1) && (x != y+1)\n}\n";
        let token = |after: &str, operator: &str| {
            let start = source.find(after).unwrap();
            let at = start + source[start..].find(operator).unwrap();
            (at, at + operator.len())
        };
        assert!(vet_candidate(source, token("!(x != 1)", "!=")));
        assert!(vet_candidate(source, token("(x != y+1)", "+")));
        assert!(vet_candidate(source, token(") && (", "&&")));
        assert!(vet_candidate(source, token("y += int", "+=")));
        assert!(!vet_candidate(source, token("y -= 1", "-=")));
        assert!(!vet_candidate(source, token("x + 1", "+")));
    }

    #[test]
    fn what_reads_alike_twice() {
        let source = "package p\n\nfunc f() { a.b[c][1], (d), _, e[g()], h().i, *j = 0, 0, 0, 0, 0, 0, 0 }\n";
        let tree = parse(source).unwrap();
        let left = tree
            .root_node()
            .descendant_for_byte_range(source.find("a.b").unwrap(), source.find("a.b").unwrap() + 1)
            .unwrap();
        let list = std::iter::successors(Some(left), Node::parent)
            .find(|node| node.kind() == "expression_list")
            .unwrap();
        let alike = list
            .named_children(&mut list.walk())
            .map(|operand| reads_alike(operand, source))
            .collect::<Vec<_>>();
        assert_eq!(alike, [true, true, false, false, false, false]);
    }

    #[test]
    fn a_file_that_does_not_parse_lists_nothing() {
        assert!(candidate_mutants("a.go", "package p\n\nfunc (").is_err());
        assert!(
            candidate_mutants_probed("a.go", "package p\n\nfunc (", &Default::default()).is_err()
        );
        assert!(listed_tokens("package p\n\nfunc (").is_err());
    }

    #[test]
    fn a_method_s_and_a_closure_s_points_are_their_own() {
        let source = "package p\n\ntype T int\n\nfunc (T) m(a int) int {\n\tf := func() int {\n\t\treturn a + 1\n\t}\n\treturn f() - 1\n}\n";
        let mutants = candidate_mutants("a.go", source).unwrap();
        let inner = &mutants[0];
        let at = source.find("return a + 1").unwrap();
        assert_eq!(
            inner.statement.as_deref(),
            Some(stable_obligation_id("go", "a.go", "statement", at, at + 12).as_str())
        );
        let literal = source.find("func() int").unwrap();
        assert!(
            inner.function.as_deref().unwrap()
                == stable_obligation_id(
                    "go",
                    "a.go",
                    "function",
                    literal,
                    source.find("\t}\n\treturn f()").unwrap() + 2
                )
        );
        let method = source.find("func (T)").unwrap();
        assert_eq!(
            mutants.last().unwrap().function.as_deref(),
            Some(stable_obligation_id("go", "a.go", "function", method, source.len() - 1).as_str())
        );
    }

    #[test]
    fn a_run_s_mutants_are_listed_from_its_sources_where_they_did_not_change() {
        let root = std::env::temp_dir().join(format!("supercov-go-list-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let sources = [
            ("a.go", "package p\n\nfunc f(a int) int { return a + 1 }\n"),
            ("b.go", "package p\n\nfunc g() {}\n"),
            ("c.go", "package p\n\nfunc h(a int) int { return a - 1 }\n"),
        ];
        let mut recorded = std::collections::BTreeMap::new();
        for (file, source) in sources {
            std::fs::write(root.join(file), source).unwrap();
            let points = crate::go_instrumenter::build_go_obligations(file, source, &mut 0, &mut 0)
                .unwrap()
                .manifest
                .points
                .into_iter()
                .map(|point| point.id)
                .collect::<std::collections::BTreeSet<_>>();
            recorded.insert(file.to_owned(), points);
        }
        // Changed since: another statement.
        std::fs::write(
            root.join("c.go"),
            "package p\n\nfunc h(a int) int { a++; return a - 1 }\n",
        )
        .unwrap();
        recorded.insert("gone.go".to_owned(), Default::default());
        let listed = list_mutants(&root, &recorded);
        assert_eq!(listed.mutants.files.keys().collect::<Vec<_>>(), ["a.go"]);
        assert_eq!(listed.changed, ["c.go", "gone.go"]);
        assert_eq!(listed.mutants.mutant_set.as_deref(), Some(MUTANT_SET));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_go_operation_s_probe_decides_its_weak_verdict() {
        use crate::js_weak_mutation::{Observations, Status, verdict};
        let source = "package p\n\nfunc f(a int) int {\n\tfor {\n\t\tbreak\n\t}\n\treturn -a\n}\n";
        let mutants = candidate_mutants("a.go", source).unwrap();
        let mut seen = Observations::default();
        seen.hits
            .extend(mutants.iter().filter_map(|mutant| mutant.statement.clone()));
        seen.hits
            .extend(mutants.iter().filter_map(|mutant| mutant.function.clone()));
        let (status, _) = verdict(&mutants[0], &seen);
        assert_eq!(status, Status::Infected { proxy: true });
        // `-a` to `+a`: no probe, no verdict from values; with one, its
        // values decide.
        let mut negated = mutants[1].clone();
        assert_eq!(negated.mutator, INVERT_NEGATIVES);
        assert_eq!(verdict(&negated, &seen).0, Status::Unknown);
        negated.probe = Some("a.go:7:9#0".into());
        seen.comparisons
            .insert("a.go:7:9#0".into(), ["same".to_owned()].into());
        assert_eq!(verdict(&negated, &seen).0, Status::NotInfected);
    }
}
