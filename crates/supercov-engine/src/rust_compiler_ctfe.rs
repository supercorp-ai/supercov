//! Strict reconstruction of compiler-owned constant-evaluation evidence.
//!
//! rustc's interpreter step events expose no stable parent span. The exact
//! companion therefore inserts entry/exit markers and records the compiler
//! thread. This module reconstructs a nested invocation stack per compiler
//! process and thread before any observation is allowed to cover an
//! obligation.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{
    coverage_analysis::McdcVector,
    coverage_report::{DecisionSnapshot, RuntimeEvent, RuntimeSnapshot},
    rust_compiler_manifest::NormalizedRustCompilerManifest,
};

const MAP_SCHEMA: &str = "supercov-rust-ctfe-map-v1";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CtfeMapFile {
    schema: String,
    #[serde(rename = "crate")]
    crate_name: String,
    mappings: Vec<CtfeMapping>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CtfeMapping {
    marker: String,
    definition: String,
    observation_kind: String,
    ordinal: u32,
    hit_ordinals: Vec<String>,
    decision: Option<CtfeDecisionMapping>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CtfeDecisionMapping {
    id: String,
    event: String,
    condition_index: Option<u64>,
    value: Option<bool>,
    outcome: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CtfeEvent {
    #[serde(rename = "crate")]
    crate_name: String,
    kind: String,
    marker: String,
    definition: String,
    observation_kind: String,
    ordinal: u32,
    thread: String,
}

#[derive(Debug)]
struct ActiveDecision {
    id: String,
    values: Vec<Option<bool>>,
}

#[derive(Debug)]
struct ActiveInvocation {
    definition: String,
    decisions: Vec<ActiveDecision>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RustCompilerCtfeUnit {
    pub identity: String,
    pub crate_name: String,
    pub snapshot: RuntimeSnapshot,
    pub observations: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RustCompilerCtfeError {
    Io { path: PathBuf, reason: String },
    Invalid(String),
}

impl std::fmt::Display for RustCompilerCtfeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, reason } => write!(formatter, "{}: {reason}", path.display()),
            Self::Invalid(reason) => write!(formatter, "invalid Rust CTFE evidence: {reason}"),
        }
    }
}

impl std::error::Error for RustCompilerCtfeError {}

fn io_error(path: &Path, error: impl std::fmt::Display) -> RustCompilerCtfeError {
    RustCompilerCtfeError::Io {
        path: path.to_path_buf(),
        reason: error.to_string(),
    }
}

fn parse_u64(value: &str, context: &str) -> Result<u64, RustCompilerCtfeError> {
    if value.is_empty() || (value.len() > 1 && value.starts_with('0')) {
        return Err(RustCompilerCtfeError::Invalid(format!(
            "{context} is not canonical unsigned decimal"
        )));
    }
    value.parse::<u64>().map_err(|_| {
        RustCompilerCtfeError::Invalid(format!("{context} is not canonical unsigned decimal"))
    })
}

fn parse_json<T: for<'de> Deserialize<'de>>(
    path: &Path,
    bytes: &[u8],
) -> Result<T, RustCompilerCtfeError> {
    serde_json::from_slice(bytes)
        .map_err(|error| RustCompilerCtfeError::Invalid(format!("{}: {error}", path.display())))
}

fn ctfe_files(
    directory: &Path,
) -> Result<BTreeMap<String, (PathBuf, PathBuf)>, RustCompilerCtfeError> {
    let mut maps = BTreeMap::new();
    let mut events = BTreeMap::new();
    for entry in fs::read_dir(directory)
        .map_err(|error| io_error(directory, error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| io_error(directory, error))?
    {
        let path = entry.path();
        if !entry
            .file_type()
            .map_err(|error| io_error(&path, error))?
            .is_file()
        {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            return Err(RustCompilerCtfeError::Invalid(
                "compiler output contains a non-UTF-8 name".into(),
            ));
        };
        let destination = name
            .strip_prefix("ctfe-map-")
            .and_then(|name| name.strip_suffix(".json"))
            .map(|identity| (&mut maps, identity))
            .or_else(|| {
                name.strip_prefix("ctfe-events-")
                    .and_then(|name| name.strip_suffix(".jsonl"))
                    .map(|identity| (&mut events, identity))
            });
        if let Some((destination, identity)) = destination
            && destination.insert(identity.to_owned(), path).is_some()
        {
            return Err(RustCompilerCtfeError::Invalid(format!(
                "duplicate CTFE compiler unit {identity}"
            )));
        }
    }
    if maps.keys().ne(events.keys()) {
        return Err(RustCompilerCtfeError::Invalid(format!(
            "CTFE map/event identities differ (maps: {}, events: {})",
            maps.len(),
            events.len()
        )));
    }
    Ok(maps
        .into_iter()
        .map(|(identity, map)| (identity.clone(), (map, events.remove(&identity).unwrap())))
        .collect())
}

fn parse_events(path: &Path) -> Result<Vec<CtfeEvent>, RustCompilerCtfeError> {
    let bytes = fs::read(path).map_err(|error| io_error(path, error))?;
    bytes
        .split(|byte| *byte == b'\n')
        .enumerate()
        .filter(|(_, line)| !line.is_empty())
        .map(|(index, line)| {
            serde_json::from_slice(line).map_err(|error| {
                RustCompilerCtfeError::Invalid(format!("{}:{}: {error}", path.display(), index + 1))
            })
        })
        .collect()
}

fn reconstruct_unit(
    identity: String,
    map_path: &Path,
    event_path: &Path,
    normalized: &NormalizedRustCompilerManifest,
    timestamp_ms: i64,
) -> Result<RustCompilerCtfeUnit, RustCompilerCtfeError> {
    let map: CtfeMapFile = parse_json(
        map_path,
        &fs::read(map_path).map_err(|error| io_error(map_path, error))?,
    )?;
    if map.schema != MAP_SCHEMA || map.crate_name.trim().is_empty() {
        return Err(RustCompilerCtfeError::Invalid(format!(
            "{} has an unsupported schema or empty crate",
            map_path.display()
        )));
    }
    let decisions = normalized
        .manifest
        .decisions
        .iter()
        .map(|decision| (decision.id.as_str(), decision))
        .collect::<BTreeMap<_, _>>();
    let mut mappings = BTreeMap::<u64, CtfeMapping>::new();
    for mapping in map.mappings {
        let marker = parse_u64(&mapping.marker, "CTFE marker")?;
        if marker == 0
            || mapping.definition.trim().is_empty()
            || !matches!(
                mapping.observation_kind.as_str(),
                "entry"
                    | "block"
                    | "edge"
                    | "exit"
                    | "decision-start"
                    | "decision-condition"
                    | "decision-finish"
            )
        {
            return Err(RustCompilerCtfeError::Invalid(format!(
                "malformed mapping for marker {marker}"
            )));
        }
        let mut previous = None;
        for hit in &mapping.hit_ordinals {
            let hit = parse_u64(hit, "CTFE hit ordinal")?;
            if hit == 0 || previous.is_some_and(|previous| previous >= hit) {
                return Err(RustCompilerCtfeError::Invalid(format!(
                    "marker {marker} has non-canonical hit ordinals"
                )));
            }
            if normalized.internal_ordinals.contains(&hit)
                || !normalized.hit_obligations_by_ordinal.contains_key(&hit)
            {
                return Err(RustCompilerCtfeError::Invalid(format!(
                    "marker {marker} references unknown/non-evidence ordinal {hit}"
                )));
            }
            previous = Some(hit);
        }
        match &mapping.decision {
            None if mapping.observation_kind.starts_with("decision-") => {
                return Err(RustCompilerCtfeError::Invalid(format!(
                    "semantic marker {marker} has no decision mapping"
                )));
            }
            Some(_) if !mapping.observation_kind.starts_with("decision-") => {
                return Err(RustCompilerCtfeError::Invalid(format!(
                    "non-decision marker {marker} carries a decision mapping"
                )));
            }
            Some(decision) => {
                let meta = decisions.get(decision.id.as_str()).ok_or_else(|| {
                    RustCompilerCtfeError::Invalid(format!(
                        "marker {marker} references unknown decision {}",
                        decision.id
                    ))
                })?;
                let valid_shape = match decision.event.as_str() {
                    "start" => {
                        mapping.observation_kind == "decision-start"
                            && decision.condition_index.is_none()
                            && decision.value.is_none()
                            && decision.outcome.is_none()
                    }
                    "condition" => {
                        mapping.observation_kind == "decision-condition"
                            && decision
                                .condition_index
                                .is_some_and(|index| index < meta.conditions.len() as u64)
                            && decision.value.is_some()
                            && decision.outcome.is_none()
                    }
                    "finish" => {
                        mapping.observation_kind == "decision-finish"
                            && decision.condition_index.is_none()
                            && decision.value.is_none()
                            && decision.outcome.is_some()
                    }
                    _ => false,
                };
                if !valid_shape {
                    return Err(RustCompilerCtfeError::Invalid(format!(
                        "marker {marker} has a malformed decision event"
                    )));
                }
                let expected_hits = match decision.event.as_str() {
                    "start" | "condition" => BTreeSet::new(),
                    "finish" => {
                        let outcome = decision.outcome.expect("validated decision outcome");
                        let alternatives = normalized
                            .decision_outcome_obligations
                            .get(&decision.id)
                            .expect("validated decision outcome mapping");
                        BTreeSet::from([if outcome {
                            alternatives.1.as_str()
                        } else {
                            alternatives.0.as_str()
                        }])
                    }
                    _ => unreachable!("validated decision event"),
                };
                let mapped_hits = mapping
                    .hit_ordinals
                    .iter()
                    .map(|ordinal| parse_u64(ordinal, "CTFE hit ordinal"))
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .flat_map(|ordinal| normalized.hit_obligations_by_ordinal[&ordinal].iter())
                    .map(String::as_str)
                    .collect::<BTreeSet<_>>();
                if mapped_hits != expected_hits {
                    return Err(RustCompilerCtfeError::Invalid(format!(
                        "decision {} {} marker maps to the wrong coverage obligations",
                        decision.id, decision.event
                    )));
                }
            }
            None => {}
        }
        if mappings.insert(marker, mapping).is_some() {
            return Err(RustCompilerCtfeError::Invalid(format!(
                "duplicate CTFE marker {marker}"
            )));
        }
    }
    if mappings.is_empty() {
        return Err(RustCompilerCtfeError::Invalid(format!(
            "{} contains no mappings",
            map_path.display()
        )));
    }

    let events = parse_events(event_path)?;
    let mut stacks = BTreeMap::<String, Vec<ActiveInvocation>>::new();
    let mut hits = BTreeSet::new();
    let mut decision_vectors = BTreeMap::<String, BTreeSet<(Vec<Option<bool>>, bool)>>::new();
    let mut runtime_events = Vec::new();
    for event in &events {
        if event.kind != "ctfe-marker"
            || event.crate_name != map.crate_name
            || event.thread.trim().is_empty()
        {
            return Err(RustCompilerCtfeError::Invalid(format!(
                "{} contains malformed event identity",
                event_path.display()
            )));
        }
        let marker = parse_u64(&event.marker, "observed CTFE marker")?;
        let mapping = mappings.get(&marker).ok_or_else(|| {
            RustCompilerCtfeError::Invalid(format!("observed CTFE marker {marker} is unmapped"))
        })?;
        if mapping.definition != event.definition
            || mapping.observation_kind != event.observation_kind
            || mapping.ordinal != event.ordinal
        {
            return Err(RustCompilerCtfeError::Invalid(format!(
                "observed CTFE marker {marker} changed identity"
            )));
        }
        let stack = stacks.entry(event.thread.clone()).or_default();
        match event.observation_kind.as_str() {
            "entry" => stack.push(ActiveInvocation {
                definition: event.definition.clone(),
                decisions: Vec::new(),
            }),
            "block" | "edge" | "decision-start" | "decision-condition" | "decision-finish" => {
                let Some(invocation) = stack.last_mut() else {
                    return Err(RustCompilerCtfeError::Invalid(format!(
                        "CTFE marker {marker} was observed outside an invocation on {}",
                        event.thread
                    )));
                };
                if invocation.definition != event.definition {
                    return Err(RustCompilerCtfeError::Invalid(format!(
                        "CTFE marker {marker} crossed invocation identity on {}",
                        event.thread
                    )));
                }
                if let Some(decision) = &mapping.decision {
                    match decision.event.as_str() {
                        "start" => {
                            let meta = decisions[decision.id.as_str()];
                            invocation.decisions.push(ActiveDecision {
                                id: decision.id.clone(),
                                values: vec![None; meta.conditions.len()],
                            });
                        }
                        "condition" => {
                            let active = invocation.decisions.last_mut().ok_or_else(|| {
                                RustCompilerCtfeError::Invalid(format!(
                                    "decision condition {} has no active frame",
                                    decision.id
                                ))
                            })?;
                            if active.id != decision.id {
                                return Err(RustCompilerCtfeError::Invalid(format!(
                                    "decision condition {} crossed active decision {}",
                                    decision.id, active.id
                                )));
                            }
                            let index = usize::try_from(
                                decision.condition_index.expect("validated condition index"),
                            )
                            .map_err(|_| {
                                RustCompilerCtfeError::Invalid(format!(
                                    "decision {} condition index exceeds usize",
                                    decision.id
                                ))
                            })?;
                            if active.values[index]
                                .replace(decision.value.expect("validated condition value"))
                                .is_some()
                            {
                                return Err(RustCompilerCtfeError::Invalid(format!(
                                    "decision {} condition {index} was observed twice",
                                    decision.id
                                )));
                            }
                        }
                        "finish" => {
                            let active = invocation.decisions.pop().ok_or_else(|| {
                                RustCompilerCtfeError::Invalid(format!(
                                    "decision finish {} has no active frame",
                                    decision.id
                                ))
                            })?;
                            if active.id != decision.id {
                                return Err(RustCompilerCtfeError::Invalid(format!(
                                    "decision finish {} closed active decision {}",
                                    decision.id, active.id
                                )));
                            }
                            let outcome = decision.outcome.expect("validated decision outcome");
                            decision_vectors
                                .entry(decision.id.clone())
                                .or_default()
                                .insert((active.values.clone(), outcome));
                            runtime_events.push(RuntimeEvent {
                                event_type: "decision".into(),
                                id: decision.id.clone(),
                                vector: Some(McdcVector {
                                    values: active.values,
                                    outcome,
                                }),
                                timestamp_ms,
                                phase_id: None,
                                environment: "rust-ctfe".into(),
                            });
                        }
                        _ => unreachable!("validated decision event"),
                    }
                }
            }
            "exit" => {
                let Some(invocation) = stack.pop() else {
                    return Err(RustCompilerCtfeError::Invalid(format!(
                        "CTFE marker {marker} closed an absent invocation on {}",
                        event.thread
                    )));
                };
                if invocation.definition != event.definition || !invocation.decisions.is_empty() {
                    return Err(RustCompilerCtfeError::Invalid(format!(
                        "CTFE marker {marker} closed the wrong or incomplete invocation on {}",
                        event.thread
                    )));
                }
            }
            _ => unreachable!("validated observation kind"),
        }
        for ordinal in &mapping.hit_ordinals {
            let ordinal = parse_u64(ordinal, "CTFE hit ordinal")?;
            for id in &normalized.hit_obligations_by_ordinal[&ordinal] {
                hits.insert(id.clone());
                runtime_events.push(RuntimeEvent {
                    event_type: "hit".into(),
                    id: id.clone(),
                    vector: None,
                    timestamp_ms,
                    phase_id: None,
                    environment: "rust-ctfe".into(),
                });
            }
        }
    }
    if let Some((thread, stack)) = stacks.iter().find(|(_, stack)| !stack.is_empty()) {
        return Err(RustCompilerCtfeError::Invalid(format!(
            "successful compiler unit {identity} left {} CTFE frame(s) open on {thread}",
            stack.len()
        )));
    }
    Ok(RustCompilerCtfeUnit {
        identity,
        crate_name: map.crate_name,
        snapshot: RuntimeSnapshot {
            decisions: decision_vectors
                .into_iter()
                .map(|(id, vectors)| DecisionSnapshot {
                    meta: decisions[id.as_str()].clone(),
                    vectors: vectors
                        .into_iter()
                        .map(|(values, outcome)| McdcVector { values, outcome })
                        .collect(),
                })
                .collect(),
            hits: hits.into_iter().collect(),
            events: runtime_events,
        },
        observations: events.len(),
    })
}

pub fn read_rust_compiler_ctfe(
    directory: &Path,
    normalized: &NormalizedRustCompilerManifest,
    timestamp_ms: i64,
) -> Result<Vec<RustCompilerCtfeUnit>, RustCompilerCtfeError> {
    ctfe_files(directory)?
        .into_iter()
        .map(|(identity, (map, events))| {
            reconstruct_unit(identity, &map, &events, normalized, timestamp_ms)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    use serde_json::{Value, json};

    use crate::{
        coverage_analysis::PointKind,
        coverage_report::{
            BranchAlternativeMeta, BranchMeta, CoverageManifest, DecisionMeta, PointMeta,
        },
    };

    static SCRATCH_NONCE: AtomicU64 = AtomicU64::new(0);

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let epoch = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock before epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "supercov-rust-ctfe-{}-{epoch}-{}",
                std::process::id(),
                SCRATCH_NONCE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).expect("create CTFE scratch directory");
            Self(path)
        }

        fn write(&self, map: Value, events: &[Value]) {
            fs::write(
                self.0.join("ctfe-map-unit.json"),
                serde_json::to_vec(&map).expect("serialize CTFE map"),
            )
            .expect("write CTFE map");
            let mut bytes = events
                .iter()
                .map(|event| serde_json::to_string(event).expect("serialize CTFE event"))
                .collect::<Vec<_>>()
                .join("\n")
                .into_bytes();
            bytes.push(b'\n');
            fs::write(self.0.join("ctfe-events-unit.jsonl"), bytes).expect("write CTFE events");
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn normalized_manifest() -> NormalizedRustCompilerManifest {
        let decision = DecisionMeta {
            id: "decision".into(),
            file: "src/lib.rs".into(),
            line: 1,
            column: 1,
            source: "value".into(),
            conditions: vec!["value".into()],
            kind: "control".into(),
        };
        let branch = BranchMeta {
            id: "outcome".into(),
            kind: "decision-outcome".into(),
            file: "src/lib.rs".into(),
            line: 1,
            column: 1,
            source: "value".into(),
            alternatives: vec![
                BranchAlternativeMeta {
                    id: "false-alternative".into(),
                    label: "condition false".into(),
                },
                BranchAlternativeMeta {
                    id: "true-alternative".into(),
                    label: "condition true".into(),
                },
            ],
        };
        NormalizedRustCompilerManifest {
            manifest: CoverageManifest {
                decisions: vec![decision],
                points: vec![PointMeta {
                    id: "function".into(),
                    kind: PointKind::Function,
                    file: "src/lib.rs".into(),
                    line: 1,
                    column: 1,
                    source: "const fn evaluated(value: bool) -> bool".into(),
                    label: None,
                }],
                branches: vec![branch],
                limitations: Vec::new(),
                scope: None,
            },
            hit_obligations_by_ordinal: BTreeMap::from([
                (101, vec!["function".into()]),
                (201, vec!["false-alternative".into()]),
                (202, vec!["true-alternative".into()]),
            ]),
            internal_ordinals: BTreeSet::new(),
            decision_outcome_obligations: BTreeMap::from([(
                "decision".into(),
                ("false-alternative".into(), "true-alternative".into()),
            )]),
        }
    }

    fn mapping(
        marker: &str,
        observation_kind: &str,
        hit_ordinals: &[&str],
        decision: Option<Value>,
    ) -> Value {
        json!({
            "marker": marker,
            "definition": "fixture::evaluated",
            "observationKind": observation_kind,
            "ordinal": 0,
            "hitOrdinals": hit_ordinals,
            "decision": decision,
        })
    }

    fn event(marker: &str, observation_kind: &str) -> Value {
        json!({
            "crate": "fixture",
            "kind": "ctfe-marker",
            "marker": marker,
            "definition": "fixture::evaluated",
            "observationKind": observation_kind,
            "ordinal": 0,
            "thread": "compiler-thread-1",
        })
    }

    fn decision_event(
        id: &str,
        event: &str,
        condition_index: Option<u64>,
        value: Option<bool>,
        outcome: Option<bool>,
    ) -> Value {
        json!({
            "id": id,
            "event": event,
            "conditionIndex": condition_index,
            "value": value,
            "outcome": outcome,
        })
    }

    fn valid_map() -> Value {
        json!({
            "schema": MAP_SCHEMA,
            "crate": "fixture",
            "mappings": [
                mapping("1", "entry", &["101"], None),
                mapping("2", "decision-start", &[], Some(decision_event(
                    "decision", "start", None, None, None,
                ))),
                mapping("3", "decision-condition", &[], Some(decision_event(
                    "decision", "condition", Some(0), Some(false), None,
                ))),
                mapping("4", "decision-finish", &["201"], Some(decision_event(
                    "decision", "finish", None, None, Some(false),
                ))),
                mapping("5", "exit", &[], None),
                mapping("6", "decision-condition", &[], Some(decision_event(
                    "decision", "condition", Some(0), Some(true), None,
                ))),
                mapping("7", "decision-finish", &["202"], Some(decision_event(
                    "decision", "finish", None, None, Some(true),
                ))),
            ],
        })
    }

    fn valid_events() -> Vec<Value> {
        [
            ("1", "entry"),
            ("2", "decision-start"),
            ("3", "decision-condition"),
            ("4", "decision-finish"),
            ("5", "exit"),
            ("1", "entry"),
            ("2", "decision-start"),
            ("6", "decision-condition"),
            ("7", "decision-finish"),
            ("5", "exit"),
        ]
        .into_iter()
        .map(|(marker, kind)| event(marker, kind))
        .collect()
    }

    #[test]
    fn canonical_unsigned_decimal_rejects_aliases() {
        assert_eq!(parse_u64("12", "marker").unwrap(), 12);
        assert!(parse_u64("012", "marker").is_err());
        assert!(parse_u64("-1", "marker").is_err());
        assert!(parse_u64("", "marker").is_err());
    }

    #[test]
    fn reconstructs_exact_independent_ctfe_vectors_and_outcome_hits() {
        let scratch = Scratch::new();
        scratch.write(valid_map(), &valid_events());

        let units = read_rust_compiler_ctfe(&scratch.0, &normalized_manifest(), 42).unwrap();
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].observations, 10);
        assert_eq!(
            units[0].snapshot.hits,
            ["false-alternative", "function", "true-alternative"]
        );
        assert_eq!(units[0].snapshot.decisions.len(), 1);
        assert_eq!(
            units[0].snapshot.decisions[0].vectors,
            [
                McdcVector {
                    values: vec![Some(false)],
                    outcome: false,
                },
                McdcVector {
                    values: vec![Some(true)],
                    outcome: true,
                },
            ]
        );
    }

    #[test]
    fn rejects_semantic_marker_with_unrelated_hit() {
        let scratch = Scratch::new();
        let mut map = valid_map();
        map["mappings"][1]["hitOrdinals"] = json!(["201"]);
        scratch.write(map, &valid_events());

        let error = read_rust_compiler_ctfe(&scratch.0, &normalized_manifest(), 42)
            .expect_err("semantic start marker must not carry a coverage hit");
        assert!(error.to_string().contains("wrong coverage obligations"));
    }

    #[test]
    fn rejects_finish_mapped_to_the_wrong_outcome_alternative() {
        let scratch = Scratch::new();
        let mut map = valid_map();
        map["mappings"][3]["hitOrdinals"] = json!(["202"]);
        scratch.write(map, &valid_events());

        let error = read_rust_compiler_ctfe(&scratch.0, &normalized_manifest(), 42)
            .expect_err("false finish must not map to the true alternative");
        assert!(error.to_string().contains("wrong coverage obligations"));
    }

    #[test]
    fn rejects_condition_without_an_active_decision() {
        let scratch = Scratch::new();
        let mut events = valid_events();
        events.remove(1);
        scratch.write(valid_map(), &events);

        let error = read_rust_compiler_ctfe(&scratch.0, &normalized_manifest(), 42)
            .expect_err("condition without start must fail closed");
        assert!(error.to_string().contains("has no active frame"));
    }

    #[test]
    fn rejects_exit_with_an_incomplete_decision() {
        let scratch = Scratch::new();
        let events = valid_events()
            .into_iter()
            .enumerate()
            .filter_map(|(index, event)| (index != 3).then_some(event))
            .collect::<Vec<_>>();
        scratch.write(valid_map(), &events);

        let error = read_rust_compiler_ctfe(&scratch.0, &normalized_manifest(), 42)
            .expect_err("invocation exit with an open decision must fail closed");
        assert!(error.to_string().contains("wrong or incomplete invocation"));
    }
}
