//! Weak mutation (Howden 1982) for JavaScript and TypeScript: how far one
//! run's tests got toward killing each StrykerJS mutant, from what the run
//! observed at the evidence `js_mutant_links` linked it to.
//!
//! A mutant is reached when a test ran its code, and infected when some
//! evaluation there gave the mutant another value than the original's. A
//! mutant no test infects cannot be killed by these tests, so `NotInfected`
//! and `NotReached` are claims about StrykerJS's results: each must be a
//! StrykerJS survivor. Every rule below that says either is sound only from
//! what was recorded; where the record cannot tell, the verdict is
//! `Unknown`. `Infected` is a candidate kill, not a kill.
//!
//! A server process records hits and decision vectors, and the outcomes of
//! its comparisons, selections and probes as records the report reads as a
//! snapshot's values. A runtime older than those records wrote none, and
//! says nothing of it: a `NotInfected` or `NotReached` read from values is
//! `Unknown` when the mutant's statement ran in a server process whose
//! records do not say they carry values (`SERVER_VALUES_RECORD`). Its
//! outcomes there are missing, not absent.
//!
//! The rules are those of an earlier research prototype, which matched
//! evidence by text;
//! here every rule reads a link made from exact spans.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};

use crate::coverage_analysis::McdcVector;
use crate::coverage_index::{IndexedMutant, MutantStatus, MutationResults};
use crate::coverage_report::{RawTestResult, SERVER_VALUES_RECORD};
use crate::js_mutant_links::{BranchRole, CandidateMutant, MutantsFile};

/// How far the run's tests got toward killing a mutant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Status {
    /// No test ran the mutated code.
    NotReached,
    /// Reached, but every evaluation gave the mutant the original's value.
    NotInfected,
    /// Some evaluation gave the mutant another value. A `proxy` infection is
    /// only that the code around it ran (a block or call removed).
    Infected { proxy: bool },
    /// Reached, but the run cannot tell.
    Unknown,
}

/// Why: a stable short code, and a sentence that says, for a mutant not
/// infected, which input is missing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Reason {
    pub code: &'static str,
    pub sentence: String,
}

/// One evaluation of a decision: its conditions' values (`None` where one
/// was not evaluated) and its outcome.
pub(crate) type Vector = (Vec<Option<bool>>, bool);

/// What one run observed, as plain data.
#[derive(Debug, Clone, Default)]
pub(crate) struct Observations {
    /// Every point hit, branch outcomes (`id:short`, `id:continued`) among
    /// them.
    pub hits: HashSet<String>,
    /// The hits a server process recorded without saying it records values:
    /// a runtime older than its value records.
    pub server_hits: HashSet<String>,
    /// The hits of tests measured without the probes (a merged run's
    /// `testsWithoutValues`): they ran the code and recorded no values.
    pub unvalued_hits: HashSet<String>,
    /// Each decision's evaluations.
    pub decisions: HashMap<String, Vec<Vector>>,
    /// Each logical selection's outcomes: whether its right side produced
    /// the value, and whether the value was truthy.
    pub logicals: HashMap<String, HashSet<(bool, bool)>>,
    /// Each comparison or infection probe site's outcomes.
    pub comparisons: HashMap<String, HashSet<String>>,
}

const BLOCK_STATEMENT: &str = "BlockStatement";

/// The mutant's status, and why.
pub(crate) fn verdict(mutant: &CandidateMutant, observations: &Observations) -> (Status, Reason) {
    let records = Records {
        observations,
        read: Cell::new(false),
    };
    let (status, reason) = decide(mutant, &records);
    let ran_in = |hits: &HashSet<String>| {
        [&mutant.statement, &mutant.function]
            .into_iter()
            .flatten()
            .any(|point| hits.contains(point))
    };
    let unkilled = records.read.get() && matches!(status, Status::NotReached | Status::NotInfected);
    if unkilled && ran_in(&observations.server_hits) {
        return (
            Status::Unknown,
            reason_of(
                "server-process",
                format!(
                    "ran in a server process whose records carry no values ({})",
                    reason.sentence
                ),
            ),
        );
    }
    if unkilled && ran_in(&observations.unvalued_hits) {
        return (
            Status::Unknown,
            reason_of(
                "values-not-recorded",
                format!(
                    "ran in tests of a merged run measured without the probes, which recorded no values ({})",
                    reason.sentence
                ),
            ),
        );
    }
    (status, reason)
}

/// The outcome records an older runtime's server process does not make,
/// noting each read.
struct Records<'o> {
    observations: &'o Observations,
    read: Cell<bool>,
}

impl Records<'_> {
    fn comparisons(&self, site: &str) -> Option<&HashSet<String>> {
        self.read.set(true);
        self.observations
            .comparisons
            .get(site)
            .filter(|outcomes| !outcomes.is_empty())
    }

    fn logicals(&self, branch: &str) -> Option<&HashSet<(bool, bool)>> {
        self.read.set(true);
        self.observations
            .logicals
            .get(branch)
            .filter(|outcomes| !outcomes.is_empty())
    }

    fn hit(&self, id: &str) -> bool {
        self.observations.hits.contains(id)
    }

    fn vectors(&self, decision: &str) -> &[Vector] {
        self.observations
            .decisions
            .get(decision)
            .map_or(&[], Vec::as_slice)
    }
}

fn reason_of(code: &'static str, sentence: impl Into<String>) -> Reason {
    Reason {
        code,
        sentence: sentence.into(),
    }
}

fn verdict_of(status: Status, code: &'static str, sentence: impl Into<String>) -> (Status, Reason) {
    (status, reason_of(code, sentence))
}

const INFECTED: Status = Status::Infected { proxy: false };
const PROXY: Status = Status::Infected { proxy: true };

/// Code as one short line, quoted.
fn quoted(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let short = if flat.chars().count() > 60 {
        format!("{}…", flat.chars().take(59).collect::<String>())
    } else {
        flat
    };
    format!("`{short}`")
}

/// A binary, logical or assignment mutant's operator and operands.
fn operands(mutant: &CandidateMutant) -> Option<(&str, String, String)> {
    let operator = mutant.operator.as_ref()?;
    let left = mutant.original.get(..operator.at)?;
    let right = mutant.original.get(operator.at + operator.text.len()..)?;
    Some((
        operator.text.as_str(),
        quoted(left.trim()),
        quoted(right.trim()),
    ))
}

fn decide(mutant: &CandidateMutant, records: &Records<'_>) -> (Status, Reason) {
    if let Some(unreached) = reach(mutant, records) {
        return unreached;
    }
    match mutant.mutator.as_str() {
        "StringLiteral" | "BooleanLiteral" | "ArrayDeclaration" | "ObjectLiteral"
        | "UpdateOperator" => verdict_of(
            INFECTED,
            "reached",
            "the code ran, and this replacement differs wherever it is evaluated",
        ),
        BLOCK_STATEMENT | "CallExpression" => {
            if mutant.statement.is_some() {
                verdict_of(
                    PROXY,
                    "statement-ran",
                    "the statement around it ran (a proxy: removing it may change nothing)",
                )
            } else {
                verdict_of(
                    PROXY,
                    "function-ran",
                    "the function ran (a proxy: emptying it may change nothing)",
                )
            }
        }
        "EqualityOperator" => equality_operator(mutant, records),
        "ConditionalExpression" => conditional_expression(mutant, records),
        "OptionalChaining" => optional_chaining(mutant, records),
        "LogicalOperator" => logical_operator(mutant, records),
        "ArithmeticOperator" | "AssignmentOperator" | "MethodExpression" => {
            probed_operation(mutant, records)
        }
        "ArrowFunction" => arrow_function(mutant, records),
        "Regex" => verdict_of(
            Status::Unknown,
            "regex",
            "regular expression mutants are not decided yet",
        ),
        other => verdict_of(
            Status::Unknown,
            "no-rule",
            format!("no rule decides {other} mutants yet"),
        ),
    }
}

/// Whether the mutated code ran: its statement, and its function where the
/// function is not around the statement (a concise arrow's body), must both
/// have been hit. A function body, which no statement contains, is reached
/// by its function.
fn reach(mutant: &CandidateMutant, records: &Records<'_>) -> Option<(Status, Reason)> {
    let statement = mutant.statement.as_deref();
    if statement.is_none() && mutant.mutator != BLOCK_STATEMENT {
        return Some(verdict_of(
            Status::Unknown,
            "no-statement-point",
            "no statement point contains it",
        ));
    }
    if let Some(statement) = statement
        && !records.hit(statement)
    {
        return Some(verdict_of(
            Status::NotReached,
            "statement-not-run",
            "no test ran its statement",
        ));
    }
    match mutant.function.as_deref() {
        Some(function) if !records.hit(function) => Some(verdict_of(
            Status::NotReached,
            "function-not-run",
            "no test called its function",
        )),
        None if statement.is_none() => Some(verdict_of(
            Status::Unknown,
            "no-point",
            "no statement or function point contains it",
        )),
        _ => None,
    }
}

const BOUNDARIES: &[(&str, &str)] = &[("<", "<="), ("<=", "<"), (">", ">="), (">=", ">")];

fn relational(operator: &str) -> bool {
    matches!(operator, "<" | "<=" | ">" | ">=")
}

/// A relational comparison's value for a recorded relation of its operands:
/// `None` for operands the probe could not order (`other`, or anything it
/// does not know).
fn relational_value(operator: &str, relation: &str) -> Option<bool> {
    match relation {
        "lt" => Some(matches!(operator, "<" | "<=")),
        "eq" => Some(matches!(operator, "<=" | ">=")),
        "gt" => Some(matches!(operator, ">" | ">=")),
        // NaN: false for every relational operator.
        "unordered" => Some(false),
        _ => None,
    }
}

fn equality_operator(mutant: &CandidateMutant, records: &Records<'_>) -> (Status, Reason) {
    let Some((operator, left, right)) = operands(mutant) else {
        return verdict_of(Status::Unknown, "no-operator", "its operator is not known");
    };
    let replacement = mutant.replacement.as_str();
    if BOUNDARIES.contains(&(operator, replacement)) {
        let Some(site) = &mutant.compare else {
            return verdict_of(
                Status::Unknown,
                "no-comparison-probe",
                format!("needs {left} == {right}, and no probe recorded the operands"),
            );
        };
        let Some(relations) = records.comparisons(site) else {
            return verdict_of(
                Status::NotReached,
                "comparison-not-evaluated",
                "the comparison never ran",
            );
        };
        if relations.contains("eq") {
            return verdict_of(INFECTED, "operands-equal", "the operands were equal");
        }
        if relations
            .iter()
            .any(|relation| !matches!(relation.as_str(), "lt" | "gt" | "unordered"))
        {
            return verdict_of(
                Status::Unknown,
                "non-primitive-operands",
                "operands the probe could not order",
            );
        }
        return verdict_of(
            Status::NotInfected,
            "operands-never-equal",
            format!("{left} never equal to {right}"),
        );
    }
    // The operator negated: every evaluation differs, but for a relational
    // one whose operands are not ordered (NaN), which is false either way.
    if let Some(site) = &mutant.compare {
        let Some(outcomes) = records.comparisons(site) else {
            return verdict_of(
                Status::NotReached,
                "comparison-not-evaluated",
                "the comparison never ran",
            );
        };
        if !relational(operator) {
            return verdict_of(INFECTED, "comparison-evaluated", "the comparison ran");
        }
        if outcomes
            .iter()
            .any(|relation| matches!(relation.as_str(), "lt" | "eq" | "gt"))
        {
            return verdict_of(
                INFECTED,
                "comparison-evaluated",
                "the comparison ran on ordered operands",
            );
        }
        if outcomes.iter().all(|relation| relation == "unordered") {
            return verdict_of(
                Status::NotInfected,
                "operands-never-ordered",
                format!("{left} and {right} never ordered (always NaN)"),
            );
        }
        return verdict_of(
            Status::Unknown,
            "non-primitive-operands",
            "operands the probe could not order",
        );
    }
    if let Some(decision) = &mutant.decision {
        let vectors = records.vectors(&decision.id);
        return match (decision.condition, decision.prefix) {
            (Some(index), _) => {
                if vectors
                    .iter()
                    .any(|(values, _)| values.get(index).copied().flatten().is_some())
                {
                    verdict_of(INFECTED, "condition-evaluated", "the condition ran")
                } else {
                    verdict_of(
                        Status::NotReached,
                        "condition-not-evaluated",
                        "the condition never ran",
                    )
                }
            }
            (None, None) if !vectors.is_empty() => {
                verdict_of(INFECTED, "decision-evaluated", "the decision ran")
            }
            (None, None) => verdict_of(
                Status::NotReached,
                "decision-not-evaluated",
                "the decision never ran",
            ),
            (None, Some(_)) => verdict_of(
                Status::Unknown,
                "no-rule",
                "a comparison is never several conditions",
            ),
        };
    }
    verdict_of(
        INFECTED,
        "statement-ran",
        "the statement ran (a comparison outside a decision)",
    )
}

/// The truthiness a short-circuit evaluation gives the first conditions of
/// a decision made of `&&` and `||`: the last one evaluated decides.
fn prefix_value(values: &[Option<bool>], width: usize) -> Option<bool> {
    values.iter().take(width).rev().find_map(|value| *value)
}

fn not_infected_unless_value_used(
    mutant: &CandidateMutant,
    code: &'static str,
    sentence: String,
) -> (Status, Reason) {
    if mutant.value_used {
        return verdict_of(
            Status::Unknown,
            "value-used",
            format!("{sentence} in truthiness, but its value is used, not only its truthiness"),
        );
    }
    verdict_of(Status::NotInfected, code, sentence)
}

fn conditional_expression(mutant: &CandidateMutant, records: &Records<'_>) -> (Status, Reason) {
    let forced = match mutant.replacement.as_str() {
        "true" => true,
        "false" => false,
        _ => {
            return verdict_of(
                Status::Unknown,
                "other-form",
                "a switch case emptied or a loop given a false test: no rule yet",
            );
        }
    };
    let original = quoted(&mutant.original);
    if let Some(decision) = &mutant.decision {
        let vectors = records.vectors(&decision.id);
        if vectors.is_empty() {
            return verdict_of(
                Status::NotReached,
                "decision-not-evaluated",
                "the decision never ran",
            );
        }
        let (values, code, sentence) = match (decision.condition, decision.prefix) {
            (Some(index), _) => (
                vectors
                    .iter()
                    .map(|(values, _)| values.get(index).copied().flatten())
                    .filter(Option::is_some)
                    .collect::<HashSet<_>>(),
                "condition-always",
                format!("condition {original} always {forced}"),
            ),
            (None, Some(width)) => (
                vectors
                    .iter()
                    .map(|(values, _)| prefix_value(values, width))
                    .collect(),
                "conditions-always",
                format!("{original} always {forced}"),
            ),
            (None, None) => (
                vectors.iter().map(|(_, outcome)| Some(*outcome)).collect(),
                "decision-always",
                format!("decision always {forced}"),
            ),
        };
        if values.is_empty() {
            return verdict_of(
                Status::NotReached,
                "condition-not-evaluated",
                "the condition never ran",
            );
        }
        if values.contains(&Some(!forced)) {
            return verdict_of(
                INFECTED,
                "opposite-value",
                format!("{original} was {} at least once", !forced),
            );
        }
        if values.contains(&None) {
            return verdict_of(
                Status::Unknown,
                "short-circuited",
                "evaluations that stopped before it",
            );
        }
        return not_infected_unless_value_used(mutant, code, sentence);
    }
    if let Some(site) = &mutant.compare {
        let Some(outcomes) = records.comparisons(site) else {
            return verdict_of(
                Status::NotReached,
                "comparison-not-evaluated",
                "the comparison never ran",
            );
        };
        let operator = mutant
            .operator
            .as_ref()
            .map_or("", |operator| operator.text.as_str());
        let values = outcomes
            .iter()
            .map(|outcome| {
                if relational(operator) {
                    relational_value(operator, outcome)
                } else {
                    match outcome.as_str() {
                        "true" => Some(true),
                        "false" => Some(false),
                        _ => None,
                    }
                }
            })
            .collect::<HashSet<_>>();
        if values.contains(&Some(!forced)) {
            return verdict_of(
                INFECTED,
                "opposite-value",
                format!("{original} was {} at least once", !forced),
            );
        }
        if values.contains(&None) {
            return verdict_of(
                Status::Unknown,
                "non-primitive-operands",
                "operands the probe could not compare",
            );
        }
        return not_infected_unless_value_used(
            mutant,
            "comparison-always",
            format!("{original} always {forced}"),
        );
    }
    if let Some(branch) = &mutant.branch {
        let Some(outcomes) = records.logicals(&branch.id) else {
            return verdict_of(
                Status::NotReached,
                "selection-not-evaluated",
                "the selection never ran",
            );
        };
        let operator = branch.operator.trim_end_matches('=');
        let values = match branch.role {
            BranchRole::Value => outcomes.iter().map(|(_, truthy)| *truthy).collect(),
            // The left operand's truthiness follows from which side gave
            // the value; the right one's is the value's when it ran.
            BranchRole::Left => outcomes
                .iter()
                .map(|(right, truthy)| match operator {
                    "&&" => *right || *truthy,
                    _ => !*right && *truthy,
                })
                .collect(),
            BranchRole::Right => outcomes
                .iter()
                .filter(|(right, _)| *right)
                .map(|(_, truthy)| *truthy)
                .collect::<HashSet<_>>(),
        };
        if values.is_empty() {
            return verdict_of(
                Status::NotReached,
                "operand-not-evaluated",
                "the right operand never ran",
            );
        }
        if values.contains(&!forced) {
            return verdict_of(
                INFECTED,
                "opposite-value",
                format!("{original} was {} at least once", truthy_word(!forced)),
            );
        }
        return not_infected_unless_value_used(
            mutant,
            "operand-always",
            format!("{original} always {}", truthy_word(forced)),
        );
    }
    verdict_of(
        Status::Unknown,
        "no-record",
        "used as a value whose truth no probe recorded",
    )
}

fn truthy_word(value: bool) -> &'static str {
    if value { "truthy" } else { "falsy" }
}

fn optional_chaining(mutant: &CandidateMutant, records: &Records<'_>) -> (Status, Reason) {
    let Some(chain) = &mutant.chain else {
        return verdict_of(
            Status::Unknown,
            "no-chain-point",
            "no optional-chain point at the link",
        );
    };
    if records.hit(&format!("{}:short", chain.id)) {
        return verdict_of(INFECTED, "nullish-seen", "the receiver was nullish");
    }
    if records.hit(&format!("{}:continued", chain.id)) {
        return verdict_of(
            Status::NotInfected,
            "never-nullish",
            format!(
                "the receiver of `{}` in {} never null or undefined",
                mutant.replacement,
                quoted(&mutant.original)
            ),
        );
    }
    // The link never ran: where an earlier link stopped the chain, removing
    // this one's `?` changed nothing.
    if chain
        .earlier
        .iter()
        .any(|earlier| records.hit(&format!("{earlier}:short")))
    {
        return verdict_of(
            Status::NotInfected,
            "earlier-link-nullish",
            "an earlier link of the chain was always nullish",
        );
    }
    verdict_of(
        Status::NotReached,
        "chain-not-evaluated",
        "the chain never ran",
    )
}

fn logical_operator(mutant: &CandidateMutant, records: &Records<'_>) -> (Status, Reason) {
    let Some(operator) = mutant
        .operator
        .as_ref()
        .map(|operator| operator.text.as_str())
    else {
        return verdict_of(Status::Unknown, "no-operator", "its operator is not known");
    };
    let replacement = mutant.replacement.as_str();
    // `a && b` as `a || b` over two conditions: both evaluated, the two
    // differ exactly when the conditions do.
    if let Some(decision) = &mutant.decision {
        let vectors = records.vectors(&decision.id);
        let pair = match (decision.condition, decision.prefix) {
            (None, Some(2)) => true,
            (None, None) => vectors.iter().all(|(values, _)| values.len() == 2),
            _ => false,
        };
        if pair {
            if vectors.is_empty() {
                return verdict_of(
                    Status::NotReached,
                    "decision-not-evaluated",
                    "the decision never ran",
                );
            }
            let mut infected = false;
            let mut short_circuited = false;
            for (values, _) in vectors {
                match (
                    values.first().copied().flatten(),
                    values.get(1).copied().flatten(),
                ) {
                    (Some(left), Some(right)) => infected |= left != right,
                    _ => short_circuited = true,
                }
            }
            if infected {
                return verdict_of(
                    INFECTED,
                    "operands-differ",
                    "one operand was true and the other false",
                );
            }
            if short_circuited {
                return verdict_of(
                    Status::Unknown,
                    "short-circuited",
                    "evaluations that never ran the right operand",
                );
            }
            let (_, left, right) = operands(mutant).unwrap_or_default();
            return not_infected_unless_value_used(
                mutant,
                "operands-agree",
                format!("{left} and {right} never one true and the other false"),
            );
        }
    }
    let Some(branch) = mutant
        .branch
        .as_ref()
        .filter(|branch| branch.role == BranchRole::Value)
    else {
        return verdict_of(
            Status::Unknown,
            "no-selection-point",
            "no logical selection point at the operator",
        );
    };
    let Some(outcomes) = records.logicals(&branch.id) else {
        return verdict_of(
            Status::NotReached,
            "selection-not-evaluated",
            "the selection never ran",
        );
    };
    // Which side gave the value, and its truthiness, imply the left operand
    // where the right one ran.
    let mut infected = false;
    let mut undecided = false;
    for &(right, truthy) in outcomes {
        match (operator, replacement) {
            ("&&", "||") if right && !truthy => infected = true,
            ("||", "&&") if right && truthy => infected = true,
            // A falsy left that is present: both give it.
            ("??", "&&") if !right && !truthy => {}
            ("??", "&&") if right && truthy => infected = true,
            _ => undecided = true,
        }
    }
    if infected {
        return verdict_of(
            INFECTED,
            "operators-differ",
            "an evaluation the two operators give different values",
        );
    }
    if undecided {
        return verdict_of(
            Status::Unknown,
            "not-decisive",
            "operand values that do not decide it",
        );
    }
    let (_, left, _) = operands(mutant).unwrap_or_default();
    verdict_of(
        Status::NotInfected,
        "same-value",
        format!("{left} always present and falsy"),
    )
}

fn probed_operation(mutant: &CandidateMutant, records: &Records<'_>) -> (Status, Reason) {
    let operator = mutant
        .operator
        .as_ref()
        .map_or("", |operator| operator.text.as_str());
    if mutant.mutator == "AssignmentOperator" && matches!(operator, "&&=" | "||=" | "??=") {
        return verdict_of(
            Status::Unknown,
            "logical-assignment",
            "a logical assignment: no rule yet",
        );
    }
    let Some(site) = &mutant.probe else {
        return verdict_of(Status::Unknown, "no-probe", "no probe at the expression");
    };
    let Some(outcomes) = records.comparisons(site) else {
        // A method probe records nothing for a receiver that is not a
        // built-in, so its silence is no sign the call never ran.
        if mutant.mutator == "MethodExpression" {
            return verdict_of(
                Status::Unknown,
                "no-method-record",
                "no record: never called, or on a receiver that is not a built-in",
            );
        }
        return verdict_of(
            Status::NotReached,
            "not-evaluated",
            "the expression never ran",
        );
    };
    if outcomes.contains("differs") {
        return verdict_of(
            INFECTED,
            "replacement-differs",
            "StrykerJS's replacement gave another value",
        );
    }
    if outcomes.iter().all(|outcome| outcome == "same") {
        let replaced = match &mutant.operator {
            Some(operator) => format!(
                "{} and {}",
                quoted(&mutant.original),
                quoted(&format!(
                    "{}{}{}",
                    &mutant.original[..operator.at],
                    mutant.replacement,
                    &mutant.original[operator.at + operator.text.len()..]
                ))
            ),
            None => format!(
                "{} and its `{}`",
                quoted(&mutant.original),
                mutant.replacement
            ),
        };
        return verdict_of(
            Status::NotInfected,
            "replacement-same",
            format!("{replaced} always gave the same value"),
        );
    }
    verdict_of(
        Status::Unknown,
        "undecided",
        "values the probe could not compare",
    )
}

fn arrow_function(mutant: &CandidateMutant, records: &Records<'_>) -> (Status, Reason) {
    if let Some(site) = &mutant.probe
        && let Some(outcomes) = records.comparisons(site)
    {
        if outcomes.contains("defined") {
            return verdict_of(
                INFECTED,
                "returned-value",
                "the arrow returned a value other than undefined",
            );
        }
        return verdict_of(
            Status::NotInfected,
            "always-undefined",
            "the arrow always returned undefined",
        );
    }
    // Without its own function point, nothing says the arrow ran.
    if mutant.function.is_none() {
        return verdict_of(
            Status::Unknown,
            "no-function-point",
            "no function point says the arrow ran",
        );
    }
    match mutant.arrow {
        Some(arrow) if arrow.effects => verdict_of(
            PROXY,
            "effects-ran",
            "the arrow ran a body with effects (a proxy)",
        ),
        Some(arrow) if arrow.never_undefined => verdict_of(
            INFECTED,
            "never-undefined",
            "the arrow ran, and its body is never undefined",
        ),
        _ => verdict_of(
            Status::Unknown,
            "no-value-record",
            "no record of the values the arrow returned",
        ),
    }
}

/// What the tests of `raw_results` observed between them: every hit,
/// decision vector, logical outcome and probe outcome of every snapshot, and
/// of every record a server process wrote. A process outside the test
/// process (a server a browser test drives) writes records, not snapshots;
/// the outcomes among them were read into a snapshot of the test's as its
/// evidence was read. A test whose records do not say they carry values was
/// measured by a runtime that wrote none: its records' hits are also
/// `server_hits`.
pub(crate) fn observations(raw_results: &[RawTestResult]) -> Observations {
    let mut observations = Observations::default();
    let mut vectors = HashMap::<String, HashSet<Vector>>::new();
    let mut vector = |id: &str, vector: &McdcVector| {
        vectors
            .entry(id.to_owned())
            .or_default()
            .insert((vector.values.clone(), vector.outcome));
    };
    for raw in raw_results {
        for snapshot in raw.runtime.iter().chain(&raw.browser) {
            observations.hits.extend(snapshot.hits.iter().cloned());
            for decision in &snapshot.decisions {
                decision
                    .vectors
                    .iter()
                    .for_each(|each| vector(&decision.id, each));
            }
            for event in &snapshot.events {
                match (event.event_type.as_str(), &event.vector) {
                    ("hit", _) => {
                        observations.hits.insert(event.id.clone());
                    }
                    ("decision", Some(each)) => vector(&event.id, each),
                    _ => {}
                }
            }
            for logical in &snapshot.logicals {
                observations
                    .logicals
                    .entry(logical.id.clone())
                    .or_default()
                    .extend(logical.vectors.iter().map(|each| (each.right, each.truthy)));
            }
            for comparison in &snapshot.comparisons {
                observations
                    .comparisons
                    .entry(comparison.id.clone())
                    .or_default()
                    .extend(comparison.outcomes.iter().cloned());
            }
        }
        let valued = raw
            .server
            .iter()
            .any(|record| record.record_type == SERVER_VALUES_RECORD);
        for record in &raw.server {
            match (record.record_type.as_str(), &record.id, &record.vector) {
                ("hit", Some(id), _) => {
                    observations.hits.insert(id.clone());
                    if !valued {
                        observations.server_hits.insert(id.clone());
                    }
                }
                ("decision", Some(id), Some(each)) => vector(id, each),
                _ => {}
            }
        }
    }
    observations.decisions = vectors
        .into_iter()
        .map(|(id, set)| (id, set.into_iter().collect()))
        .collect();
    observations
}

/// Each mutant the run listed, with its verdict from what the tests of
/// `raw_results` observed; `None` when the run listed none (the probes were
/// off, or the run predates the list).
pub(crate) fn results(
    raw_results: &[RawTestResult],
    listed: Option<MutantsFile>,
) -> Option<MutationResults> {
    let listed = listed?;
    let mutant_set = listed.mutant_set?;
    let mut observations = observations(raw_results);
    let without_values = listed.tests_without_values.iter().collect::<HashSet<_>>();
    if !without_values.is_empty() {
        let unvalued = raw_results
            .iter()
            .filter(|raw| {
                raw.test_id
                    .as_ref()
                    .is_some_and(|id| without_values.contains(id))
            })
            .collect::<Vec<_>>();
        for raw in unvalued {
            let hits = self::observations(std::slice::from_ref(raw)).hits;
            observations.unvalued_hits.extend(hits);
        }
    }
    let mut mutants = Vec::new();
    for (file, candidates) in listed.files {
        let mut verdicts = candidates
            .iter()
            .map(|mutant| {
                let (status, reason) = verdict(mutant, &observations);
                IndexedMutant {
                    file: file.clone(),
                    start: mutant.start,
                    end: mutant.end,
                    mutator: mutant.mutator.clone(),
                    descriptor: mutant.replacement.clone(),
                    original: one_line(&mutant.original),
                    replacement: one_line(&replaced(mutant)),
                    status: match status {
                        Status::NotReached => MutantStatus::NotReached,
                        Status::NotInfected => MutantStatus::NotInfected,
                        Status::Infected { .. } => MutantStatus::Infected,
                        Status::Unknown => MutantStatus::Unknown,
                    },
                    proxy: status == PROXY,
                    code: reason.code.to_owned(),
                    reason: reason.sentence,
                }
            })
            .collect::<Vec<_>>();
        // StrykerJS's order within a position.
        verdicts.sort_by_key(|mutant| (mutant.start, mutant.end));
        mutants.extend(verdicts);
    }
    Some(MutationResults {
        mutant_set,
        mutants,
    })
}

/// Code as one line, cut at 120 characters: a block statement's original
/// is a whole function.
fn one_line(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > 120 {
        format!("{}…", flat.chars().take(119).collect::<String>())
    } else {
        flat
    }
}

/// The code with the mutant in place, where it is simply written from the
/// original and the descriptor; otherwise the descriptor.
fn replaced(mutant: &CandidateMutant) -> String {
    let original = mutant.original.as_str();
    let descriptor = mutant.replacement.as_str();
    let spliced = |at: usize, length: usize, with: &str| {
        Some(format!(
            "{}{with}{}",
            original.get(..at)?,
            original.get(at + length..)?
        ))
    };
    let text = match mutant.mutator.as_str() {
        "ArithmeticOperator" | "EqualityOperator" | "LogicalOperator" | "AssignmentOperator" => {
            mutant
                .operator
                .as_ref()
                .and_then(|operator| spliced(operator.at, operator.text.len(), descriptor))
        }
        "UpdateOperator" => ["++", "--"]
            .into_iter()
            .find_map(|operator| original.find(operator))
            .and_then(|at| spliced(at, 2, descriptor)),
        "UnaryOperator" => match descriptor {
            "removed" => original.get(1..).map(str::to_owned),
            operator => spliced(0, 1, operator),
        },
        "BooleanLiteral" => match descriptor {
            "removed" => original.strip_prefix('!').map(str::to_owned),
            literal => Some(literal.to_owned()),
        },
        "ConditionalExpression" if matches!(descriptor, "true" | "false") => {
            Some(descriptor.to_owned())
        }
        "StringLiteral" => Some(match descriptor {
            "empty" => "\"\"".to_owned(),
            text => format!("\"{text}\""),
        }),
        "MethodExpression" => crate::js_mutants::method_replaced(original, descriptor),
        "OptionalChaining" => {
            let (from, to) = match descriptor {
                "()" => ("?.(".to_owned(), "(".to_owned()),
                "[]" => ("?.[".to_owned(), "[".to_owned()),
                name => (format!("?{name}"), name.to_owned()),
            };
            original
                .rfind(&from)
                .and_then(|at| spliced(at, from.len(), &to))
        }
        "ArrowFunction" => Some("() => undefined".to_owned()),
        "BlockStatement" | "ObjectLiteral" => Some("{}".to_owned()),
        "ArrayDeclaration" => Some(descriptor.to_owned()),
        _ => None,
    };
    text.unwrap_or_else(|| descriptor.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::js_mutant_links::{ArrowLink, BranchLink, ChainLink, DecisionLink, MutantOperator};

    /// Every test's snapshots and records are observed together, and only
    /// what a process outside the test process recorded, without saying it
    /// records values, is a server hit.
    #[test]
    fn observations_are_every_snapshot_and_record_and_a_server_s_hits() {
        use crate::coverage_report::{
            ComparisonSnapshot, DecisionSnapshot, LogicalSnapshot, LogicalVector, RuntimeEvent,
            RuntimeSnapshot, ServerRecord,
        };
        let vector = |values: Vec<Option<bool>>, outcome: bool| McdcVector { values, outcome };
        let record = |kind: &str, id: &str, vector: Option<McdcVector>| ServerRecord {
            record_type: kind.into(),
            vector,
            id: Some(id.into()),
            timestamp_ms: None,
            phase_id: None,
            scope: None,
            request: None,
            outcome: None,
        };
        let raw = |runtime: RuntimeSnapshot, browser: Vec<RuntimeSnapshot>, server| {
            let mut raw: RawTestResult = serde_json::from_value(serde_json::json!({
                "test": "t", "provenance": {"runner": "r", "kind": "unit", "source": "s"}
            }))
            .unwrap();
            raw.runtime = vec![runtime];
            raw.browser = browser;
            raw.server = server;
            raw
        };
        let unit = RuntimeSnapshot {
            hits: vec!["s".into()],
            decisions: vec![DecisionSnapshot {
                id: "d".into(),
                vectors: vec![vector(vec![Some(true)], true)],
            }],
            events: vec![RuntimeEvent {
                event_type: "decision".into(),
                id: "d".into(),
                vector: Some(vector(vec![Some(false)], false)),
                timestamp_ms: 0,
                phase_id: None,
                environment: "server".into(),
            }],
            logicals: vec![LogicalSnapshot {
                id: "b".into(),
                vectors: vec![LogicalVector {
                    right: true,
                    truthy: false,
                }],
            }],
            comparisons: vec![ComparisonSnapshot {
                id: "c".into(),
                outcomes: vec!["lt".into()],
            }],
            phase_id: None,
        };
        let page = RuntimeSnapshot {
            events: vec![RuntimeEvent {
                event_type: "hit".into(),
                id: "page".into(),
                vector: None,
                timestamp_ms: 0,
                phase_id: None,
                environment: "browser".into(),
            }],
            comparisons: vec![ComparisonSnapshot {
                id: "c".into(),
                outcomes: vec!["eq".into()],
            }],
            ..RuntimeSnapshot::default()
        };
        // A process that records values says so: its values are in the
        // test's snapshot, and its silence is a snapshot's.
        let valued = RuntimeSnapshot {
            comparisons: vec![ComparisonSnapshot {
                id: "c".into(),
                outcomes: vec!["gt".into()],
            }],
            ..RuntimeSnapshot::default()
        };
        let observed = observations(&[
            raw(unit.clone(), vec![page], vec![]),
            raw(
                unit,
                vec![],
                vec![
                    record("hit", "served", None),
                    record("decision", "d", Some(vector(vec![Some(true)], true))),
                ],
            ),
            raw(
                valued,
                vec![],
                vec![
                    record(SERVER_VALUES_RECORD, "", None),
                    record("hit", "valued", None),
                ],
            ),
        ]);
        let sorted = |set: &HashSet<String>| {
            let mut values = set.iter().cloned().collect::<Vec<_>>();
            values.sort();
            values
        };
        assert_eq!(sorted(&observed.hits), ["page", "s", "served", "valued"]);
        assert_eq!(sorted(&observed.server_hits), ["served"]);
        let mut vectors = observed.decisions["d"].clone();
        vectors.sort();
        assert_eq!(
            vectors,
            [(vec![Some(false)], false), (vec![Some(true)], true)]
        );
        assert_eq!(observed.logicals["b"], HashSet::from([(true, false)]));
        assert_eq!(sorted(&observed.comparisons["c"]), ["eq", "gt", "lt"]);
    }

    /// A mutant's code with it in place, where that is simply written.
    #[test]
    fn a_replacement_is_written_from_the_original_where_it_can_be() {
        let cases = [
            (operated("EqualityOperator", "<=", "a < b", "<"), "a <= b"),
            (operated("LogicalOperator", "||", "a && b", "&&"), "a || b"),
            (mutant("UpdateOperator", "--", "i++"), "i--"),
            (mutant("UnaryOperator", "+", "-x"), "+x"),
            (mutant("UnaryOperator", "removed", "~x"), "x"),
            (mutant("BooleanLiteral", "removed", "!ok"), "ok"),
            (mutant("BooleanLiteral", "false", "true"), "false"),
            (mutant("ConditionalExpression", "true", "a > b"), "true"),
            (
                mutant("ConditionalExpression", "empty", "case 1: x();"),
                "empty",
            ),
            (mutant("StringLiteral", "empty", "\"a\""), "\"\""),
            (
                mutant("StringLiteral", "Stryker was here!", "\"\""),
                "\"Stryker was here!\"",
            ),
            (mutant("MethodExpression", "removed", "name.trim()"), "name"),
            (
                mutant("MethodExpression", "endsWith", "s.startsWith(\"a.b(\")"),
                "s.endsWith(\"a.b(\")",
            ),
            (
                mutant("OptionalChaining", ".name", "a?.b?.name"),
                "a?.b.name",
            ),
            (mutant("OptionalChaining", "()", "f?.()"), "f()"),
            (mutant("OptionalChaining", "[]", "a?.[k]"), "a[k]"),
            (
                mutant("ArrowFunction", "undefined", "() => 1"),
                "() => undefined",
            ),
            (mutant("BlockStatement", "{}", "{ x(); }"), "{}"),
            (mutant("ArrayDeclaration", "[]", "[1, 2]"), "[]"),
            (mutant("CallExpression", "empty", "x();"), "empty"),
        ];
        for (mutant, written) in cases {
            assert_eq!(replaced(&mutant), written, "{}", mutant.id);
        }
        assert_eq!(one_line("{\n  a();\n  b();\n}"), "{ a(); b(); }");
        assert_eq!(one_line(&"x".repeat(200)).chars().count(), 120);
    }

    /// A mutant of `original` on line 1, in statement `s` of function `f`.
    fn mutant(mutator: &str, replacement: &str, original: &str) -> CandidateMutant {
        let end = original.len() as u32 + 1;
        CandidateMutant {
            id: format!("a.ts:1:1-1:{end}:{mutator}:{replacement}"),
            mutator: mutator.into(),
            replacement: replacement.into(),
            start: (1, 1),
            end: (1, end),
            original: original.into(),
            operator: None,
            value_used: false,
            statement: Some("s".into()),
            function: Some("f".into()),
            decision: None,
            branch: None,
            chain: None,
            compare: None,
            probe: None,
            arrow: None,
        }
    }

    /// The same, with its operator: the first occurrence of `operator`.
    fn operated(
        mutator: &str,
        replacement: &str,
        original: &str,
        operator: &str,
    ) -> CandidateMutant {
        CandidateMutant {
            operator: Some(MutantOperator {
                text: operator.into(),
                at: original.find(&format!(" {operator} ")).unwrap() + 1,
            }),
            ..mutant(mutator, replacement, original)
        }
    }

    fn decided(id: &str, condition: Option<usize>, prefix: Option<usize>) -> Option<DecisionLink> {
        Some(DecisionLink {
            id: id.into(),
            condition,
            prefix,
        })
    }

    fn selection(role: BranchRole, operator: &str) -> Option<BranchLink> {
        Some(BranchLink {
            id: "b".into(),
            role,
            operator: operator.into(),
        })
    }

    /// The statement and function ran.
    fn ran() -> Observations {
        let mut observations = Observations::default();
        observations.hits.extend(["s".to_owned(), "f".to_owned()]);
        observations
    }

    fn with_vectors(vectors: &[(&[Option<bool>], bool)]) -> Observations {
        let mut observations = ran();
        observations.decisions.insert(
            "d".into(),
            vectors
                .iter()
                .map(|(values, outcome)| (values.to_vec(), *outcome))
                .collect(),
        );
        observations
    }

    fn with_outcomes(site: &str, outcomes: &[&str]) -> Observations {
        let mut observations = ran();
        observations.comparisons.insert(
            site.into(),
            outcomes
                .iter()
                .map(|outcome| (*outcome).to_owned())
                .collect(),
        );
        observations
    }

    fn with_logicals(outcomes: &[(bool, bool)]) -> Observations {
        let mut observations = ran();
        observations
            .logicals
            .insert("b".into(), outcomes.iter().copied().collect());
        observations
    }

    #[track_caller]
    fn expect(mutant: &CandidateMutant, observations: &Observations, status: Status, code: &str) {
        let (found, reason) = verdict(mutant, observations);
        assert_eq!((found, reason.code), (status, code), "{reason:?}");
    }

    const NOT_REACHED: Status = Status::NotReached;
    const NOT_INFECTED: Status = Status::NotInfected;
    const UNKNOWN: Status = Status::Unknown;

    #[test]
    fn reach_comes_from_the_statement_and_the_function() {
        let literal = mutant("StringLiteral", "empty", "\"x\"");
        expect(&literal, &ran(), INFECTED, "reached");
        expect(
            &literal,
            &Observations::default(),
            NOT_REACHED,
            "statement-not-run",
        );
        // A concise arrow's statement ran when the arrow was made; the arrow
        // itself never ran.
        let mut only_statement = Observations::default();
        only_statement.hits.insert("s".into());
        expect(&literal, &only_statement, NOT_REACHED, "function-not-run");
        let outside = CandidateMutant {
            statement: None,
            ..literal.clone()
        };
        expect(&outside, &ran(), UNKNOWN, "no-statement-point");
        for mutator in [
            "BooleanLiteral",
            "ArrayDeclaration",
            "ObjectLiteral",
            "UpdateOperator",
        ] {
            expect(&mutant(mutator, "x", "x"), &ran(), INFECTED, "reached");
        }
        expect(
            &mutant("UnaryOperator", "-", "+a"),
            &ran(),
            UNKNOWN,
            "no-rule",
        );
        expect(&mutant("Regex", "", "/a/"), &ran(), UNKNOWN, "regex");
    }

    #[test]
    fn removed_blocks_and_calls_are_infected_by_proxy() {
        let call = mutant("CallExpression", "empty", "f();");
        expect(&call, &ran(), PROXY, "statement-ran");
        // A function body is in no statement: its function decides.
        let body = CandidateMutant {
            statement: None,
            ..mutant("BlockStatement", "{}", "{ f(); }")
        };
        expect(&body, &ran(), PROXY, "function-ran");
        expect(
            &body,
            &Observations::default(),
            NOT_REACHED,
            "function-not-run",
        );
        let nowhere = CandidateMutant {
            function: None,
            ..body
        };
        expect(&nowhere, &ran(), UNKNOWN, "no-point");
    }

    #[test]
    fn a_boundary_needs_equal_operands() {
        let boundary = CandidateMutant {
            compare: Some("c".into()),
            ..operated("EqualityOperator", "<=", "a < b", "<")
        };
        expect(
            &boundary,
            &with_outcomes("c", &["lt", "eq"]),
            INFECTED,
            "operands-equal",
        );
        let (status, reason) = verdict(&boundary, &with_outcomes("c", &["lt", "gt"]));
        assert_eq!(status, NOT_INFECTED);
        assert_eq!(reason.sentence, "`a` never equal to `b`");
        // NaN is unequal to everything: `a < b` and `a <= b` are both false.
        expect(
            &boundary,
            &with_outcomes("c", &["unordered"]),
            NOT_INFECTED,
            "operands-never-equal",
        );
        expect(
            &boundary,
            &with_outcomes("c", &["lt", "other"]),
            UNKNOWN,
            "non-primitive-operands",
        );
        expect(&boundary, &ran(), NOT_REACHED, "comparison-not-evaluated");
        let unprobed = CandidateMutant {
            compare: None,
            ..boundary
        };
        expect(
            &unprobed,
            &with_outcomes("c", &["eq"]),
            UNKNOWN,
            "no-comparison-probe",
        );
    }

    #[test]
    fn a_negated_comparison_differs_wherever_it_runs_on_ordered_operands() {
        let negated = CandidateMutant {
            compare: Some("c".into()),
            ..operated("EqualityOperator", ">=", "a < b", "<")
        };
        expect(
            &negated,
            &with_outcomes("c", &["gt"]),
            INFECTED,
            "comparison-evaluated",
        );
        expect(
            &negated,
            &with_outcomes("c", &["unordered"]),
            NOT_INFECTED,
            "operands-never-ordered",
        );
        expect(
            &negated,
            &with_outcomes("c", &["other"]),
            UNKNOWN,
            "non-primitive-operands",
        );
        expect(&negated, &ran(), NOT_REACHED, "comparison-not-evaluated");
        // An equality's value recorded where it is a value.
        let equality = CandidateMutant {
            compare: Some("c".into()),
            ..operated("EqualityOperator", "!==", "a === b", "===")
        };
        expect(
            &equality,
            &with_outcomes("c", &["false"]),
            INFECTED,
            "comparison-evaluated",
        );
        // Without a probe, the decision says whether it ran.
        let condition = CandidateMutant {
            decision: decided("d", Some(1), None),
            ..operated("EqualityOperator", "!==", "a === b", "===")
        };
        expect(
            &condition,
            &with_vectors(&[(&[Some(true), Some(false)], false)]),
            INFECTED,
            "condition-evaluated",
        );
        expect(
            &condition,
            &with_vectors(&[(&[Some(false), None], false)]),
            NOT_REACHED,
            "condition-not-evaluated",
        );
        let whole = CandidateMutant {
            decision: decided("d", None, None),
            ..condition.clone()
        };
        expect(
            &whole,
            &with_vectors(&[(&[Some(true)], true)]),
            INFECTED,
            "decision-evaluated",
        );
        expect(&whole, &ran(), NOT_REACHED, "decision-not-evaluated");
        let elsewhere = operated("EqualityOperator", "!==", "a === b", "===");
        expect(&elsewhere, &ran(), INFECTED, "statement-ran");
    }

    #[test]
    fn a_forced_decision_condition_or_prefix_needs_the_other_value() {
        let whole = CandidateMutant {
            decision: decided("d", None, None),
            ..mutant("ConditionalExpression", "true", "a && b")
        };
        expect(
            &whole,
            &with_vectors(&[
                (&[Some(true), Some(true)], true),
                (&[Some(false), None], false),
            ]),
            INFECTED,
            "opposite-value",
        );
        let (status, reason) = verdict(&whole, &with_vectors(&[(&[Some(true), Some(true)], true)]));
        assert_eq!(status, NOT_INFECTED);
        assert_eq!(reason.sentence, "decision always true");
        expect(&whole, &ran(), NOT_REACHED, "decision-not-evaluated");

        let condition = CandidateMutant {
            decision: decided("d", Some(1), None),
            ..mutant("ConditionalExpression", "true", "b")
        };
        // Where `a` was false, `b` never ran: the mutant changed nothing.
        expect(
            &condition,
            &with_vectors(&[
                (&[Some(false), None], false),
                (&[Some(true), Some(true)], true),
            ]),
            NOT_INFECTED,
            "condition-always",
        );
        expect(
            &condition,
            &with_vectors(&[(&[Some(true), Some(false)], false)]),
            INFECTED,
            "opposite-value",
        );
        expect(
            &condition,
            &with_vectors(&[(&[Some(false), None], false)]),
            NOT_REACHED,
            "condition-not-evaluated",
        );

        // `(a || b) && c`: the prefix is the last of its conditions run.
        let prefix = CandidateMutant {
            decision: decided("d", None, Some(2)),
            ..mutant("ConditionalExpression", "true", "a || b")
        };
        expect(
            &prefix,
            &with_vectors(&[
                (&[Some(true), None, Some(true)], true),
                (&[Some(false), Some(true), Some(false)], false),
            ]),
            NOT_INFECTED,
            "conditions-always",
        );
        expect(
            &prefix,
            &with_vectors(&[(&[Some(false), Some(false), None], false)]),
            INFECTED,
            "opposite-value",
        );
        // A vector that recorded neither condition cannot say.
        expect(
            &prefix,
            &with_vectors(&[(&[None, None, Some(true)], true)]),
            UNKNOWN,
            "short-circuited",
        );
    }

    #[test]
    fn a_truthiness_that_agrees_does_not_decide_a_value_that_is_used() {
        let kept = CandidateMutant {
            decision: decided("d", None, None),
            value_used: true,
            ..mutant("ConditionalExpression", "true", "a && b")
        };
        expect(
            &kept,
            &with_vectors(&[(&[Some(true), Some(true)], true)]),
            UNKNOWN,
            "value-used",
        );
        expect(
            &kept,
            &with_vectors(&[(&[Some(false), None], false)]),
            INFECTED,
            "opposite-value",
        );
    }

    #[test]
    fn a_forced_comparison_used_as_a_value_reads_its_probe() {
        let equality = CandidateMutant {
            compare: Some("c".into()),
            ..operated("ConditionalExpression", "false", "a === b", "===")
        };
        expect(
            &equality,
            &with_outcomes("c", &["true"]),
            INFECTED,
            "opposite-value",
        );
        let (status, reason) = verdict(&equality, &with_outcomes("c", &["false"]));
        assert_eq!(status, NOT_INFECTED);
        assert_eq!(reason.sentence, "`a === b` always false");
        let relational = CandidateMutant {
            compare: Some("c".into()),
            ..operated("ConditionalExpression", "false", "a <= b", "<=")
        };
        expect(
            &relational,
            &with_outcomes("c", &["eq"]),
            INFECTED,
            "opposite-value",
        );
        // NaN makes every relational comparison false.
        expect(
            &relational,
            &with_outcomes("c", &["gt", "unordered"]),
            NOT_INFECTED,
            "comparison-always",
        );
        expect(
            &relational,
            &with_outcomes("c", &["other"]),
            UNKNOWN,
            "non-primitive-operands",
        );
        expect(&relational, &ran(), NOT_REACHED, "comparison-not-evaluated");
        expect(
            &mutant("ConditionalExpression", "true", "a > b"),
            &ran(),
            UNKNOWN,
            "no-record",
        );
        expect(
            &mutant("ConditionalExpression", "empty", "case 1: f();"),
            &ran(),
            UNKNOWN,
            "other-form",
        );
    }

    #[test]
    fn a_forced_logical_value_or_operand_reads_its_selection() {
        let value = CandidateMutant {
            branch: selection(BranchRole::Value, "&&"),
            ..mutant("ConditionalExpression", "true", "a > 0 && b > 0")
        };
        expect(
            &value,
            &with_logicals(&[(false, false)]),
            INFECTED,
            "opposite-value",
        );
        expect(
            &value,
            &with_logicals(&[(true, true)]),
            NOT_INFECTED,
            "operand-always",
        );
        expect(&value, &ran(), NOT_REACHED, "selection-not-evaluated");
        let kept = CandidateMutant {
            value_used: true,
            ..value
        };
        expect(
            &kept,
            &with_logicals(&[(true, true)]),
            UNKNOWN,
            "value-used",
        );

        // `a > 0 && x`: the left operand was truthy whenever the right ran.
        let left = CandidateMutant {
            branch: selection(BranchRole::Left, "&&"),
            ..mutant("ConditionalExpression", "true", "a > 0")
        };
        expect(
            &left,
            &with_logicals(&[(true, false), (true, true)]),
            NOT_INFECTED,
            "operand-always",
        );
        expect(
            &left,
            &with_logicals(&[(false, false)]),
            INFECTED,
            "opposite-value",
        );
        let left_of_or = CandidateMutant {
            branch: selection(BranchRole::Left, "||"),
            ..mutant("ConditionalExpression", "false", "a > 0")
        };
        expect(
            &left_of_or,
            &with_logicals(&[(true, true)]),
            NOT_INFECTED,
            "operand-always",
        );
        expect(
            &left_of_or,
            &with_logicals(&[(false, true)]),
            INFECTED,
            "opposite-value",
        );
        // `x ?? (a > 0)`: the left side's truthiness is known too.
        let coalesced = CandidateMutant {
            branch: selection(BranchRole::Left, "??"),
            ..mutant("ConditionalExpression", "true", "a > 0")
        };
        expect(
            &coalesced,
            &with_logicals(&[(true, true)]),
            INFECTED,
            "opposite-value",
        );

        let right = CandidateMutant {
            branch: selection(BranchRole::Right, "??"),
            ..mutant("ConditionalExpression", "true", "a > 0")
        };
        expect(
            &right,
            &with_logicals(&[(true, true), (false, false)]),
            NOT_INFECTED,
            "operand-always",
        );
        expect(
            &right,
            &with_logicals(&[(true, false)]),
            INFECTED,
            "opposite-value",
        );
        // The right side never ran: neither did the mutant.
        expect(
            &right,
            &with_logicals(&[(false, true)]),
            NOT_REACHED,
            "operand-not-evaluated",
        );
    }

    #[test]
    fn an_optional_link_is_infected_by_a_nullish_receiver() {
        let link = CandidateMutant {
            chain: Some(ChainLink {
                id: "o".into(),
                earlier: vec!["e".into()],
            }),
            ..mutant("OptionalChaining", ".b", "x?.a?.b")
        };
        let hits = |ids: &[&str]| {
            let mut observations = ran();
            observations
                .hits
                .extend(ids.iter().map(|id| (*id).to_owned()));
            observations
        };
        expect(
            &link,
            &hits(&["o:short", "o:continued"]),
            INFECTED,
            "nullish-seen",
        );
        let (status, reason) = verdict(&link, &hits(&["o:continued"]));
        assert_eq!(status, NOT_INFECTED);
        assert_eq!(
            reason.sentence,
            "the receiver of `.b` in `x?.a?.b` never null or undefined"
        );
        expect(
            &link,
            &hits(&["e:short"]),
            NOT_INFECTED,
            "earlier-link-nullish",
        );
        expect(
            &link,
            &hits(&["e:continued"]),
            NOT_REACHED,
            "chain-not-evaluated",
        );
        let unlinked = mutant("OptionalChaining", ".b", "x?.b");
        expect(&unlinked, &ran(), UNKNOWN, "no-chain-point");
        // Hits are recorded in a server process too: nothing to downgrade.
        let mut server = hits(&["o:continued"]);
        server.server_hits.insert("s".into());
        expect(&link, &server, NOT_INFECTED, "never-nullish");
    }

    #[test]
    fn two_conditions_swap_their_operator_where_they_differ() {
        let swapped = CandidateMutant {
            decision: decided("d", None, None),
            ..operated("LogicalOperator", "||", "a && b", "&&")
        };
        expect(
            &swapped,
            &with_vectors(&[(&[Some(true), Some(false)], false)]),
            INFECTED,
            "operands-differ",
        );
        let (status, reason) = verdict(
            &swapped,
            &with_vectors(&[(&[Some(true), Some(true)], true)]),
        );
        assert_eq!(status, NOT_INFECTED);
        assert_eq!(
            reason.sentence,
            "`a` and `b` never one true and the other false"
        );
        // `a` false: the original never ran `b`, the mutant would have.
        expect(
            &swapped,
            &with_vectors(&[
                (&[Some(false), None], false),
                (&[Some(true), Some(true)], true),
            ]),
            UNKNOWN,
            "short-circuited",
        );
        expect(&swapped, &ran(), NOT_REACHED, "decision-not-evaluated");
        let kept = CandidateMutant {
            value_used: true,
            ..swapped.clone()
        };
        expect(
            &kept,
            &with_vectors(&[(&[Some(true), Some(true)], true)]),
            UNKNOWN,
            "value-used",
        );
        // The first two of more conditions.
        let prefix = CandidateMutant {
            decision: decided("d", None, Some(2)),
            ..swapped.clone()
        };
        expect(
            &prefix,
            &with_vectors(&[(&[Some(false), Some(true), Some(true)], true)]),
            INFECTED,
            "operands-differ",
        );
        // The whole of three conditions is no pair.
        expect(
            &swapped,
            &with_vectors(&[(&[Some(true), Some(true), Some(true)], true)]),
            UNKNOWN,
            "no-selection-point",
        );
    }

    #[test]
    fn a_swapped_logical_value_follows_its_outcomes() {
        let case = |operator: &str, replacement: &str| CandidateMutant {
            branch: selection(BranchRole::Value, operator),
            ..operated(
                "LogicalOperator",
                replacement,
                &format!("a {operator} b"),
                operator,
            )
        };
        let and = case("&&", "||");
        expect(
            &and,
            &with_logicals(&[(true, false)]),
            INFECTED,
            "operators-differ",
        );
        expect(
            &and,
            &with_logicals(&[(true, true), (false, false)]),
            UNKNOWN,
            "not-decisive",
        );
        expect(&and, &ran(), NOT_REACHED, "selection-not-evaluated");
        let or = case("||", "&&");
        expect(
            &or,
            &with_logicals(&[(true, true)]),
            INFECTED,
            "operators-differ",
        );
        expect(
            &or,
            &with_logicals(&[(false, true)]),
            UNKNOWN,
            "not-decisive",
        );
        let coalesce = case("??", "&&");
        expect(
            &coalesce,
            &with_logicals(&[(false, false)]),
            NOT_INFECTED,
            "same-value",
        );
        expect(
            &coalesce,
            &with_logicals(&[(true, true)]),
            INFECTED,
            "operators-differ",
        );
        expect(
            &coalesce,
            &with_logicals(&[(false, false), (false, true)]),
            UNKNOWN,
            "not-decisive",
        );
        expect(
            &operated("LogicalOperator", "||", "a && b", "&&"),
            &ran(),
            UNKNOWN,
            "no-selection-point",
        );
    }

    #[test]
    fn arithmetic_assignment_and_method_probes_compare_the_replacement() {
        let arithmetic = CandidateMutant {
            probe: Some("p".into()),
            ..operated("ArithmeticOperator", "-", "a + b", "+")
        };
        expect(
            &arithmetic,
            &with_outcomes("p", &["same", "differs"]),
            INFECTED,
            "replacement-differs",
        );
        let (status, reason) = verdict(&arithmetic, &with_outcomes("p", &["same"]));
        assert_eq!(status, NOT_INFECTED);
        assert_eq!(
            reason.sentence,
            "`a + b` and `a - b` always gave the same value"
        );
        expect(
            &arithmetic,
            &with_outcomes("p", &["same", "unknown"]),
            UNKNOWN,
            "undecided",
        );
        // A probe placed and no record: the expression never ran.
        expect(&arithmetic, &ran(), NOT_REACHED, "not-evaluated");
        let unprobed = CandidateMutant {
            probe: None,
            ..arithmetic.clone()
        };
        expect(&unprobed, &ran(), UNKNOWN, "no-probe");
        let assignment = CandidateMutant {
            probe: Some("p".into()),
            ..operated("AssignmentOperator", "-=", "x += 1", "+=")
        };
        expect(
            &assignment,
            &with_outcomes("p", &["differs"]),
            INFECTED,
            "replacement-differs",
        );
        let logical = operated("AssignmentOperator", "&&=", "x ||= y", "||=");
        expect(&logical, &ran(), UNKNOWN, "logical-assignment");
        // A method on a receiver that is not a built-in records nothing.
        let method = CandidateMutant {
            probe: Some("p".into()),
            ..mutant("MethodExpression", "every", "xs.some(f)")
        };
        expect(&method, &ran(), UNKNOWN, "no-method-record");
        let (status, reason) = verdict(&method, &with_outcomes("p", &["same"]));
        assert_eq!(status, NOT_INFECTED);
        assert_eq!(
            reason.sentence,
            "`xs.some(f)` and its `every` always gave the same value"
        );
    }

    #[test]
    fn an_arrow_is_decided_by_its_values_or_by_its_body() {
        let probed = CandidateMutant {
            probe: Some("p".into()),
            arrow: Some(ArrowLink {
                effects: false,
                never_undefined: false,
            }),
            ..mutant("ArrowFunction", "undefined", "(x) => x.id")
        };
        expect(
            &probed,
            &with_outcomes("p", &["undefined", "defined"]),
            INFECTED,
            "returned-value",
        );
        expect(
            &probed,
            &with_outcomes("p", &["undefined"]),
            NOT_INFECTED,
            "always-undefined",
        );
        expect(&probed, &ran(), UNKNOWN, "no-value-record");
        let effects = CandidateMutant {
            probe: None,
            arrow: Some(ArrowLink {
                effects: true,
                never_undefined: false,
            }),
            ..probed.clone()
        };
        expect(&effects, &ran(), PROXY, "effects-ran");
        let defined = CandidateMutant {
            probe: None,
            arrow: Some(ArrowLink {
                effects: false,
                never_undefined: true,
            }),
            ..probed.clone()
        };
        expect(&defined, &ran(), INFECTED, "never-undefined");
        let mut only_statement = Observations::default();
        only_statement.hits.insert("s".into());
        expect(&defined, &only_statement, NOT_REACHED, "function-not-run");
        let pointless = CandidateMutant {
            function: None,
            ..defined
        };
        expect(&pointless, &ran(), UNKNOWN, "no-function-point");
    }

    #[test]
    fn a_verdict_from_outcomes_a_server_process_does_not_record_is_unknown_there() {
        let mut server = ran();
        server.server_hits.insert("s".into());
        let boundary = CandidateMutant {
            compare: Some("c".into()),
            ..operated("EqualityOperator", "<=", "a < b", "<")
        };
        // No record, and outcomes that say "never equal": both may be the
        // server's silence.
        expect(&boundary, &server, UNKNOWN, "server-process");
        let mut never_equal = with_outcomes("c", &["lt"]);
        never_equal.server_hits.insert("s".into());
        expect(&boundary, &never_equal, UNKNOWN, "server-process");
        // An infection seen is seen.
        let mut equal = with_outcomes("c", &["eq"]);
        equal.server_hits.insert("s".into());
        expect(&boundary, &equal, INFECTED, "operands-equal");
        let arithmetic = CandidateMutant {
            probe: Some("p".into()),
            ..operated("ArithmeticOperator", "-", "a + b", "+")
        };
        expect(&arithmetic, &server, UNKNOWN, "server-process");
        let selection = CandidateMutant {
            branch: selection(BranchRole::Value, "&&"),
            ..mutant("ConditionalExpression", "true", "a > 0 && b > 0")
        };
        expect(&selection, &server, UNKNOWN, "server-process");
        // Decision vectors are recorded there: a verdict from them stands.
        let mut vectors = with_vectors(&[(&[Some(true), Some(true)], true)]);
        vectors.server_hits.insert("s".into());
        let decision = CandidateMutant {
            decision: decided("d", None, None),
            ..mutant("ConditionalExpression", "true", "a && b")
        };
        expect(&decision, &vectors, NOT_INFECTED, "decision-always");
        // A statement never run is not reached wherever the code lives.
        let mut elsewhere = Observations::default();
        elsewhere.server_hits.insert("t".into());
        expect(&boundary, &elsewhere, NOT_REACHED, "statement-not-run");
    }

    /// A server process that records values is read as a test process is:
    /// its hits are no `server_hits`, and its silence decides.
    #[test]
    fn a_verdict_from_a_server_process_that_records_values_stands() {
        use crate::coverage_report::{ComparisonSnapshot, RuntimeSnapshot, ServerRecord};
        let record = |kind: &str, id: &str| ServerRecord {
            record_type: kind.into(),
            vector: None,
            id: Some(id.into()),
            timestamp_ms: None,
            phase_id: None,
            scope: None,
            request: None,
            outcome: None,
        };
        let served = |values: bool, outcomes: &[&str]| {
            let mut raw: RawTestResult = serde_json::from_value(serde_json::json!({
                "test": "t", "provenance": {"runner": "r", "kind": "e2e", "source": "s"}
            }))
            .unwrap();
            raw.server = vec![record("hit", "s"), record("hit", "f")];
            if values {
                raw.server.insert(0, record(SERVER_VALUES_RECORD, ""));
                raw.runtime = vec![RuntimeSnapshot {
                    comparisons: vec![ComparisonSnapshot {
                        id: "c".into(),
                        outcomes: outcomes.iter().map(|outcome| (*outcome).into()).collect(),
                    }],
                    ..RuntimeSnapshot::default()
                }];
            }
            observations(&[raw])
        };
        let boundary = CandidateMutant {
            compare: Some("c".into()),
            ..operated("EqualityOperator", "<=", "a < b", "<")
        };
        expect(
            &boundary,
            &served(true, &["lt"]),
            NOT_INFECTED,
            "operands-never-equal",
        );
        expect(
            &boundary,
            &served(true, &["eq"]),
            INFECTED,
            "operands-equal",
        );
        // No record of the comparison from a process that records values:
        // it never ran there.
        expect(
            &boundary,
            &served(true, &[]),
            NOT_REACHED,
            "comparison-not-evaluated",
        );
        // An older runtime's process: its silence is unknown.
        expect(&boundary, &served(false, &[]), UNKNOWN, "server-process");
    }

    /// A merged run's tests measured without the probes ran code and
    /// recorded no values: what their silence would decide is unknown.
    #[test]
    fn a_verdict_from_values_tests_measured_without_the_probes_lack_is_unknown() {
        let boundary = CandidateMutant {
            compare: Some("c".into()),
            ..operated("EqualityOperator", "<=", "a < b", "<")
        };
        let mut unvalued = ran();
        unvalued.unvalued_hits.insert("s".into());
        expect(&boundary, &unvalued, UNKNOWN, "values-not-recorded");
        let mut never_equal = with_outcomes("c", &["lt"]);
        never_equal.unvalued_hits.insert("s".into());
        expect(&boundary, &never_equal, UNKNOWN, "values-not-recorded");
        let mut equal = with_outcomes("c", &["eq"]);
        equal.unvalued_hits.insert("s".into());
        expect(&boundary, &equal, INFECTED, "operands-equal");
        // Only where those tests ran it.
        let mut elsewhere = with_outcomes("c", &["lt"]);
        elsewhere.unvalued_hits.insert("t".into());
        expect(&boundary, &elsewhere, NOT_INFECTED, "operands-never-equal");
    }
}
