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
    coverage_report::{RuntimeEvent, RuntimeSnapshot},
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
    let mut mappings = BTreeMap::<u64, CtfeMapping>::new();
    for mapping in map.mappings {
        let marker = parse_u64(&mapping.marker, "CTFE marker")?;
        if marker == 0
            || mapping.definition.trim().is_empty()
            || !matches!(
                mapping.observation_kind.as_str(),
                "entry" | "block" | "edge" | "exit"
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
    let mut stacks = BTreeMap::<String, Vec<String>>::new();
    let mut hits = BTreeSet::new();
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
            "entry" => stack.push(event.definition.clone()),
            "block" | "edge" => {
                if stack.last() != Some(&event.definition) {
                    return Err(RustCompilerCtfeError::Invalid(format!(
                        "CTFE marker {marker} crossed invocation identity on {}",
                        event.thread
                    )));
                }
            }
            "exit" => {
                if stack.pop().as_deref() != Some(event.definition.as_str()) {
                    return Err(RustCompilerCtfeError::Invalid(format!(
                        "CTFE marker {marker} closed the wrong invocation on {}",
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
            decisions: Vec::new(),
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

    #[test]
    fn canonical_unsigned_decimal_rejects_aliases() {
        assert_eq!(parse_u64("12", "marker").unwrap(), 12);
        assert!(parse_u64("012", "marker").is_err());
        assert!(parse_u64("-1", "marker").is_err());
        assert!(parse_u64("", "marker").is_err());
    }
}
