//! Reviewed MC/DC waiver parsing and evaluation.
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

use crate::coverage_analysis::serialize_javascript_number;
use crate::coverage_report::DecisionResult;

pub const WAIVERS_FILE: &str = "supercov.waivers.json";
pub const WAIVERS_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageWaiver {
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    pub condition: String,
    pub reason: String,
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
    pub applied_by_file: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContradictedWaiver {
    pub file: String,
    pub line: usize,
    pub condition: String,
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
    pub mcdc_excluding_waived: McdcExcludingWaived,
}

impl CoverageWaiverEvaluation {
    pub fn summary(&self, covered: usize, total: usize) -> CoverageWaiverSummary {
        let adjusted_total = total.saturating_sub(self.applied.len());
        CoverageWaiverSummary {
            file: WAIVERS_FILE.into(),
            entries: self.waivers.len(),
            applied: self.applied.len(),
            contradicted: self
                .contradicted
                .iter()
                .map(|matched| ContradictedWaiver {
                    file: matched.file.clone(),
                    line: matched.line,
                    condition: matched.condition_source.clone(),
                    reason: matched.waiver.reason.clone(),
                })
                .collect(),
            unmatched: self.unmatched.clone(),
            mcdc_excluding_waived: McdcExcludingWaived {
                covered,
                total: adjusted_total,
                percentage: if adjusted_total > 0 {
                    covered as f64 / adjusted_total as f64 * 100.0
                } else {
                    100.0
                },
            },
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
    let condition = nonempty_string(object, "condition")
        .ok_or_else(|| entry_problem(index, "requires a non-empty condition"))?;
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
    let line = match object.get("line") {
        None => None,
        Some(value) => value
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .filter(|value| *value > 0)
            .ok_or_else(|| entry_problem(index, "has a non-positive line"))?
            .into(),
    };
    if positional_condition(condition).is_some() && decision.is_none() {
        return Err(entry_problem(
            index,
            format!("uses the positional condition {condition} without a decision"),
        ));
    }
    Ok(CoverageWaiver {
        file: file.into(),
        decision,
        line,
        condition: condition.into(),
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
        applied_by_file: BTreeMap::new(),
    };
    for waiver in &source.waivers {
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
                    decision: Some("ready && enabled".into()),
                    line: None,
                    condition: "enabled".into(),
                    reason: "source".into(),
                },
                CoverageWaiver {
                    file: "src/example.ts".into(),
                    decision: Some("decision-1".into()),
                    line: Some(12),
                    condition: "C1".into(),
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
            decision: Some("decision-1".into()),
            line: None,
            condition: "C1".into(),
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
}
