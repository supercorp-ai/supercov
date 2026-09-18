//! Named code properties, asked as yes/no questions and composed in code.
//!
//! This is a different instrument from the rubric in `rubric.json`. The rubric
//! asks the model to place a file on a scale, which ties with counting
//! statements. These ask whether a specific, named, checkable property is
//! present, and the arithmetic that turns twelve answers into one number lives
//! here rather than in the model, so every part of the score is a claim a reader
//! can check against the file in seconds.
//!
//! Evidence, on 272 Java classes carrying professional maintainability ratings:
//! the composite orders size-matched pairs 78% correctly against CodeScene Code
//! Health's 67%, a statement count's 58% and the Maintainability Index's 52%.
//! Repeated, a check moves by a median of 0.01 and the composite by 0.5% of its
//! range. Asked about a change instead of a file, eight of eight deliberately
//! introduced smells were detected, seven of eight ranked first, with one false
//! alarm in 312 control questions.
//!
//! Two limits belong next to the number. The composite still correlates 0.92
//! with a statement count, so it is largely a size measure whose residual is
//! right. And six checks score as well as twelve, because these are not twelve
//! measurements but one measurement taken twelve times.

use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::sync::OnceLock;

/// Bumped whenever a check's wording changes, so a saved answer is never read
/// as an answer to a question that was not asked.
pub const CATALOG_VERSION: &str = "smells-v2";

const CATALOG: &str = include_str!("smells.json");

/// Risks a change can introduce, which the complexity catalog cannot express.
///
/// A [benchmark of 173 comments real reviewers wrote] found the complexity
/// catalog has a word for 8% of them: the rest are bugs, concurrency, security
/// and api. These seven are aimed at that gap and, on constructed positives with
/// matched safe changes, they separate by 0.91 or more where the complexity
/// checks separate by far less and co-fire constantly. They are asked only of a
/// change, never of a file, because every one of them is about what a change did.
///
/// They are not a bug finder. 94 of those 173 comments are bugs and none of
/// these asks about correctness.
const RISKS: &str = include_str!("risks.json");

/// How a change question is framed. Both versions go in one request, because
/// scoring each alone and subtracting hits a ceiling: a file the model already
/// half-believes is guilty has no room left to rise.
const CHANGE_PREFIX: &str = "Two versions of one file are given, `before` and `after`. \
Answer only about what changed: does `after` show the following where `before` did not? ";
/// A risk check already asks about the change, so it needs the framing without
/// the "where `before` did not" clause the complexity questions require.
const CHANGE_PREFIX_PLAIN: &str = "Two versions of one file are given, `before` and `after`. \
Answer only about what changed: ";
const CHANGE_PRESENT: &str = "`after` shows it and `before` did not.";
const CHANGE_ABSENT: &str =
    "`before` already showed it, or neither version does, or `after` no longer does.";

#[derive(Debug, Deserialize)]
pub struct Check {
    pub id: String,
    pub task: String,
    pub present: String,
    pub absent: String,
    /// What is known about this check's accuracy, shown on request rather than
    /// left in a commit message.
    pub evidence: String,
}

pub fn catalog() -> &'static [Check] {
    static PARSED: OnceLock<Vec<Check>> = OnceLock::new();
    PARSED.get_or_init(|| {
        serde_json::from_str(CATALOG).expect("smells.json ships with the binary and parses")
    })
}

pub fn risks() -> &'static [Check] {
    static PARSED: OnceLock<Vec<Check>> = OnceLock::new();
    PARSED.get_or_init(|| {
        serde_json::from_str(RISKS).expect("risks.json ships with the binary and parses")
    })
}

fn noul(task: String, present: &str, absent: &str) -> Value {
    json!({
        "type": "noul",
        "instructions": { "task": task },
        "criteria": { "true": present, "false": absent },
    })
}

/// Every check, asked about one file as it stands.
pub fn file_questions() -> Map<String, Value> {
    catalog()
        .iter()
        .map(|c| (c.id.clone(), noul(c.task.clone(), &c.present, &c.absent)))
        .collect()
}

/// Every check, asked about what a change introduced: the complexity catalog in
/// its differential form, and the risk checks, which only exist in this form.
pub fn change_questions() -> Map<String, Value> {
    let mut questions: Map<String, Value> = catalog()
        .iter()
        .map(|c| {
            (
                c.id.clone(),
                noul(
                    format!("{CHANGE_PREFIX}{}", c.task),
                    CHANGE_PRESENT,
                    CHANGE_ABSENT,
                ),
            )
        })
        .collect();
    for risk in risks() {
        questions.insert(
            risk.id.clone(),
            noul(
                format!("{CHANGE_PREFIX_PLAIN}{}", risk.task),
                &risk.present,
                &risk.absent,
            ),
        );
    }
    questions
}

/// The catalog as a report carries it: what each check asks, and what is known
/// about how well it answers. A finding is only checkable if the question and
/// its evidence travel with it.
pub fn described() -> Value {
    catalog()
        .iter()
        .chain(risks())
        .map(|c| json!({ "check": c.id, "asks": c.present, "evidence": c.evidence }))
        .collect()
}

/// Ask which of these paths the repository actually ships.
///
/// A convention is per-path and cannot see the tree. The model sees all of it in
/// one request, which is the whole difference: `runtime/python/supercov_runtime.py`
/// is unclassifiable alone and obvious beside `runtime/javascript/`,
/// `runtime/ruby/` and a manifest that ships one of them.
///
/// Measured on six repositories in six languages: 99.1% agreement with the
/// conventions on the 1,503 files they decide, and on the 115 they cannot it
/// separated a shipped runtime tree from prototype crates exactly. Paths only,
/// no contents, so it costs a fraction of a cent.
pub fn scope_request(repository: &str, tree: &str, manifests: &str, asking: &[String]) -> Value {
    const TASK: &str = "The complete list of source files in this repository is in state.tree, \
and the manifests it declares are in state.manifests. Is the file named in this question part of \
the product this repository ships or runs in production, rather than one of: a test, a fixture, a \
build or tooling script, an example or demo, documentation, a benchmark, generated output, or a \
prototype the project does not ship? Judge it in the context of the whole tree.";
    let questions: Map<String, Value> = asking
        .iter()
        .enumerate()
        .map(|(index, path)| {
            (
                format!("f{index}"),
                noul(
                    format!("{TASK}\n\nThe file: {path}"),
                    "It is part of the product.",
                    "It is a test, tooling, an example, documentation, a benchmark, \
                     generated output or a prototype.",
                ),
            )
        })
        .collect();
    json!({
        "model": super::MODEL,
        "state": { "repository": repository, "tree": tree, "manifests": manifests },
        "questions": questions,
    })
}

pub fn file_request(path: &str, source: &str) -> Value {
    json!({
        "model": super::MODEL,
        "state": { "file": { "path": path, "source": source } },
        "questions": file_questions(),
    })
}

/// The fallback when two whole versions do not fit: the unified diff alone.
///
/// A patch cannot separate a property the change introduced from one the file
/// already had, which is the reason whole files are sent at all. On one real
/// change it scored 0.83 where both versions scored 0.82, so it is a usable
/// second choice and a great deal better than refusing to review the file. It
/// is reported as a different kind of answer, never silently substituted.
pub fn patch_request(patch: &str) -> Value {
    json!({
        "model": super::MODEL,
        "state": { "diff": patch },
        "questions": change_questions(),
    })
}

/// State is exactly `before` and `after` and nothing else.
///
/// The path is deliberately left out. Wrapping the two versions under a `file`
/// key and adding the path measurably weakened detection in testing: a real
/// five-level nest came back at 0.37 with the wrapper and 0.5 or better
/// without. The shape that was validated is the shape that ships.
pub fn change_request(before: &str, after: &str) -> Value {
    json!({
        "model": super::MODEL,
        "state": { "before": before, "after": after },
        "questions": change_questions(),
    })
}

/// A check is reported as present at or above this value.
///
/// Chosen against three references rather than taken as the midpoint, which is
/// what 0.5 was. On 240 judgements a blind reader and the model both made, 0.60
/// agrees with the reader 80.8% of the time against 79.2% at 0.50, which is
/// inside the noise on that many judgements. The other two references are not
/// noisy and both point the same way: across 272 classes it reports 2.2
/// properties per file rather than 2.8, and on eight deliberately introduced
/// smells it still catches all eight while co-firing on unrelated checks falls
/// from 12% to 7%. Nothing favoured 0.50.
///
/// The composite does not use this. Health is the mean of the raw answers, so a
/// threshold decides only what is shown, never what is scored.
pub const PRESENT_AT: f64 = 0.6;

/// Health on a 0 to 10 scale, where 10 is a file no check fires on.
///
/// The mean rather than the sum, so the number means the same thing if the
/// catalog gains or loses a check. Ranking is identical either way, and the sum
/// is what the evidence above was measured on.
pub fn health(values: &BTreeMap<String, f64>) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mean = values.values().sum::<f64>() / values.len() as f64;
    Some(((1.0 - mean) * 10.0 * 100.0).round() / 100.0)
}

/// Which checks fired, strongest first.
pub fn present(values: &BTreeMap<String, f64>) -> Vec<(String, f64)> {
    let mut fired: Vec<_> = values
        .iter()
        .filter(|(_, v)| **v >= PRESENT_AT)
        .map(|(k, v)| (k.clone(), *v))
        .collect();
    fired.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    fired
}

/// Health across many files, weighted by size.
///
/// A plain average would let a directory of one-line re-exports outvote the
/// file everything depends on. Weighting by bytes says the score is about the
/// code, not about how it happens to be split into files. This is arithmetic
/// done here and is not a judgment the model was asked for; the per-file
/// numbers it combines are.
pub fn aggregate(files: &[(u64, f64)]) -> Option<f64> {
    let total: u64 = files.iter().map(|(bytes, _)| *bytes).sum();
    if total == 0 {
        return None;
    }
    let weighted: f64 = files
        .iter()
        .map(|(bytes, health)| *bytes as f64 * health)
        .sum();
    Some((weighted / total as f64 * 100.0).round() / 100.0)
}
