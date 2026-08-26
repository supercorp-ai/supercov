//! Strict deferred source joining for rustdoc's merged doctest mode.
//!
//! rustdoc compiles an extracted bundle before it compiles the runner that
//! carries each `__doctest_N` module's original path and line. The bundle must
//! therefore publish temporary, run-local identities. This module validates
//! the later runner map and resolves extracted byte ranges back to immutable
//! authored source before the ordinary compiler-manifest parser sees them.

use std::collections::{BTreeMap, BTreeSet};

use ra_ap_syntax::{
    AstNode, Edition, SourceFile,
    ast::{self, HasName},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::rust_compiler_manifest::{
    RustCompilerManifest, RustCompilerSource, RustCompilerSourceSnapshots,
};

const MAP_SCHEMA: &str = "supercov-rustdoc-merged-map-v1";
const SOURCE_MODEL: &str = "rust-source-v1";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RustdocMergedMap {
    pub schema: String,
    pub group: String,
    pub entries: Vec<RustdocMergedEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RustdocMergedEntry {
    pub module: String,
    pub display_name: String,
    pub path: String,
    pub line: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RustdocMappedRange {
    pub source_key: String,
    pub start: u32,
    pub end: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RustSourceIdentity {
    pub id: String,
    pub canonical: String,
    pub probe_ordinal: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RustdocMergedJoin {
    pub manifest: RustCompilerManifest,
    pub sources: RustCompilerSourceSnapshots,
    /// Every temporary bundle identity translated to its final authored ID.
    pub obligation_ids: BTreeMap<String, String>,
    /// Every temporary runtime ordinal translated to its final authored ordinal.
    pub probe_ordinals: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RustdocJoinError {
    Json(String),
    Manifest(String),
    Invalid(String),
}

impl std::fmt::Display for RustdocJoinError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Json(reason) => write!(formatter, "invalid merged rustdoc map JSON: {reason}"),
            Self::Manifest(reason) => {
                write!(
                    formatter,
                    "invalid merged rustdoc compiler manifest: {reason}"
                )
            }
            Self::Invalid(reason) => write!(formatter, "invalid merged rustdoc join: {reason}"),
        }
    }
}

impl std::error::Error for RustdocJoinError {}

fn safe_group(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn module_index(value: &str) -> Option<u64> {
    value.strip_prefix("__doctest_")?.parse::<u64>().ok()
}

fn normalized_relative_path(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('/')
        && !value.contains('\\')
        && value
            .split('/')
            .all(|component| !component.is_empty() && !matches!(component, "." | ".."))
}

impl RustdocMergedMap {
    pub fn parse(bytes: &[u8]) -> Result<Self, RustdocJoinError> {
        let map: Self = serde_json::from_slice(bytes)
            .map_err(|error| RustdocJoinError::Json(error.to_string()))?;
        map.validate()?;
        Ok(map)
    }

    pub fn validate(&self) -> Result<(), RustdocJoinError> {
        if self.schema != MAP_SCHEMA || !safe_group(&self.group) || self.entries.is_empty() {
            return Err(RustdocJoinError::Invalid(
                "schema, group and at least one entry are required".into(),
            ));
        }
        let mut modules = BTreeSet::new();
        let mut source_sites = BTreeSet::new();
        let mut previous = None;
        for entry in &self.entries {
            let Some(index) = module_index(&entry.module) else {
                return Err(RustdocJoinError::Invalid(format!(
                    "invalid merged doctest module {}",
                    entry.module
                )));
            };
            if previous.is_some_and(|previous| previous >= index) {
                return Err(RustdocJoinError::Invalid(
                    "merged doctest entries are not in numeric module order".into(),
                ));
            }
            previous = Some(index);
            if !modules.insert(entry.module.as_str())
                || !source_sites.insert((entry.path.as_str(), entry.line))
                || !normalized_relative_path(&entry.path)
                || entry.line == 0
                || entry.display_name.trim().is_empty()
                || entry.display_name.chars().any(char::is_control)
            {
                return Err(RustdocJoinError::Invalid(format!(
                    "malformed merged doctest entry {}",
                    entry.module
                )));
            }
        }
        Ok(())
    }

    pub fn entry(&self, module: &str) -> Result<&RustdocMergedEntry, RustdocJoinError> {
        self.entries
            .iter()
            .find(|entry| entry.module == module)
            .ok_or_else(|| {
                RustdocJoinError::Invalid(format!(
                    "pending bundle module {module} has no runner descriptor"
                ))
            })
    }

    fn next_line_for(&self, entry: &RustdocMergedEntry) -> Option<u64> {
        self.entries
            .iter()
            .filter(|candidate| candidate.path == entry.path && candidate.line > entry.line)
            .map(|candidate| candidate.line)
            .min()
    }
}

fn source_lines(source: &str) -> Vec<(u64, usize, &str)> {
    let mut offset = 0;
    source
        .split_inclusive('\n')
        .enumerate()
        .map(|(index, line)| {
            let record = (index as u64 + 1, offset, line);
            offset += line.len();
            record
        })
        .collect()
}

#[derive(Clone, Copy)]
struct ExtractedLine<'a> {
    start: usize,
    end: usize,
    source: &'a str,
}

fn extracted_module_lines<'a>(
    bundle_source: &'a str,
    module: &str,
) -> Result<Vec<ExtractedLine<'a>>, RustdocJoinError> {
    let parsed = SourceFile::parse(bundle_source, Edition::CURRENT);
    if !parsed.errors().is_empty() {
        return Err(RustdocJoinError::Invalid(format!(
            "merged bundle does not parse as Rust: {}",
            parsed
                .errors()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ")
        )));
    }
    let tree = parsed.tree();
    let modules = tree
        .syntax()
        .descendants()
        .filter_map(ast::Module::cast)
        .filter(|candidate| candidate.name().is_some_and(|name| name.text() == module))
        .collect::<Vec<_>>();
    let [module_node] = modules.as_slice() else {
        return Err(RustdocJoinError::Invalid(format!(
            "merged bundle contains {} modules named {module}",
            modules.len()
        )));
    };
    let functions = module_node
        .syntax()
        .descendants()
        .filter_map(ast::Fn::cast)
        .filter(|function| {
            function.name().is_some_and(|name| name.text() == "main")
                && function
                    .syntax()
                    .ancestors()
                    .skip(1)
                    .find_map(ast::Module::cast)
                    .as_ref()
                    == Some(module_node)
        })
        .collect::<Vec<_>>();
    let [function] = functions.as_slice() else {
        return Err(RustdocJoinError::Invalid(format!(
            "merged module {module} contains {} direct main functions",
            functions.len()
        )));
    };
    let body = function.body().ok_or_else(|| {
        RustdocJoinError::Invalid(format!("merged module {module} main has no body"))
    })?;
    let range = body.syntax().text_range();
    let body_start = usize::from(range.start());
    let body_end = usize::from(range.end());
    if body_end <= body_start + 1
        || bundle_source.as_bytes().get(body_start) != Some(&b'{')
        || bundle_source.as_bytes().get(body_end - 1) != Some(&b'}')
    {
        return Err(RustdocJoinError::Invalid(format!(
            "merged module {module} main has an invalid syntax range"
        )));
    }
    let content_start = body_start + 1;
    let content = &bundle_source[content_start..body_end - 1];
    let mut offset = content_start;
    let lines = content
        .split_inclusive('\n')
        .filter_map(|line| {
            let source = line.strip_suffix('\n').unwrap_or(line);
            let record = (!source.trim().is_empty()).then_some(ExtractedLine {
                start: offset,
                end: offset + source.len(),
                source,
            });
            offset += line.len();
            record
        })
        .collect::<Vec<_>>();
    if lines.is_empty() {
        return Err(RustdocJoinError::Invalid(format!(
            "merged module {module} main has no extracted source lines"
        )));
    }
    Ok(lines)
}

/// Map one exact extracted range to its authored source. Its nonblank lines
/// must have exactly one complete, ordered mapping inside that doctest's
/// runner-bounded source interval. Repeated fragments are valid when their
/// sequence identifies one mapping; genuinely ambiguous sequences fail closed.
pub fn map_merged_range(
    map: &RustdocMergedMap,
    module: &str,
    bundle_source: &str,
    pending_start: u32,
    pending_end: u32,
    authored_source: &str,
) -> Result<RustdocMappedRange, RustdocJoinError> {
    map.validate()?;
    let entry = map.entry(module)?;
    let start = pending_start as usize;
    let end = pending_end as usize;
    if start >= end
        || end > bundle_source.len()
        || !bundle_source.is_char_boundary(start)
        || !bundle_source.is_char_boundary(end)
    {
        return Err(RustdocJoinError::Invalid(format!(
            "pending range {pending_start}..{pending_end} is outside UTF-8 bundle bytes"
        )));
    }
    if bundle_source[start..end].contains('\r') {
        return Err(RustdocJoinError::Invalid(
            "carriage-return extracted source is unsupported".into(),
        ));
    }
    let next_line = map.next_line_for(entry).unwrap_or(u64::MAX);
    let authored_lines = source_lines(authored_source);
    let extracted_lines = extracted_module_lines(bundle_source, module)?;
    let candidates = extracted_lines
        .iter()
        .map(|extracted| {
            authored_lines
                .iter()
                .filter(|(line, _, _)| *line >= entry.line && *line < next_line)
                .flat_map(|(line, offset, authored_line)| {
                    authored_line
                        .match_indices(extracted.source)
                        .map(move |(column, _)| (*line, *offset + column, extracted.source.len()))
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    if candidates.iter().any(Vec::is_empty) {
        return Err(RustdocJoinError::Invalid(format!(
            "merged fragment has no authored match in {}:{}",
            entry.path, entry.line
        )));
    }
    fn ordered_sequences(
        candidates: &[Vec<(u64, usize, usize)>],
        index: usize,
        previous_line: Option<u64>,
        current: &mut Vec<(u64, usize, usize)>,
        solutions: &mut Vec<Vec<(u64, usize, usize)>>,
    ) {
        if solutions.len() > 1 {
            return;
        }
        if index == candidates.len() {
            solutions.push(current.clone());
            return;
        }
        for candidate in &candidates[index] {
            if previous_line.is_some_and(|previous| previous >= candidate.0) {
                continue;
            }
            current.push(*candidate);
            ordered_sequences(candidates, index + 1, Some(candidate.0), current, solutions);
            current.pop();
            if solutions.len() > 1 {
                return;
            }
        }
    }
    let mut solutions = Vec::new();
    ordered_sequences(&candidates, 0, None, &mut Vec::new(), &mut solutions);
    let [anchors] = solutions.as_slice() else {
        return Err(RustdocJoinError::Invalid(format!(
            "merged fragments have {} ordered authored mappings in {}:{}",
            solutions.len(),
            entry.path,
            entry.line
        )));
    };
    let start_line = extracted_lines
        .iter()
        .position(|line| start >= line.start && start < line.end)
        .ok_or_else(|| {
            RustdocJoinError::Invalid(format!(
                "pending range start {pending_start} is outside extracted source lines"
            ))
        })?;
    let end_line = extracted_lines
        .iter()
        .position(|line| end > line.start && end <= line.end)
        .ok_or_else(|| {
            RustdocJoinError::Invalid(format!(
                "pending range end {pending_end} is outside extracted source lines"
            ))
        })?;
    if start_line > end_line {
        return Err(RustdocJoinError::Invalid(
            "pending range crosses extracted lines in reverse order".into(),
        ));
    }
    let authored_start = anchors[start_line]
        .1
        .checked_add(start - extracted_lines[start_line].start)
        .ok_or_else(|| RustdocJoinError::Invalid("authored source offset overflow".into()))?;
    let authored_end = anchors[end_line]
        .1
        .checked_add(end - extracted_lines[end_line].start)
        .ok_or_else(|| RustdocJoinError::Invalid("authored source offset overflow".into()))?;
    Ok(RustdocMappedRange {
        source_key: format!("source:{}", entry.path),
        start: u32::try_from(authored_start)
            .map_err(|_| RustdocJoinError::Invalid("authored start exceeds u32".into()))?,
        end: u32::try_from(authored_end)
            .map_err(|_| RustdocJoinError::Invalid("authored end exceeds u32".into()))?,
    })
}

/// Produce the frozen identity for a non-synthetic authored/doctest
/// obligation after deferred source mapping.
pub fn rust_source_identity(
    kind: &str,
    source: &RustdocMappedRange,
    discriminator: &str,
) -> Result<RustSourceIdentity, RustdocJoinError> {
    if !matches!(
        kind,
        "statement" | "function" | "branch" | "branch-alternative" | "decision" | "match-group"
    ) || !source.source_key.starts_with("source:")
        || !normalized_relative_path(&source.source_key["source:".len()..])
        || source.start >= source.end
    {
        return Err(RustdocJoinError::Invalid(
            "invalid final Rust source identity input".into(),
        ));
    }
    identity_for_range(kind, source, discriminator)
}

fn identity_for_range(
    kind: &str,
    source: &RustdocMappedRange,
    discriminator: &str,
) -> Result<RustSourceIdentity, RustdocJoinError> {
    if !matches!(
        kind,
        "statement" | "function" | "branch" | "branch-alternative" | "decision" | "match-group"
    ) || source.start >= source.end
        || source.source_key.chars().any(char::is_control)
        || discriminator.chars().any(char::is_control)
    {
        return Err(RustdocJoinError::Invalid(
            "invalid Rust source identity components".into(),
        ));
    }
    let canonical = format!(
        "{SOURCE_MODEL}\0{kind}\0{}\0{}\0{}\0{discriminator}\0",
        source.source_key, source.start, source.end
    );
    identity_from_canonical(kind, canonical)
}

fn identity_from_canonical(
    kind: &str,
    canonical: String,
) -> Result<RustSourceIdentity, RustdocJoinError> {
    let digest = Sha256::digest(canonical.as_bytes());
    let encoded = digest[..12]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let probe_ordinal = u64::from_be_bytes(
        digest[..8]
            .try_into()
            .expect("a SHA-256 digest always has eight prefix bytes"),
    );
    Ok(RustSourceIdentity {
        id: format!("rs:{kind}:{encoded}"),
        canonical,
        probe_ordinal,
    })
}

#[derive(Debug)]
struct SyntheticExpansionFrame {
    description: String,
    source: RustdocMappedRange,
    definition: String,
}

#[derive(Debug)]
struct SyntheticCanonical {
    frames: Vec<SyntheticExpansionFrame>,
    definition: String,
    owner_ordinal: u64,
}

fn canonical_u32(value: &str, field: &str) -> Result<u32, RustdocJoinError> {
    let parsed = value.parse::<u32>().map_err(|_| {
        RustdocJoinError::Invalid(format!("synthetic canonical has invalid {field}"))
    })?;
    if value != parsed.to_string() {
        return Err(RustdocJoinError::Invalid(format!(
            "synthetic canonical has non-canonical {field}"
        )));
    }
    Ok(parsed)
}

fn canonical_u64(value: &str, field: &str) -> Result<u64, RustdocJoinError> {
    let parsed = value.parse::<u64>().map_err(|_| {
        RustdocJoinError::Invalid(format!("synthetic canonical has invalid {field}"))
    })?;
    if value != parsed.to_string() {
        return Err(RustdocJoinError::Invalid(format!(
            "synthetic canonical has non-canonical {field}"
        )));
    }
    Ok(parsed)
}

fn parse_synthetic_canonical(
    canonical: &str,
    kind: &str,
    source_key: &str,
    start: u32,
    end: u32,
    discriminator: &str,
) -> Result<Option<SyntheticCanonical>, RustdocJoinError> {
    let parts = canonical.split('\0').collect::<Vec<_>>();
    if parts.get(6) != Some(&"synthetic-expansion") {
        return Ok(None);
    }
    if parts.last() != Some(&"")
        || parts.len() < 15
        || (parts.len() - 10) % 5 != 0
        || parts[0] != SOURCE_MODEL
        || parts[1] != kind
        || parts[2] != source_key
        || canonical_u32(parts[3], "source start")? != start
        || canonical_u32(parts[4], "source end")? != end
        || parts[5] != discriminator
    {
        return Err(RustdocJoinError::Invalid(format!(
            "malformed synthetic canonical for {kind}"
        )));
    }
    let frame_count = (parts.len() - 10) / 5;
    let mut frames = Vec::with_capacity(frame_count);
    for frame in parts[7..7 + frame_count * 5].chunks_exact(5) {
        if frame[0].is_empty() || frame[1].is_empty() || frame[4].is_empty() {
            return Err(RustdocJoinError::Invalid(
                "synthetic expansion frame has an empty identity component".into(),
            ));
        }
        frames.push(SyntheticExpansionFrame {
            description: frame[0].into(),
            source: RustdocMappedRange {
                source_key: frame[1].into(),
                start: canonical_u32(frame[2], "frame start")?,
                end: canonical_u32(frame[3], "frame end")?,
            },
            definition: frame[4].into(),
        });
    }
    let definition_index = 7 + frame_count * 5;
    let definition = parts[definition_index];
    let owner_ordinal = canonical_u64(parts[definition_index + 1], "owner ordinal")?;
    if definition.is_empty() {
        return Err(RustdocJoinError::Invalid(
            "synthetic canonical has an empty owner definition".into(),
        ));
    }
    Ok(Some(SyntheticCanonical {
        frames,
        definition: definition.into(),
        owner_ordinal,
    }))
}

fn stable_definition(
    entry: &RustdocMergedEntry,
    definition: &str,
) -> Result<String, RustdocJoinError> {
    let main = format!("{}::main", entry.module);
    if let Some(suffix) = definition.strip_prefix(&main) {
        return Ok(format!("doctest:{}:{}{suffix}", entry.path, entry.line));
    }
    if definition.is_empty()
        || definition.chars().any(char::is_control)
        || definition.contains("doctest_bundle_")
        || definition.contains("__doctest_")
    {
        return Err(RustdocJoinError::Invalid(format!(
            "synthetic expansion definition {definition} is not stable"
        )));
    }
    Ok(definition.into())
}

struct RebasedIdentity {
    identity: RustSourceIdentity,
    source: RustdocMappedRange,
    provenance: &'static str,
}

#[allow(clippy::too_many_arguments)]
fn rebase_identity(
    map: &RustdocMergedMap,
    entry: &RustdocMergedEntry,
    bundle_source: &str,
    authored_sources: &BTreeMap<String, RustCompilerSource>,
    kind: &str,
    source_key: &str,
    start: u32,
    end: u32,
    old_discriminator: &str,
    new_discriminator: &str,
    id: &str,
    canonical: &str,
    probe_ordinal: &str,
) -> Result<RebasedIdentity, RustdocJoinError> {
    let source = map_obligation_range(map, entry, bundle_source, start, end, authored_sources)?;
    if let Some(synthetic) =
        parse_synthetic_canonical(canonical, kind, source_key, start, end, old_discriminator)?
    {
        let old = identity_from_canonical(kind, canonical.into())?;
        verify_pending_identity(&old, id, Some(canonical), probe_ordinal)?;
        let pending_key = format!("doctest-pending:{}", map.group);
        let mut frame_canonical = String::new();
        for frame in synthetic.frames {
            if frame.source.source_key != pending_key {
                return Err(RustdocJoinError::Invalid(format!(
                    "synthetic expansion frame escaped pending source {}",
                    frame.source.source_key
                )));
            }
            let mapped = map_obligation_range(
                map,
                entry,
                bundle_source,
                frame.source.start,
                frame.source.end,
                authored_sources,
            )?;
            frame_canonical.push_str(&format!(
                "{}\0{}\0{}\0{}\0{}\0",
                frame.description,
                mapped.source_key,
                mapped.start,
                mapped.end,
                stable_definition(entry, &frame.definition)?,
            ));
        }
        let canonical = format!(
            "{SOURCE_MODEL}\0{kind}\0{}\0{}\0{}\0{new_discriminator}\0synthetic-expansion\0{}{}\0{}\0",
            source.source_key,
            source.start,
            source.end,
            frame_canonical,
            stable_definition(entry, &synthetic.definition)?,
            synthetic.owner_ordinal,
        );
        return Ok(RebasedIdentity {
            identity: identity_from_canonical(kind, canonical)?,
            source,
            provenance: "synthetic-expansion",
        });
    }
    let old = pending_identity(kind, source_key, start, end, old_discriminator)?;
    verify_pending_identity(&old, id, Some(canonical), probe_ordinal)?;
    Ok(RebasedIdentity {
        identity: rust_source_identity(kind, &source, new_discriminator)?,
        source,
        provenance: "doctest-source",
    })
}

fn pending_identity(
    kind: &str,
    source_key: &str,
    start: u32,
    end: u32,
    discriminator: &str,
) -> Result<RustSourceIdentity, RustdocJoinError> {
    identity_for_range(
        kind,
        &RustdocMappedRange {
            source_key: source_key.into(),
            start,
            end,
        },
        discriminator,
    )
}

fn verify_pending_identity(
    identity: &RustSourceIdentity,
    id: &str,
    canonical: Option<&str>,
    probe_ordinal: &str,
) -> Result<(), RustdocJoinError> {
    if identity.probe_ordinal == 0
        || id != identity.id
        || canonical.is_some_and(|canonical| canonical != identity.canonical)
        || probe_ordinal != identity.probe_ordinal.to_string()
    {
        return Err(RustdocJoinError::Invalid(format!(
            "temporary merged-doctest identity {id} does not match its frozen canonical form"
        )));
    }
    Ok(())
}

fn insert_translation(
    ids: &mut BTreeMap<String, String>,
    ordinals: &mut BTreeMap<String, String>,
    old_id: &str,
    old_ordinal: &str,
    new_identity: &RustSourceIdentity,
) -> Result<(), RustdocJoinError> {
    let parsed_ordinal = old_ordinal.parse::<u64>().map_err(|_| {
        RustdocJoinError::Invalid(format!(
            "temporary obligation {old_id} has an invalid ordinal"
        ))
    })?;
    if parsed_ordinal == 0 || old_ordinal != parsed_ordinal.to_string() {
        return Err(RustdocJoinError::Invalid(format!(
            "temporary obligation {old_id} has a non-canonical ordinal"
        )));
    }
    if ids.insert(old_id.into(), new_identity.id.clone()).is_some()
        || ordinals
            .insert(old_ordinal.into(), new_identity.probe_ordinal.to_string())
            .is_some()
    {
        return Err(RustdocJoinError::Invalid(format!(
            "duplicate temporary merged-doctest identity {old_id}"
        )));
    }
    Ok(())
}

fn definition_module<'a>(
    map: &'a RustdocMergedMap,
    definitions: &[String],
) -> Result<&'a RustdocMergedEntry, RustdocJoinError> {
    let mut matches = BTreeSet::new();
    for definition in definitions {
        for entry in &map.entries {
            let main = format!("{}::main", entry.module);
            if definition == &main || definition.starts_with(&format!("{main}::")) {
                matches.insert(entry.module.as_str());
            }
        }
    }
    if matches.len() != 1 {
        return Err(RustdocJoinError::Invalid(format!(
            "obligation definitions do not resolve to exactly one merged doctest module: {}",
            definitions.join(", ")
        )));
    }
    let module = matches.into_iter().next().expect("exactly one module");
    map.entry(module)
}

fn stable_definitions(
    entry: &RustdocMergedEntry,
    definitions: &[String],
) -> Result<Vec<String>, RustdocJoinError> {
    let main = format!("{}::main", entry.module);
    let root = format!("doctest:{}:{}", entry.path, entry.line);
    let mut stable = definitions
        .iter()
        .map(|definition| {
            definition
                .strip_prefix(&main)
                .map(|suffix| format!("{root}{suffix}"))
        })
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| {
            RustdocJoinError::Invalid(format!(
                "definition escaped merged doctest module {}",
                entry.module
            ))
        })?;
    stable.sort();
    stable.dedup();
    if stable.is_empty() {
        return Err(RustdocJoinError::Invalid(
            "merged doctest obligation has no stable definitions".into(),
        ));
    }
    Ok(stable)
}

fn authored_source<'a>(
    sources: &'a BTreeMap<String, RustCompilerSource>,
    entry: &RustdocMergedEntry,
) -> Result<&'a RustCompilerSource, RustdocJoinError> {
    let key = format!("source:{}", entry.path);
    let source = sources.get(&key).ok_or_else(|| {
        RustdocJoinError::Invalid(format!("authored source snapshot {key} was not supplied"))
    })?;
    if source.file != entry.path {
        return Err(RustdocJoinError::Invalid(format!(
            "authored source snapshot {key} has display path {}",
            source.file
        )));
    }
    Ok(source)
}

fn map_obligation_range(
    map: &RustdocMergedMap,
    entry: &RustdocMergedEntry,
    bundle_source: &str,
    start: u32,
    end: u32,
    authored_sources: &BTreeMap<String, RustCompilerSource>,
) -> Result<RustdocMappedRange, RustdocJoinError> {
    let source = authored_source(authored_sources, entry)?;
    map_merged_range(
        map,
        &entry.module,
        bundle_source,
        start,
        end,
        &source.source,
    )
}

fn alternative_discriminator(
    discriminator: &str,
    kind: &str,
    label: &str,
) -> Result<String, RustdocJoinError> {
    let token = match (kind, label) {
        ("decision-outcome", "condition false") => "false",
        ("decision-outcome", "condition true") => "true",
        ("assertion-outcome", "failed") => "failed",
        ("assertion-outcome", "passed") => "passed",
        ("loop-entry", "zero iterations") => "zero",
        ("loop-entry", "entered") => "entered",
        ("match-arm", "not selected") => "not-selected",
        ("match-arm", "selected") => "selected",
        ("let-else", "matched") => "matched",
        ("let-else", "else") => "else",
        ("try-operator", "continued") => "continued",
        ("try-operator", "early return") => "returned",
        _ => {
            return Err(RustdocJoinError::Invalid(format!(
                "unknown {} alternative label {label}",
                kind
            )));
        }
    };
    Ok(format!("{discriminator}:{token}"))
}

/// Resolve a merged rustdoc bundle only after its runner map and immutable
/// authored source snapshots are available. The returned ID/ordinal maps are
/// required to translate already-emitted bundle observations; accepting the
/// final manifest without translating those observations would silently lose
/// coverage.
pub fn join_merged_doctest(
    pending_manifest_bytes: &[u8],
    pending_source_bytes: &[u8],
    map_bytes: &[u8],
    authored_sources: &BTreeMap<String, RustCompilerSource>,
) -> Result<RustdocMergedJoin, RustdocJoinError> {
    let map = RustdocMergedMap::parse(map_bytes)?;
    let mut manifest =
        RustCompilerManifest::parse_pending_doctest(pending_manifest_bytes, &map.group)
            .map_err(|error| RustdocJoinError::Manifest(error.to_string()))?;
    let pending_sources =
        RustCompilerSourceSnapshots::parse_pending_doctest(pending_source_bytes, &map.group)
            .map_err(|error| RustdocJoinError::Manifest(error.to_string()))?;
    if pending_sources.crate_name != manifest.crate_name {
        return Err(RustdocJoinError::Invalid(
            "pending merged-doctest manifest/source crate mismatch".into(),
        ));
    }
    let pending_key = format!("doctest-pending:{}", map.group);
    let bundle_source = &pending_sources
        .sources
        .get(&pending_key)
        .expect("pending source parser requires the exact key")
        .source;
    let mut ids = BTreeMap::new();
    let mut ordinals = BTreeMap::new();

    for point in &mut manifest.points {
        let entry = definition_module(&map, &point.definitions)?;
        let rebased = rebase_identity(
            &map,
            entry,
            bundle_source,
            authored_sources,
            &point.kind,
            &point.source_key,
            point.start,
            point.end,
            &point.discriminator,
            &point.discriminator,
            &point.id,
            &point.canonical,
            &point.probe_ordinal,
        )?;
        insert_translation(
            &mut ids,
            &mut ordinals,
            &point.id,
            &point.probe_ordinal,
            &rebased.identity,
        )?;
        point.id = rebased.identity.id;
        point.canonical = rebased.identity.canonical;
        point.probe_ordinal = rebased.identity.probe_ordinal.to_string();
        point.source_key = rebased.source.source_key;
        point.start = rebased.source.start;
        point.end = rebased.source.end;
        point.provenance = rebased.provenance.into();
        point.definitions = stable_definitions(entry, &point.definitions)?;
    }

    let mut group_ids = BTreeMap::new();
    for group in &mut manifest.selection_groups {
        let entry = definition_module(&map, &group.definitions)?;
        let rebased = rebase_identity(
            &map,
            entry,
            bundle_source,
            authored_sources,
            "match-group",
            &group.source_key,
            group.start,
            group.end,
            "match",
            "match",
            &group.id,
            &group.canonical,
            &group.probe_ordinal,
        )?;
        insert_translation(
            &mut ids,
            &mut ordinals,
            &group.id,
            &group.probe_ordinal,
            &rebased.identity,
        )?;
        group_ids.insert(group.id.clone(), rebased.identity.id.clone());
        group.id = rebased.identity.id;
        group.canonical = rebased.identity.canonical;
        group.probe_ordinal = rebased.identity.probe_ordinal.to_string();
        group.source_key = rebased.source.source_key;
        group.start = rebased.source.start;
        group.end = rebased.source.end;
        group.provenance = rebased.provenance.into();
        group.definitions = stable_definitions(entry, &group.definitions)?;
        for arm in &mut group.arms {
            let source = map_obligation_range(
                &map,
                entry,
                bundle_source,
                arm.body_start,
                arm.body_end,
                authored_sources,
            )?;
            arm.body_source_key = source.source_key;
            arm.body_start = source.start;
            arm.body_end = source.end;
        }
    }

    let mut branch_ids = BTreeMap::new();
    for branch in &mut manifest.branches {
        let old_discriminator = branch.discriminator.clone();
        let old_source_key = branch.source_key.clone();
        let old_start = branch.start;
        let old_end = branch.end;
        let entry = definition_module(&map, &branch.definitions)?;
        let discriminator = if branch.kind == "match-arm" {
            let mut translated = old_discriminator.clone();
            for (old_group, new_group) in &group_ids {
                translated = translated.replace(old_group, new_group);
            }
            if translated == old_discriminator {
                return Err(RustdocJoinError::Invalid(format!(
                    "match-arm discriminator {} has no translated parent group",
                    old_discriminator
                )));
            }
            translated
        } else {
            old_discriminator.clone()
        };
        let rebased = rebase_identity(
            &map,
            entry,
            bundle_source,
            authored_sources,
            "branch",
            &old_source_key,
            old_start,
            old_end,
            &old_discriminator,
            &discriminator,
            &branch.id,
            &branch.canonical,
            &branch.probe_ordinal,
        )?;
        insert_translation(
            &mut ids,
            &mut ordinals,
            &branch.id,
            &branch.probe_ordinal,
            &rebased.identity,
        )?;
        branch_ids.insert(branch.id.clone(), rebased.identity.id.clone());
        branch.id = rebased.identity.id;
        branch.canonical = rebased.identity.canonical;
        branch.probe_ordinal = rebased.identity.probe_ordinal.to_string();
        branch.source_key = rebased.source.source_key.clone();
        branch.start = rebased.source.start;
        branch.end = rebased.source.end;
        branch.provenance = rebased.provenance.into();
        branch.definitions = stable_definitions(entry, &branch.definitions)?;
        branch.discriminator = discriminator.clone();
        for alternative in &mut branch.alternatives {
            let old_alternative_discriminator =
                alternative_discriminator(&old_discriminator, &branch.kind, &alternative.label)?;
            let new_discriminator =
                alternative_discriminator(&discriminator, &branch.kind, &alternative.label)?;
            let rebased = rebase_identity(
                &map,
                entry,
                bundle_source,
                authored_sources,
                "branch-alternative",
                &old_source_key,
                old_start,
                old_end,
                &old_alternative_discriminator,
                &new_discriminator,
                &alternative.id,
                &alternative.canonical,
                &alternative.probe_ordinal,
            )?;
            insert_translation(
                &mut ids,
                &mut ordinals,
                &alternative.id,
                &alternative.probe_ordinal,
                &rebased.identity,
            )?;
            alternative.id = rebased.identity.id;
            alternative.probe_ordinal = rebased.identity.probe_ordinal.to_string();
            alternative.canonical = rebased.identity.canonical;
        }
    }

    let mut decision_ids = BTreeMap::new();
    for decision in &mut manifest.decisions {
        let entry = definition_module(&map, &decision.definitions)?;
        let rebased = rebase_identity(
            &map,
            entry,
            bundle_source,
            authored_sources,
            "decision",
            &decision.source_key,
            decision.start,
            decision.end,
            &decision.kind,
            &decision.kind,
            &decision.id,
            &decision.canonical,
            &decision.probe_ordinal,
        )?;
        insert_translation(
            &mut ids,
            &mut ordinals,
            &decision.id,
            &decision.probe_ordinal,
            &rebased.identity,
        )?;
        decision_ids.insert(decision.id.clone(), rebased.identity.id.clone());
        decision.id = rebased.identity.id;
        decision.canonical = rebased.identity.canonical;
        decision.probe_ordinal = rebased.identity.probe_ordinal.to_string();
        decision.source_key = rebased.source.source_key;
        decision.start = rebased.source.start;
        decision.end = rebased.source.end;
        decision.provenance = rebased.provenance.into();
        decision.definitions = stable_definitions(entry, &decision.definitions)?;
        decision.outcome_branch_id = branch_ids
            .get(&decision.outcome_branch_id)
            .cloned()
            .ok_or_else(|| {
                RustdocJoinError::Invalid("decision outcome branch was not rebased".into())
            })?;
        decision.loop_branch_id = decision
            .loop_branch_id
            .as_ref()
            .map(|id| {
                branch_ids.get(id).cloned().ok_or_else(|| {
                    RustdocJoinError::Invalid("decision loop branch was not rebased".into())
                })
            })
            .transpose()?;
        for condition in &mut decision.conditions {
            let source = map_obligation_range(
                &map,
                entry,
                bundle_source,
                condition.start,
                condition.end,
                authored_sources,
            )?;
            condition.source_key = source.source_key;
            condition.start = source.start;
            condition.end = source.end;
        }
    }

    for group in &mut manifest.selection_groups {
        group.parent_group_id = group
            .parent_group_id
            .as_ref()
            .map(|id| {
                group_ids.get(id).cloned().ok_or_else(|| {
                    RustdocJoinError::Invalid("match parent group was not rebased".into())
                })
            })
            .transpose()?;
        for arm in &mut group.arms {
            arm.branch_id = branch_ids.get(&arm.branch_id).cloned().ok_or_else(|| {
                RustdocJoinError::Invalid("match arm branch was not rebased".into())
            })?;
            arm.guard_decision_id = arm
                .guard_decision_id
                .as_ref()
                .map(|id| {
                    decision_ids.get(id).cloned().ok_or_else(|| {
                        RustdocJoinError::Invalid("match guard decision was not rebased".into())
                    })
                })
                .transpose()?;
            arm.selected_ordinal = ordinals
                .get(&arm.selected_ordinal)
                .ok_or_else(|| {
                    RustdocJoinError::Invalid("match selected ordinal was not rebased".into())
                })?
                .clone();
            arm.not_selected_ordinal = ordinals
                .get(&arm.not_selected_ordinal)
                .ok_or_else(|| {
                    RustdocJoinError::Invalid("match not-selected ordinal was not rebased".into())
                })?
                .clone();
        }
    }

    manifest
        .points
        .sort_by(|left, right| left.id.cmp(&right.id));
    manifest
        .branches
        .sort_by(|left, right| left.id.cmp(&right.id));
    manifest
        .decisions
        .sort_by(|left, right| left.id.cmp(&right.id));
    manifest
        .selection_groups
        .sort_by(|left, right| left.id.cmp(&right.id));

    let required_keys = manifest
        .points
        .iter()
        .map(|point| point.source_key.as_str())
        .chain(
            manifest
                .branches
                .iter()
                .map(|branch| branch.source_key.as_str()),
        )
        .chain(manifest.decisions.iter().flat_map(|decision| {
            std::iter::once(decision.source_key.as_str()).chain(
                decision
                    .conditions
                    .iter()
                    .map(|condition| condition.source_key.as_str()),
            )
        }))
        .chain(manifest.selection_groups.iter().flat_map(|group| {
            std::iter::once(group.source_key.as_str())
                .chain(group.arms.iter().map(|arm| arm.body_source_key.as_str()))
        }))
        .collect::<BTreeSet<_>>();
    let sources = RustCompilerSourceSnapshots {
        schema: pending_sources.schema,
        crate_name: manifest.crate_name.clone(),
        sources: required_keys
            .into_iter()
            .map(|key| {
                authored_sources
                    .get(key)
                    .cloned()
                    .map(|source| (key.into(), source))
                    .ok_or_else(|| {
                        RustdocJoinError::Invalid(format!(
                            "final authored source snapshot {key} was not supplied"
                        ))
                    })
            })
            .collect::<Result<_, _>>()?,
    };
    manifest
        .validate()
        .map_err(|error| RustdocJoinError::Manifest(error.to_string()))?;
    sources
        .validate()
        .map_err(|error| RustdocJoinError::Manifest(error.to_string()))?;
    manifest
        .normalize(&sources.sources)
        .map_err(|error| RustdocJoinError::Manifest(error.to_string()))?;
    Ok(RustdocMergedJoin {
        manifest,
        sources,
        obligation_ids: ids,
        probe_ordinals: ordinals,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rust_compiler_manifest::{
        RustCompilerBranch, RustCompilerBranchAlternative, RustCompilerCondition,
        RustCompilerDecision, RustCompilerManifest, RustCompilerMatchArm, RustCompilerPoint,
        RustCompilerSelectionGroup, RustCompilerSourceSnapshots,
    };

    fn map() -> RustdocMergedMap {
        RustdocMergedMap::parse(
            br#"{
                "schema":"supercov-rustdoc-merged-map-v1",
                "group":"fixture",
                "entries":[
                    {"module":"__doctest_0","displayName":"src/lib.rs - (line 3)","path":"src/lib.rs","line":3},
                    {"module":"__doctest_1","displayName":"src/lib.rs - (line 10)","path":"src/lib.rs","line":10}
                ]
            }"#,
        )
        .expect("valid map")
    }

    fn merged_bundle(body: &str) -> String {
        format!(
            "\n#![allow(unused)]\npub mod __doctest_0 {{\nfn main() {{\n{body}\n}}\npub fn __main_fn() -> impl std::process::Termination {{ main() }}\n}}\n"
        )
    }

    #[test]
    fn map_is_strict_sorted_and_path_safe() {
        let valid = map();
        assert_eq!(valid.entry("__doctest_1").expect("entry").line, 10);
        for invalid in [
            br#"{"schema":"wrong","group":"fixture","entries":[]}"#.as_slice(),
            br#"{"schema":"supercov-rustdoc-merged-map-v1","group":"fixture","entries":[{"module":"__doctest_1","displayName":"one","path":"src/lib.rs","line":10},{"module":"__doctest_0","displayName":"zero","path":"src/lib.rs","line":3}]}"#.as_slice(),
            br#"{"schema":"supercov-rustdoc-merged-map-v1","group":"fixture","entries":[{"module":"__doctest_0","displayName":"bad","path":"../src/lib.rs","line":3}]}"#.as_slice(),
            br#"{"schema":"supercov-rustdoc-merged-map-v1","group":"fixture","entries":[{"module":"__doctest_0","displayName":"bad","path":"src/lib.rs","line":0,"extra":true}]}"#.as_slice(),
        ] {
            assert!(RustdocMergedMap::parse(invalid).is_err());
        }
    }

    #[test]
    fn maps_hidden_multiline_and_duplicate_later_doctests_exactly() {
        let map = map();
        let snippet = "let value = hidden\n    + 2;";
        let bundle = merged_bundle(snippet);
        let start = bundle.find(snippet).unwrap() as u32;
        let end = start + snippet.len() as u32;
        let authored = concat!(
            "//! docs\n",
            "//! ```\n",
            "//! # let hidden = 20;\n",
            "//! let value = hidden\n",
            "//!     + 2;\n",
            "//! assert_eq!(value, 22);\n",
            "//! ```\n",
            "//! more\n",
            "//! ```\n",
            "//! let value = hidden\n",
            "//!     + 2;\n",
            "//! ```\n",
        );
        let mapped = map_merged_range(&map, "__doctest_0", &bundle, start, end, authored)
            .expect("exact range");
        assert_eq!(mapped.source_key, "source:src/lib.rs");
        assert_eq!(
            &authored[mapped.start as usize..mapped.end as usize],
            "let value = hidden\n//!     + 2;"
        );
    }

    #[test]
    fn rejects_ambiguous_or_unmapped_bundle_ranges() {
        let map = map();
        let bundle = merged_bundle("same();");
        let start = bundle.find("same();").unwrap() as u32;
        let end = start + 7;
        let ambiguous = "//! docs\n//! ```\n//! same();\n//! same();\n//! ```\n";
        assert!(map_merged_range(&map, "__doctest_0", &bundle, start, end, ambiguous).is_err());
        assert!(map_merged_range(&map, "__doctest_9", &bundle, start, end, ambiguous).is_err());
        assert!(map_merged_range(&map, "__doctest_0", &bundle, end, start, ambiguous).is_err());
    }

    #[test]
    fn maps_repeated_fragments_when_the_full_sequence_is_unique() {
        let map = map();
        let snippet = "same();\nsame();";
        let bundle = merged_bundle(snippet);
        let start = bundle.find(snippet).unwrap() as u32;
        let end = start + snippet.len() as u32;
        let authored = concat!(
            "//! docs\n",
            "//! ```\n",
            "//! same();\n",
            "//! same();\n",
            "//! ```\n",
        );
        let mapped = map_merged_range(&map, "__doctest_0", &bundle, start, end, authored)
            .expect("one ordered mapping");
        assert_eq!(
            &authored[mapped.start as usize..mapped.end as usize],
            "same();\n//! same();"
        );
    }

    #[test]
    fn maps_repeated_subexpressions_through_their_extracted_line_context() {
        let map = map();
        let snippet = concat!(
            "let flag = true;\n",
            "if flag { yes(); } else { no(); }\n",
            "match flag { true => yes(), false => no() };",
        );
        let bundle = merged_bundle(snippet);
        let if_line = "if flag { yes(); } else { no(); }";
        let start = bundle.find(if_line).unwrap() + "if ".len();
        let end = start + "flag".len();
        let authored = concat!(
            "//! docs\n",
            "//! ```\n",
            "//! let flag = true;\n",
            "//! if flag { yes(); } else { no(); }\n",
            "//! match flag { true => yes(), false => no() };\n",
            "//! ```\n",
        );
        let mapped = map_merged_range(
            &map,
            "__doctest_0",
            &bundle,
            start as u32,
            end as u32,
            authored,
        )
        .expect("full extracted-line context disambiguates flag");
        assert_eq!(
            &authored[mapped.start as usize..mapped.end as usize],
            "flag"
        );
        assert_eq!(
            authored[..mapped.start as usize]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count(),
            3,
            "the mapped flag must come from the if line"
        );
    }

    #[test]
    fn final_identity_matches_the_frozen_rust_source_model() {
        let source = RustdocMappedRange {
            source_key: "source:src/lib.rs".into(),
            start: 42,
            end: 57,
        };
        let identity =
            rust_source_identity("statement", &source, "expression").expect("valid identity");
        assert_eq!(
            identity.canonical,
            "rust-source-v1\0statement\0source:src/lib.rs\x0042\x0057\0expression\0"
        );
        assert_eq!(identity.id, "rs:statement:8446ba638fcb36ffc76b4293");
        assert_eq!(identity.probe_ordinal, 9531510598153221887);
    }

    fn pending_assertion_candidate() -> (
        Vec<u8>,
        Vec<u8>,
        Vec<u8>,
        BTreeMap<String, RustCompilerSource>,
    ) {
        let group = "fixture";
        let key = format!("doctest-pending:{group}");
        let snippet = "assert_eq!(fixture::authored(true), 1)";
        let bundle = format!(
            "\n#![allow(unused)]\npub mod __doctest_0 {{\nfn main() {{\n{snippet};\n}}\n}}\n"
        );
        let start = u32::try_from(bundle.find(snippet).expect("snippet")).unwrap();
        let end = start + u32::try_from(snippet.len()).unwrap();
        let definition = vec!["__doctest_0::main".into()];
        let point_identity = pending_identity("statement", &key, start, end, "expression").unwrap();
        let branch_identity =
            pending_identity("branch", &key, start, end, "assertion-outcome:assertion").unwrap();
        let passed_identity = pending_identity(
            "branch-alternative",
            &key,
            start,
            end,
            "assertion-outcome:assertion:passed",
        )
        .unwrap();
        let failed_identity = pending_identity(
            "branch-alternative",
            &key,
            start,
            end,
            "assertion-outcome:assertion:failed",
        )
        .unwrap();
        let decision_identity =
            pending_identity("decision", &key, start, end, "assertion").unwrap();
        let manifest = RustCompilerManifest {
            schema: "supercov-rust-manifest-candidate-v2".into(),
            model: "rust-source-v1".into(),
            crate_name: "doctest_bundle_2024".into(),
            measurement_complete: false,
            points: vec![RustCompilerPoint {
                id: point_identity.id,
                kind: "statement".into(),
                source_key: key.clone(),
                start,
                end,
                provenance: "doctest-pending".into(),
                discriminator: "expression".into(),
                probe_ordinal: point_identity.probe_ordinal.to_string(),
                definitions: definition.clone(),
                canonical: point_identity.canonical,
            }],
            branches: vec![RustCompilerBranch {
                id: branch_identity.id.clone(),
                kind: "assertion-outcome".into(),
                discriminator: "assertion-outcome:assertion".into(),
                source_key: key.clone(),
                start,
                end,
                provenance: "doctest-pending".into(),
                probe_ordinal: branch_identity.probe_ordinal.to_string(),
                definitions: definition.clone(),
                alternatives: vec![
                    RustCompilerBranchAlternative {
                        id: passed_identity.id,
                        label: "passed".into(),
                        probe_ordinal: passed_identity.probe_ordinal.to_string(),
                        canonical: passed_identity.canonical,
                    },
                    RustCompilerBranchAlternative {
                        id: failed_identity.id,
                        label: "failed".into(),
                        probe_ordinal: failed_identity.probe_ordinal.to_string(),
                        canonical: failed_identity.canonical,
                    },
                ],
                canonical: branch_identity.canonical,
            }],
            decisions: vec![RustCompilerDecision {
                id: decision_identity.id,
                kind: "assertion".into(),
                source_key: key.clone(),
                start,
                end,
                provenance: "doctest-pending".into(),
                probe_ordinal: decision_identity.probe_ordinal.to_string(),
                definitions: definition,
                outcome_branch_id: branch_identity.id,
                loop_branch_id: None,
                conditions: vec![RustCompilerCondition {
                    source_key: key.clone(),
                    start,
                    end,
                    source: snippet.into(),
                }],
                canonical: decision_identity.canonical,
            }],
            selection_groups: Vec::new(),
            limitations: vec!["RUST_DOCTEST_MAPPING_PENDING".into()],
        };
        let snapshots = RustCompilerSourceSnapshots {
            schema: "supercov-rust-source-snapshots-v1".into(),
            crate_name: manifest.crate_name.clone(),
            sources: BTreeMap::from([(
                key.clone(),
                RustCompilerSource {
                    file: key,
                    source: bundle,
                },
            )]),
        };
        let map = br#"{
            "schema":"supercov-rustdoc-merged-map-v1",
            "group":"fixture",
            "entries":[{
                "module":"__doctest_0",
                "displayName":"src/lib.rs - (line 3)",
                "path":"src/lib.rs",
                "line":3
            }]
        }"#
        .to_vec();
        let authored = concat!(
            "//! docs\n",
            "//! ```\n",
            "//! assert_eq!(fixture::authored(true), 1);\n",
            "//! ```\n",
        );
        (
            serde_json::to_vec(&manifest).unwrap(),
            serde_json::to_vec(&snapshots).unwrap(),
            map,
            BTreeMap::from([(
                "source:src/lib.rs".into(),
                RustCompilerSource {
                    file: "src/lib.rs".into(),
                    source: authored.into(),
                },
            )]),
        )
    }

    fn pending_branch(
        key: &str,
        start: u32,
        end: u32,
        kind: &str,
        discriminator: &str,
        alternatives: [(&str, &str); 2],
    ) -> RustCompilerBranch {
        let identity = pending_identity("branch", key, start, end, discriminator).unwrap();
        RustCompilerBranch {
            id: identity.id,
            kind: kind.into(),
            discriminator: discriminator.into(),
            source_key: key.into(),
            start,
            end,
            provenance: "doctest-pending".into(),
            probe_ordinal: identity.probe_ordinal.to_string(),
            definitions: vec!["__doctest_0::main".into()],
            alternatives: alternatives
                .into_iter()
                .map(|(token, label)| {
                    let identity = pending_identity(
                        "branch-alternative",
                        key,
                        start,
                        end,
                        &format!("{discriminator}:{token}"),
                    )
                    .unwrap();
                    RustCompilerBranchAlternative {
                        id: identity.id,
                        label: label.into(),
                        probe_ordinal: identity.probe_ordinal.to_string(),
                        canonical: identity.canonical,
                    }
                })
                .collect(),
            canonical: identity.canonical,
        }
    }

    fn synthetic_pending_identity(
        kind: &str,
        key: &str,
        start: u32,
        end: u32,
        discriminator: &str,
        owner_ordinal: u64,
    ) -> RustSourceIdentity {
        identity_from_canonical(
            kind,
            format!(
                concat!(
                    "rust-source-v1\0{}\0{}\0{}\0{}\0{}\0",
                    "synthetic-expansion\0proc-macro\0{}\0{}\0{}\0probe_macros::generated\0",
                    "__doctest_0::main\0{}\0"
                ),
                kind, key, start, end, discriminator, key, start, end, owner_ordinal,
            ),
        )
        .unwrap()
    }

    #[test]
    fn joins_pending_bundle_manifest_into_final_authored_identities() {
        let (manifest, sources, map, authored) = pending_assertion_candidate();
        assert!(RustCompilerManifest::parse(&manifest).is_err());
        assert!(RustCompilerSourceSnapshots::parse(&sources).is_err());

        let joined =
            join_merged_doctest(&manifest, &sources, &map, &authored).expect("strict merged join");
        assert_eq!(joined.manifest.points.len(), 1);
        assert_eq!(joined.manifest.branches.len(), 1);
        assert_eq!(joined.manifest.decisions.len(), 1);
        assert_eq!(joined.obligation_ids.len(), 5);
        assert_eq!(joined.probe_ordinals.len(), 5);
        assert_eq!(joined.manifest.points[0].source_key, "source:src/lib.rs");
        assert_eq!(joined.manifest.branches[0].source_key, "source:src/lib.rs");
        assert_eq!(joined.manifest.decisions[0].source_key, "source:src/lib.rs");
        let point = &joined.manifest.points[0];
        let source = &authored["source:src/lib.rs"].source;
        assert_eq!(
            &source[point.start as usize..point.end as usize],
            "assert_eq!(fixture::authored(true), 1)"
        );
        assert_eq!(point.provenance, "doctest-source");
        assert_eq!(point.definitions, ["doctest:src/lib.rs:3"]);
        assert_eq!(joined.sources.sources.len(), 1);
        joined
            .manifest
            .normalize(&joined.sources.sources)
            .expect("final manifest normalizes through the production path");
    }

    #[test]
    fn merged_join_rejects_tampering_missing_sources_and_malformed_synthetic_expansion() {
        let (manifest, sources, map, authored) = pending_assertion_candidate();
        let mut tampered: serde_json::Value = serde_json::from_slice(&manifest).unwrap();
        tampered["points"][0]["id"] =
            serde_json::Value::String("rs:statement:000000000000000000000000".into());
        assert!(
            join_merged_doctest(
                &serde_json::to_vec(&tampered).unwrap(),
                &sources,
                &map,
                &authored,
            )
            .is_err()
        );

        assert!(join_merged_doctest(&manifest, &sources, &map, &BTreeMap::new(),).is_err());

        let mut synthetic: serde_json::Value = serde_json::from_slice(&manifest).unwrap();
        synthetic["points"][0]["canonical"] = serde_json::Value::String(
            concat!(
                "rust-source-v1\0statement\0doctest-pending:fixture\0",
                "1\0",
                "2\0expression\0synthetic-expansion\0"
            )
            .into(),
        );
        assert!(
            join_merged_doctest(
                &serde_json::to_vec(&synthetic).unwrap(),
                &sources,
                &map,
                &authored,
            )
            .is_err()
        );
    }

    #[test]
    fn rebases_decision_match_cross_references_and_runtime_ordinals() {
        let key = "doctest-pending:fixture";
        let body = concat!(
            "let flag = true;\n",
            "if flag { yes(); } else { no(); }\n",
            "match flag { true => yes(), false => no() };",
        );
        let bundle = merged_bundle(body);
        let range = |fragment: &str| {
            let start = bundle.find(fragment).unwrap() as u32;
            (start, start + fragment.len() as u32)
        };
        let point_range = range("let flag = true;");
        let if_range = range("if flag { yes(); } else { no(); }");
        let if_flag_start = if_range.0 + "if ".len() as u32;
        let if_flag_end = if_flag_start + "flag".len() as u32;
        let match_range = range("match flag { true => yes(), false => no() }");
        let first_arm_range = range("true => yes()");
        let second_arm_range = range("false => no()");
        let match_start = match_range.0 as usize;
        let first_body_start = match_start + bundle[match_start..].find("yes()").unwrap();
        let second_body_start = match_start + bundle[match_start..].find("no()").unwrap();
        let first_body_range = (
            first_body_start as u32,
            (first_body_start + "yes()".len()) as u32,
        );
        let second_body_range = (
            second_body_start as u32,
            (second_body_start + "no()".len()) as u32,
        );

        let point_identity =
            pending_identity("statement", key, point_range.0, point_range.1, "let").unwrap();
        let decision_identity =
            pending_identity("decision", key, if_flag_start, if_flag_end, "if").unwrap();
        let outcome = pending_branch(
            key,
            if_range.0,
            if_range.1,
            "decision-outcome",
            "decision-outcome:if",
            [("true", "condition true"), ("false", "condition false")],
        );
        let group_identity =
            pending_identity("match-group", key, match_range.0, match_range.1, "match").unwrap();
        let first_arm = pending_branch(
            key,
            first_arm_range.0,
            first_arm_range.1,
            "match-arm",
            &format!("match-arm:{}:0", group_identity.id),
            [("not-selected", "not selected"), ("selected", "selected")],
        );
        let second_arm = pending_branch(
            key,
            second_arm_range.0,
            second_arm_range.1,
            "match-arm",
            &format!("match-arm:{}:1", group_identity.id),
            [("not-selected", "not selected"), ("selected", "selected")],
        );
        let arm_ordinals = |branch: &RustCompilerBranch| {
            let ordinal = |label: &str| {
                branch
                    .alternatives
                    .iter()
                    .find(|alternative| alternative.label == label)
                    .unwrap()
                    .probe_ordinal
                    .clone()
            };
            (ordinal("selected"), ordinal("not selected"))
        };
        let first_ordinals = arm_ordinals(&first_arm);
        let second_ordinals = arm_ordinals(&second_arm);
        let mut branches = vec![outcome.clone(), first_arm.clone(), second_arm.clone()];
        branches.sort_by(|left, right| left.id.cmp(&right.id));
        let manifest = RustCompilerManifest {
            schema: "supercov-rust-manifest-candidate-v2".into(),
            model: "rust-source-v1".into(),
            crate_name: "doctest_bundle_2024".into(),
            measurement_complete: false,
            points: vec![RustCompilerPoint {
                id: point_identity.id,
                kind: "statement".into(),
                source_key: key.into(),
                start: point_range.0,
                end: point_range.1,
                provenance: "doctest-pending".into(),
                discriminator: "let".into(),
                probe_ordinal: point_identity.probe_ordinal.to_string(),
                definitions: vec!["__doctest_0::main".into()],
                canonical: point_identity.canonical,
            }],
            branches,
            decisions: vec![RustCompilerDecision {
                id: decision_identity.id,
                kind: "if".into(),
                source_key: key.into(),
                start: if_flag_start,
                end: if_flag_end,
                provenance: "doctest-pending".into(),
                probe_ordinal: decision_identity.probe_ordinal.to_string(),
                definitions: vec!["__doctest_0::main".into()],
                outcome_branch_id: outcome.id,
                loop_branch_id: None,
                conditions: vec![RustCompilerCondition {
                    source_key: key.into(),
                    start: if_flag_start,
                    end: if_flag_end,
                    source: "flag".into(),
                }],
                canonical: decision_identity.canonical,
            }],
            selection_groups: vec![RustCompilerSelectionGroup {
                id: group_identity.id,
                kind: "match".into(),
                source_key: key.into(),
                start: match_range.0,
                end: match_range.1,
                provenance: "doctest-pending".into(),
                probe_ordinal: group_identity.probe_ordinal.to_string(),
                definitions: vec!["__doctest_0::main".into()],
                parent_group_id: None,
                parent_site: None,
                parent_arm_index: None,
                arms: vec![
                    RustCompilerMatchArm {
                        branch_id: first_arm.id,
                        body_source_key: key.into(),
                        body_start: first_body_range.0,
                        body_end: first_body_range.1,
                        guarded: false,
                        guard_decision_id: None,
                        selected_ordinal: first_ordinals.0,
                        not_selected_ordinal: first_ordinals.1,
                    },
                    RustCompilerMatchArm {
                        branch_id: second_arm.id,
                        body_source_key: key.into(),
                        body_start: second_body_range.0,
                        body_end: second_body_range.1,
                        guarded: false,
                        guard_decision_id: None,
                        selected_ordinal: second_ordinals.0,
                        not_selected_ordinal: second_ordinals.1,
                    },
                ],
                canonical: group_identity.canonical,
            }],
            limitations: vec!["RUST_DOCTEST_MAPPING_PENDING".into()],
        };
        let snapshots = RustCompilerSourceSnapshots {
            schema: "supercov-rust-source-snapshots-v1".into(),
            crate_name: manifest.crate_name.clone(),
            sources: BTreeMap::from([(
                key.into(),
                RustCompilerSource {
                    file: key.into(),
                    source: bundle,
                },
            )]),
        };
        let authored = concat!(
            "//! docs\n",
            "//! ```\n",
            "//! let flag = true;\n",
            "//! if flag { yes(); } else { no(); }\n",
            "//! match flag { true => yes(), false => no() };\n",
            "//! ```\n",
        );
        let map = br#"{
            "schema":"supercov-rustdoc-merged-map-v1",
            "group":"fixture",
            "entries":[{
                "module":"__doctest_0",
                "displayName":"src/lib.rs - (line 3)",
                "path":"src/lib.rs",
                "line":3
            }]
        }"#;
        let joined = join_merged_doctest(
            &serde_json::to_vec(&manifest).unwrap(),
            &serde_json::to_vec(&snapshots).unwrap(),
            map,
            &BTreeMap::from([(
                "source:src/lib.rs".into(),
                RustCompilerSource {
                    file: "src/lib.rs".into(),
                    source: authored.into(),
                },
            )]),
        )
        .expect("decision and match join");

        let decision = &joined.manifest.decisions[0];
        assert!(
            joined
                .manifest
                .branches
                .iter()
                .any(|branch| branch.id == decision.outcome_branch_id)
        );
        let group = &joined.manifest.selection_groups[0];
        assert!(group.id.starts_with("rs:match-group:"));
        for arm in &group.arms {
            let branch = joined
                .manifest
                .branches
                .iter()
                .find(|branch| branch.id == arm.branch_id)
                .unwrap();
            assert!(branch.discriminator.contains(&group.id));
            assert!(
                branch
                    .alternatives
                    .iter()
                    .any(|alternative| alternative.probe_ordinal == arm.selected_ordinal)
            );
            assert!(
                branch
                    .alternatives
                    .iter()
                    .any(|alternative| alternative.probe_ordinal == arm.not_selected_ordinal)
            );
        }
        assert!(
            joined.manifest.decisions[0]
                .conditions
                .iter()
                .all(|condition| condition.source_key == "source:src/lib.rs")
        );
        assert_eq!(joined.obligation_ids.len(), 12);
        assert_eq!(joined.probe_ordinals.len(), 12);
    }

    #[test]
    fn rebases_complete_synthetic_expansion_canonicals_without_guessing() {
        let (manifest, sources, map, authored) = pending_assertion_candidate();
        let mut manifest = RustCompilerManifest::parse_pending_doctest(&manifest, "fixture")
            .expect("pending candidate");
        let key = "doctest-pending:fixture";
        let mut owner_ordinal = 1;
        let mut replace = |kind: &str,
                           start: u32,
                           end: u32,
                           discriminator: &str,
                           id: &mut String,
                           canonical: &mut String,
                           ordinal: &mut String| {
            let identity =
                synthetic_pending_identity(kind, key, start, end, discriminator, owner_ordinal);
            owner_ordinal += 1;
            *id = identity.id;
            *canonical = identity.canonical;
            *ordinal = identity.probe_ordinal.to_string();
        };
        for point in &mut manifest.points {
            replace(
                &point.kind,
                point.start,
                point.end,
                &point.discriminator,
                &mut point.id,
                &mut point.canonical,
                &mut point.probe_ordinal,
            );
        }
        for branch in &mut manifest.branches {
            replace(
                "branch",
                branch.start,
                branch.end,
                &branch.discriminator,
                &mut branch.id,
                &mut branch.canonical,
                &mut branch.probe_ordinal,
            );
            for alternative in &mut branch.alternatives {
                let discriminator = alternative_discriminator(
                    &branch.discriminator,
                    &branch.kind,
                    &alternative.label,
                )
                .unwrap();
                replace(
                    "branch-alternative",
                    branch.start,
                    branch.end,
                    &discriminator,
                    &mut alternative.id,
                    &mut alternative.canonical,
                    &mut alternative.probe_ordinal,
                );
            }
        }
        let branch_id = manifest.branches[0].id.clone();
        for decision in &mut manifest.decisions {
            replace(
                "decision",
                decision.start,
                decision.end,
                &decision.kind,
                &mut decision.id,
                &mut decision.canonical,
                &mut decision.probe_ordinal,
            );
            decision.outcome_branch_id = branch_id.clone();
        }
        manifest
            .points
            .sort_by(|left, right| left.id.cmp(&right.id));
        manifest
            .branches
            .sort_by(|left, right| left.id.cmp(&right.id));
        manifest
            .decisions
            .sort_by(|left, right| left.id.cmp(&right.id));

        let joined = join_merged_doctest(
            &serde_json::to_vec(&manifest).unwrap(),
            &sources,
            &map,
            &authored,
        )
        .expect("synthetic expansion join");
        assert!(
            joined
                .manifest
                .points
                .iter()
                .all(|point| point.provenance == "synthetic-expansion")
        );
        assert!(
            joined
                .manifest
                .branches
                .iter()
                .all(|branch| branch.provenance == "synthetic-expansion"
                    && branch.alternatives.iter().all(|alternative| {
                        alternative.canonical.contains("source:src/lib.rs")
                            && !alternative.canonical.contains("doctest-pending:")
                    }))
        );
        assert!(
            joined
                .manifest
                .decisions
                .iter()
                .all(|decision| decision.provenance == "synthetic-expansion")
        );
        for canonical in joined
            .manifest
            .points
            .iter()
            .map(|point| &point.canonical)
            .chain(joined.manifest.branches.iter().flat_map(|branch| {
                std::iter::once(&branch.canonical).chain(
                    branch
                        .alternatives
                        .iter()
                        .map(|alternative| &alternative.canonical),
                )
            }))
            .chain(
                joined
                    .manifest
                    .decisions
                    .iter()
                    .map(|decision| &decision.canonical),
            )
        {
            assert!(canonical.contains("doctest:src/lib.rs:3"));
            assert!(!canonical.contains("__doctest_0"));
            assert!(!canonical.contains("doctest-pending:"));
        }
        assert_eq!(joined.obligation_ids.len(), 5);
        assert_eq!(joined.probe_ordinals.len(), 5);
    }
}
