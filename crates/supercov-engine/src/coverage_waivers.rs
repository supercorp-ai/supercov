//! Reviewed coverage-obligation waiver parsing and evaluation.
//!
//! Waivers are mutable project policy, never evidence. They are evaluated at
//! query time and therefore must not affect the immutable evidence index or
//! any measured raw total.

use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::coverage_analysis::{CoverageCount, CoverageSummary, serialize_javascript_number};
use crate::coverage_report::DecisionResult;

pub const WAIVERS_FILE: &str = "supercov.waivers.json";
pub const WAIVERS_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageWaiver {
    pub file: String,
    #[serde(default = "default_waiver_kind", skip_serializing_if = "is_mcdc_kind")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<usize>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub condition: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alternative: Option<String>,
    pub reason: String,
}

fn default_waiver_kind() -> String {
    "mcdc".into()
}

fn is_mcdc_kind(kind: &String) -> bool {
    kind == "mcdc"
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverageWaiverSource {
    pub path: PathBuf,
    pub waivers: Vec<CoverageWaiver>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageWaiverMatch {
    pub waiver: CoverageWaiver,
    pub decision_id: String,
    pub file: String,
    pub line: usize,
    pub condition_index: usize,
    pub condition_source: String,
    pub covered: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverageWaiverEvaluation {
    pub path: PathBuf,
    pub waivers: Vec<CoverageWaiver>,
    pub applied: Vec<CoverageWaiverMatch>,
    pub contradicted: Vec<CoverageWaiverMatch>,
    pub unmatched: Vec<CoverageWaiver>,
    pub waived_by_decision: BTreeMap<String, BTreeMap<usize, CoverageWaiver>>,
    pub waived_lines: BTreeMap<String, CoverageWaiver>,
    pub waived_hits: BTreeMap<String, CoverageWaiver>,
    pub applied_obligations: Vec<CoverageObligationWaiverMatch>,
    pub contradicted_obligations: Vec<CoverageObligationWaiverMatch>,
    pub applied_by_file: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageObligationWaiverMatch {
    pub waiver: CoverageWaiver,
    pub id: String,
    pub kind: String,
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub source: String,
    pub covered: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverageWaiverObligation {
    pub id: String,
    pub kind: String,
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub source: String,
    pub alternative: Option<String>,
    pub covered: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContradictedWaiver {
    pub kind: String,
    pub file: String,
    pub line: usize,
    pub obligation: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McdcExcludingWaived {
    pub covered: usize,
    pub total: usize,
    #[serde(serialize_with = "serialize_javascript_number")]
    pub percentage: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageWaiverSummary {
    pub file: String,
    pub entries: usize,
    pub applied: usize,
    pub contradicted: Vec<ContradictedWaiver>,
    pub unmatched: Vec<CoverageWaiver>,
    pub complete: bool,
    pub coverage_excluding_waived: CoverageExcludingWaived,
    pub mcdc_excluding_waived: McdcExcludingWaived,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CoverageExcludingWaived {
    pub lines: CoverageCount,
    pub statements: CoverageCount,
    pub functions: CoverageCount,
    pub branches: CoverageCount,
    pub mcdc: McdcExcludingWaived,
}

fn adjusted_count(count: &CoverageCount, waived: usize) -> CoverageCount {
    let total = count.total.saturating_sub(waived);
    CoverageCount {
        covered: count.covered,
        total,
        percentage: if total > 0 {
            count.covered as f64 / total as f64 * 100.0
        } else {
            100.0
        },
    }
}

impl CoverageWaiverEvaluation {
    pub fn summary(&self, coverage: &CoverageSummary) -> CoverageWaiverSummary {
        let adjusted_total = coverage.conditions.saturating_sub(self.applied.len());
        let waived_kind = |kind: &str| {
            self.applied_obligations
                .iter()
                .filter(|matched| matched.kind == kind)
                .count()
        };
        let mcdc = McdcExcludingWaived {
            covered: coverage.covered_conditions,
            total: adjusted_total,
            percentage: if adjusted_total > 0 {
                coverage.covered_conditions as f64 / adjusted_total as f64 * 100.0
            } else {
                100.0
            },
        };
        let adjusted = CoverageExcludingWaived {
            lines: adjusted_count(&coverage.lines, waived_kind("line")),
            statements: adjusted_count(&coverage.statements, waived_kind("statement")),
            functions: adjusted_count(&coverage.functions, waived_kind("function")),
            branches: adjusted_count(&coverage.branches, waived_kind("branch")),
            mcdc: mcdc.clone(),
        };
        CoverageWaiverSummary {
            file: WAIVERS_FILE.into(),
            entries: self.waivers.len(),
            applied: self.applied.len() + self.applied_obligations.len(),
            contradicted: self
                .contradicted
                .iter()
                .map(|matched| ContradictedWaiver {
                    kind: "mcdc".into(),
                    file: matched.file.clone(),
                    line: matched.line,
                    obligation: matched.condition_source.clone(),
                    reason: matched.waiver.reason.clone(),
                })
                .chain(
                    self.contradicted_obligations
                        .iter()
                        .map(|matched| ContradictedWaiver {
                            kind: matched.kind.clone(),
                            file: matched.file.clone(),
                            line: matched.line,
                            obligation: matched.source.clone(),
                            reason: matched.waiver.reason.clone(),
                        }),
                )
                .collect(),
            unmatched: self.unmatched.clone(),
            complete: adjusted.lines.covered == adjusted.lines.total
                && adjusted.statements.covered == adjusted.statements.total
                && adjusted.functions.covered == adjusted.functions.total
                && adjusted.branches.covered == adjusted.branches.total
                && adjusted.mcdc.covered == adjusted.mcdc.total,
            coverage_excluding_waived: adjusted,
            mcdc_excluding_waived: mcdc,
        }
    }
}

#[derive(Debug)]
pub enum WaiverError {
    Io { path: PathBuf, source: io::Error },
    InvalidJson(serde_json::Error),
    InvalidShape,
    InvalidEntry { index: usize, problem: String },
}

impl std::fmt::Display for WaiverError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::InvalidJson(error) => {
                write!(formatter, "{WAIVERS_FILE} is not valid JSON: {error}")
            }
            Self::InvalidShape => write!(
                formatter,
                "{WAIVERS_FILE} must be {{\"version\": 1, \"waivers\": [...]}}"
            ),
            Self::InvalidEntry { index, problem } => {
                write!(formatter, "{WAIVERS_FILE} waiver {} {problem}", index + 1)
            }
        }
    }
}

impl std::error::Error for WaiverError {}

fn entry_problem(index: usize, problem: impl Into<String>) -> WaiverError {
    WaiverError::InvalidEntry {
        index,
        problem: problem.into(),
    }
}

fn nonempty_string<'a>(object: &'a serde_json::Map<String, Value>, field: &str) -> Option<&'a str> {
    object
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
}

fn parse_waiver(value: &Value, index: usize) -> Result<CoverageWaiver, WaiverError> {
    let object = value
        .as_object()
        .ok_or_else(|| entry_problem(index, "requires a non-empty file"))?;
    let file = nonempty_string(object, "file")
        .ok_or_else(|| entry_problem(index, "requires a non-empty file"))?;
    let kind = object
        .get("kind")
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| entry_problem(index, "has a non-string kind"))
        })
        .transpose()?
        .unwrap_or("mcdc");
    if !matches!(kind, "mcdc" | "line" | "statement" | "function" | "branch") {
        return Err(entry_problem(index, format!("has unsupported kind {kind}")));
    }
    let condition = nonempty_string(object, "condition").unwrap_or_default();
    let reason = object
        .get("reason")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| entry_problem(index, "requires a non-empty reason"))?;
    let decision = match object.get("decision") {
        None => None,
        Some(Value::String(value)) => Some(value.clone()),
        Some(_) => return Err(entry_problem(index, "has a non-string decision")),
    };
    let id = match object.get("id") {
        None => None,
        Some(Value::String(value)) if !value.is_empty() => Some(value.clone()),
        Some(_) => return Err(entry_problem(index, "has an invalid id")),
    };
    let line = match object.get("line") {
        None => None,
        Some(value) => value
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .filter(|value| *value > 0)
            .ok_or_else(|| entry_problem(index, "has a non-positive line"))?
            .into(),
    };
    let column = match object.get("column") {
        None => None,
        Some(value) => value
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| entry_problem(index, "has an invalid column"))?
            .into(),
    };
    let source = match object.get("source") {
        None => None,
        Some(Value::String(value)) if !value.trim().is_empty() => Some(value.clone()),
        Some(_) => return Err(entry_problem(index, "has an invalid source")),
    };
    let alternative = match object.get("alternative") {
        None => None,
        Some(Value::String(value)) if !value.trim().is_empty() => Some(value.clone()),
        Some(_) => return Err(entry_problem(index, "has an invalid alternative")),
    };
    if kind == "mcdc" && condition.is_empty() {
        return Err(entry_problem(index, "requires a non-empty condition"));
    }
    if kind != "mcdc" && (!condition.is_empty() || decision.is_some()) {
        return Err(entry_problem(
            index,
            "uses MC/DC selectors for a non-MC/DC obligation",
        ));
    }
    if kind == "line" && line.is_none() {
        return Err(entry_problem(
            index,
            "requires a positive line for kind line",
        ));
    }
    if matches!(kind, "statement" | "function" | "branch")
        && id.is_none()
        && (line.is_none() || column.is_none())
    {
        return Err(entry_problem(
            index,
            format!("requires id or line plus column for kind {kind}"),
        ));
    }
    if positional_condition(condition).is_some() && decision.is_none() {
        return Err(entry_problem(
            index,
            format!("uses the positional condition {condition} without a decision"),
        ));
    }
    Ok(CoverageWaiver {
        file: file.into(),
        kind: kind.into(),
        id,
        decision,
        line,
        column,
        condition: condition.into(),
        source,
        alternative,
        reason: reason.into(),
    })
}

pub fn read_coverage_waivers(root: &Path) -> Result<Option<CoverageWaiverSource>, WaiverError> {
    let path = root.join(WAIVERS_FILE);
    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(WaiverError::Io { path, source }),
    };
    let parsed: Value = serde_json::from_str(&raw).map_err(WaiverError::InvalidJson)?;
    let object = parsed.as_object().ok_or(WaiverError::InvalidShape)?;
    if object.get("version").and_then(Value::as_u64) != Some(WAIVERS_SCHEMA_VERSION.into()) {
        return Err(WaiverError::InvalidShape);
    }
    let values = object
        .get("waivers")
        .and_then(Value::as_array)
        .ok_or(WaiverError::InvalidShape)?;
    let waivers = values
        .iter()
        .enumerate()
        .map(|(index, value)| parse_waiver(value, index))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(CoverageWaiverSource { path, waivers }))
}

fn positional_condition(value: &str) -> Option<usize> {
    let digits = value.strip_prefix('C')?;
    (!digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| digits.parse::<usize>().ok())
        .flatten()
}

fn ecmascript_whitespace(character: char) -> bool {
    matches!(
        character,
        '\u{0009}'..='\u{000d}'
            | '\u{0020}'
            | '\u{00a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
            | '\u{feff}'
    )
}

fn normalized_source(source: &str) -> String {
    let mut normalized = String::new();
    let mut pending_space = false;
    for character in source.chars() {
        if ecmascript_whitespace(character) {
            pending_space = !normalized.is_empty();
        } else {
            if pending_space {
                normalized.push(' ');
                pending_space = false;
            }
            normalized.push(character);
        }
    }
    normalized
}

pub fn evaluate_coverage_waivers(
    decisions: &[DecisionResult],
    source: &CoverageWaiverSource,
) -> CoverageWaiverEvaluation {
    let mut evaluation = CoverageWaiverEvaluation {
        path: source.path.clone(),
        waivers: source.waivers.clone(),
        applied: Vec::new(),
        contradicted: Vec::new(),
        unmatched: Vec::new(),
        waived_by_decision: BTreeMap::new(),
        waived_lines: BTreeMap::new(),
        waived_hits: BTreeMap::new(),
        applied_obligations: Vec::new(),
        contradicted_obligations: Vec::new(),
        applied_by_file: BTreeMap::new(),
    };
    for waiver in &source.waivers {
        if waiver.kind != "mcdc" {
            continue;
        }
        let mut matches = Vec::new();
        for decision in decisions {
            if decision.meta.file != waiver.file
                || waiver.line.is_some_and(|line| line != decision.meta.line)
            {
                continue;
            }
            if let Some(selector) = &waiver.decision
                && decision.meta.id != *selector
                && normalized_source(&decision.meta.source) != normalized_source(selector)
            {
                continue;
            }
            for condition in &decision.conditions {
                let positional = format!("C{}", condition.index + 1);
                if waiver.condition != positional
                    && normalized_source(&condition.source) != normalized_source(&waiver.condition)
                {
                    continue;
                }
                matches.push(CoverageWaiverMatch {
                    waiver: waiver.clone(),
                    decision_id: decision.meta.id.clone(),
                    file: decision.meta.file.clone(),
                    line: decision.meta.line,
                    condition_index: condition.index,
                    condition_source: condition.source.clone(),
                    covered: condition.covered,
                });
            }
        }
        if matches.is_empty() {
            evaluation.unmatched.push(waiver.clone());
            continue;
        }
        for matched in matches {
            if matched.covered {
                evaluation.contradicted.push(matched);
                continue;
            }
            let conditions = evaluation
                .waived_by_decision
                .entry(matched.decision_id.clone())
                .or_default();
            if conditions.contains_key(&matched.condition_index) {
                continue;
            }
            conditions.insert(matched.condition_index, matched.waiver.clone());
            *evaluation
                .applied_by_file
                .entry(matched.file.clone())
                .or_default() += 1;
            evaluation.applied.push(matched);
        }
    }
    evaluation
}

/// Add line, statement, function, and branch policy matches to an MC/DC
/// evaluation. Raw coverage remains immutable; these maps only annotate query
/// results and produce the explicitly labelled policy-adjusted view.
pub fn evaluate_obligation_waivers(
    evaluation: &mut CoverageWaiverEvaluation,
    obligations: &[CoverageWaiverObligation],
) {
    for waiver in evaluation
        .waivers
        .iter()
        .filter(|waiver| waiver.kind != "mcdc")
    {
        let matches = obligations
            .iter()
            .filter(|obligation| {
                obligation.kind == waiver.kind
                    && obligation.file == waiver.file
                    && waiver.id.as_ref().is_none_or(|id| id == &obligation.id)
                    && waiver.line.is_none_or(|line| line == obligation.line)
                    && waiver
                        .column
                        .is_none_or(|column| column == obligation.column)
                    && waiver.source.as_ref().is_none_or(|source| {
                        normalized_source(source) == normalized_source(&obligation.source)
                    })
                    && waiver.alternative.as_ref().is_none_or(|alternative| {
                        obligation.alternative.as_ref() == Some(alternative)
                    })
            })
            .collect::<Vec<_>>();
        if matches.is_empty() {
            evaluation.unmatched.push(waiver.clone());
            continue;
        }
        for obligation in matches {
            let matched = CoverageObligationWaiverMatch {
                waiver: waiver.clone(),
                id: obligation.id.clone(),
                kind: obligation.kind.clone(),
                file: obligation.file.clone(),
                line: obligation.line,
                column: obligation.column,
                source: obligation.source.clone(),
                covered: obligation.covered,
            };
            if obligation.covered {
                evaluation.contradicted_obligations.push(matched);
                continue;
            }
            let inserted = if obligation.kind == "line" {
                let key = obligation.id.clone();
                match evaluation.waived_lines.entry(key) {
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        entry.insert(waiver.clone());
                        true
                    }
                    std::collections::btree_map::Entry::Occupied(_) => false,
                }
            } else {
                match evaluation.waived_hits.entry(obligation.id.clone()) {
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        entry.insert(waiver.clone());
                        true
                    }
                    std::collections::btree_map::Entry::Occupied(_) => false,
                }
            };
            if inserted {
                *evaluation
                    .applied_by_file
                    .entry(obligation.file.clone())
                    .or_default() += 1;
                evaluation.applied_obligations.push(matched);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    use crate::coverage_report::{CoverageConfidence, DecisionMeta};

    use super::*;

    static SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn directory() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "supercov-waivers-{}-{nonce}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn decision(covered: [bool; 2]) -> DecisionResult {
        DecisionResult {
            meta: DecisionMeta {
                id: "decision-1".into(),
                file: "src/example.ts".into(),
                line: 12,
                column: 3,
                source: "ready\n &&\u{feff} enabled".into(),
                conditions: vec!["ready".into(), "enabled".into()],
                kind: "logical-and".into(),
            },
            executed: true,
            covered: covered.into_iter().all(|value| value),
            vectors: Vec::new(),
            vector_observations: Vec::new(),
            conditions: ["ready", "enabled"]
                .into_iter()
                .enumerate()
                .map(|(index, source)| crate::coverage_report::ConditionResult {
                    index,
                    source: source.into(),
                    covered: covered[index],
                    assertion_covered: false,
                    witness: None,
                    witness_tests: None,
                })
                .collect(),
            tests: Vec::new(),
            confidence: CoverageConfidence {
                level: "unexecuted".into(),
                setup_only: false,
                background_only: false,
                asserted: false,
                tests: Vec::new(),
                asserted_tests: Vec::new(),
                runners: Vec::new(),
                kinds: Vec::new(),
                e2e: false,
            },
        }
    }

    #[test]
    fn absence_is_distinct_from_malformed_policy() {
        let root = directory();
        assert!(read_coverage_waivers(&root).unwrap().is_none());
        fs::write(root.join(WAIVERS_FILE), "{").unwrap();
        assert!(matches!(
            read_coverage_waivers(&root),
            Err(WaiverError::InvalidJson(_))
        ));
        fs::write(root.join(WAIVERS_FILE), r#"{"version":2,"waivers":[]}"#).unwrap();
        assert!(matches!(
            read_coverage_waivers(&root),
            Err(WaiverError::InvalidShape)
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn validates_each_frozen_entry_rule() {
        let root = directory();
        for (entry, problem) in [
            (
                r#"{"condition":"ready","reason":"why"}"#,
                "requires a non-empty file",
            ),
            (
                r#"{"file":"x","reason":"why"}"#,
                "requires a non-empty condition",
            ),
            (
                r#"{"file":"x","condition":"ready","reason":" "}"#,
                "requires a non-empty reason",
            ),
            (
                r#"{"file":"x","condition":"C1","reason":"why"}"#,
                "uses the positional condition C1 without a decision",
            ),
        ] {
            fs::write(
                root.join(WAIVERS_FILE),
                format!(r#"{{"version":1,"waivers":[{entry}]}}"#),
            )
            .unwrap();
            assert_eq!(
                read_coverage_waivers(&root).unwrap_err().to_string(),
                format!("{WAIVERS_FILE} waiver 1 {problem}")
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn matches_id_ecmascript_whitespace_source_and_position() {
        let source = CoverageWaiverSource {
            path: PathBuf::from(WAIVERS_FILE),
            waivers: vec![
                CoverageWaiver {
                    file: "src/example.ts".into(),
                    kind: "mcdc".into(),
                    id: None,
                    decision: Some("ready && enabled".into()),
                    line: None,
                    column: None,
                    condition: "enabled".into(),
                    source: None,
                    alternative: None,
                    reason: "source".into(),
                },
                CoverageWaiver {
                    file: "src/example.ts".into(),
                    kind: "mcdc".into(),
                    id: None,
                    decision: Some("decision-1".into()),
                    line: Some(12),
                    column: None,
                    condition: "C1".into(),
                    source: None,
                    alternative: None,
                    reason: "position".into(),
                },
            ],
        };
        let evaluation = evaluate_coverage_waivers(&[decision([false, true])], &source);
        assert_eq!(evaluation.applied.len(), 1);
        assert_eq!(evaluation.applied[0].condition_index, 0);
        assert_eq!(evaluation.contradicted.len(), 1);
        assert_eq!(evaluation.contradicted[0].condition_index, 1);
        assert!(evaluation.unmatched.is_empty());
    }

    #[test]
    fn first_uncovered_waiver_owns_annotation_and_unknowns_remain_visible() {
        let first = CoverageWaiver {
            file: "src/example.ts".into(),
            kind: "mcdc".into(),
            id: None,
            decision: Some("decision-1".into()),
            line: None,
            column: None,
            condition: "C1".into(),
            source: None,
            alternative: None,
            reason: "first".into(),
        };
        let mut duplicate = first.clone();
        duplicate.reason = "second".into();
        let mut unknown = first.clone();
        unknown.condition = "C9".into();
        let source = CoverageWaiverSource {
            path: PathBuf::from(WAIVERS_FILE),
            waivers: vec![first, duplicate, unknown.clone()],
        };
        let evaluation = evaluate_coverage_waivers(&[decision([false, true])], &source);
        assert_eq!(evaluation.applied.len(), 1);
        assert_eq!(evaluation.applied[0].waiver.reason, "first");
        assert_eq!(evaluation.unmatched, [unknown]);
        assert_eq!(evaluation.applied_by_file["src/example.ts"], 1);
    }

    #[test]
    fn line_and_statement_waivers_are_separate_reviewed_policy() {
        let source = CoverageWaiverSource {
            path: PathBuf::from(WAIVERS_FILE),
            waivers: vec![
                CoverageWaiver {
                    file: "src/example.ts".into(),
                    kind: "line".into(),
                    id: None,
                    decision: None,
                    line: Some(20),
                    column: None,
                    condition: String::new(),
                    source: None,
                    alternative: None,
                    reason: "unreachable by construction".into(),
                },
                CoverageWaiver {
                    file: "src/example.ts".into(),
                    kind: "statement".into(),
                    id: Some("statement-1".into()),
                    decision: None,
                    line: None,
                    column: None,
                    condition: String::new(),
                    source: None,
                    alternative: None,
                    reason: "upstream contract excludes it".into(),
                },
            ],
        };
        let mut evaluation = evaluate_coverage_waivers(&[], &source);
        evaluate_obligation_waivers(
            &mut evaluation,
            &[
                CoverageWaiverObligation {
                    id: "line:src/example.ts:20".into(),
                    kind: "line".into(),
                    file: "src/example.ts".into(),
                    line: 20,
                    column: 0,
                    source: String::new(),
                    alternative: None,
                    covered: false,
                },
                CoverageWaiverObligation {
                    id: "statement-1".into(),
                    kind: "statement".into(),
                    file: "src/example.ts".into(),
                    line: 21,
                    column: 4,
                    source: "unreachable()".into(),
                    alternative: None,
                    covered: true,
                },
            ],
        );
        assert_eq!(evaluation.applied_obligations.len(), 1);
        assert!(
            evaluation
                .waived_lines
                .contains_key("line:src/example.ts:20")
        );
        assert_eq!(evaluation.contradicted_obligations.len(), 1);
        assert!(evaluation.unmatched.is_empty());
    }

    #[test]
    fn non_mcdc_waivers_require_unambiguous_selectors() {
        let root = directory();
        fs::write(
            root.join(WAIVERS_FILE),
            r#"{"version":1,"waivers":[{"kind":"statement","file":"x.ts","line":4,"reason":"why"}]}"#,
        )
        .unwrap();
        assert_eq!(
            read_coverage_waivers(&root).unwrap_err().to_string(),
            format!("{WAIVERS_FILE} waiver 1 requires id or line plus column for kind statement")
        );
        fs::remove_dir_all(root).unwrap();
    }
}
