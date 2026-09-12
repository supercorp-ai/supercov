//! Agent-authored assertion maps. Edges are explanations, never inferred proofs.
//! This module owns format validation, text relocation and review bookkeeping.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub type Files = BTreeMap<String, String>;
pub fn digest(value: &impl Serialize) -> String {
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(value).expect("serializable map"))
    )
}
fn version() -> u32 {
    1
}

/// One-based lines and UTF-8 byte columns, for every language. Text is exact.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Anchor {
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub text: String,
}

pub fn local_path(file: &str) -> bool {
    !file.is_empty()
        && !file.contains(['\\', ':'])
        && file.split('/').all(|p| !matches!(p, "" | "." | ".."))
}
impl Anchor {
    pub fn new(file: &str, source: &str, start: usize, end: usize) -> Self {
        Self {
            file: file.into(),
            line: source[..start].bytes().filter(|b| *b == b'\n').count() + 1,
            column: start - source[..start].rfind('\n').map_or(0, |n| n + 1) + 1,
            text: source[start..end].into(),
        }
    }
    pub fn offset(&self, files: &Files) -> Option<usize> {
        if !local_path(&self.file) || self.text.is_empty() || self.line == 0 || self.column == 0 {
            return None;
        }
        let source = files.get(&self.file)?;
        let start = source
            .split_inclusive('\n')
            .take(self.line - 1)
            .map(str::len)
            .sum::<usize>();
        if source[..start].bytes().filter(|b| *b == b'\n').count() != self.line - 1 {
            return None;
        }
        let line = source.get(start..)?.split('\n').next()?;
        if self.column - 1 > line.len() {
            return None;
        }
        let pos = start.checked_add(self.column - 1)?;
        source.get(pos..)?.starts_with(&self.text).then_some(pos)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InventorySite {
    pub at: Anchor,
    pub operation: String,
}

/// Source text held in memory for capture or a verified current-checkout query.
/// The serialized form is retained only for reading legacy source archives.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Inputs {
    #[serde(default = "version")]
    pub schema_version: u32,
    pub language: String,
    pub context_digest: String,
    pub files: Files,
    pub assertions: Vec<InventorySite>,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileFingerprint {
    pub sha256: String,
    pub bytes: usize,
}
impl FileFingerprint {
    pub fn of(source: &str) -> Self {
        Self {
            sha256: format!("{:x}", Sha256::digest(source.as_bytes())),
            bytes: source.len(),
        }
    }
}
pub type FileManifest = BTreeMap<String, FileFingerprint>;

/// The run stores identities and hashes, never complete source files.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InputManifest {
    pub schema_version: u32,
    pub language: String,
    pub context_digest: String,
    pub files: FileManifest,
    pub assertions: Vec<InventorySite>,
    pub limitations: Vec<String>,
}
impl Inputs {
    pub fn manifest(&self) -> InputManifest {
        InputManifest {
            schema_version: 2,
            language: self.language.clone(),
            context_digest: self.context_digest.clone(),
            files: self
                .files
                .iter()
                .map(|(p, s)| (p.clone(), FileFingerprint::of(s)))
                .collect(),
            assertions: self.assertions.clone(),
            limitations: self.limitations.clone(),
        }
    }
    pub fn identity(&self) -> String {
        digest(&self.manifest())
    }
}
impl InputManifest {
    pub fn with_sources(&self, files: Files) -> Inputs {
        Inputs {
            schema_version: 1,
            language: self.language.clone(),
            context_digest: self.context_digest.clone(),
            files,
            assertions: self.assertions.clone(),
            limitations: self.limitations.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Analysis {
    #[default]
    Unmapped,
    Partial,
    Mapped,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Node {
    pub id: String,
    pub at: Anchor,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub role: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub meaning: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub basis: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum Watch {
    File { file: String },
    Span { at: Anchor },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Flow {
    pub id: String,
    pub explanation: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applies_to: Vec<String>,
    pub nodes: Vec<Node>,
    #[serde(default)]
    pub edges: Vec<Edge>,
    pub counts_as_asserted: Vec<String>,
    pub watch: Vec<Watch>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Assertion {
    pub id: String,
    pub at: Anchor,
    #[serde(default)]
    pub analysis: Analysis,
    #[serde(default)]
    pub observes: Vec<String>,
    #[serde(default)]
    pub flows: Vec<Flow>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Retired {
    pub assertion: Assertion,
    pub reason: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssertionMap {
    #[serde(default = "version")]
    #[schemars(range(min = 1, max = 1))]
    pub schema_version: u32,
    pub assertions: Vec<Assertion>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retired_assertions: Vec<Retired>,
}

/// Editor schema generated from the same Rust types used by every map command.
/// Source existence, links, freshness and semantic meaning are outside JSON Schema.
pub fn schema() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(AssertionMap)).expect("schema")
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParseError {
    pub pointer: String,
    pub line: usize,
    pub column: usize,
    pub message: String,
}
impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} at {} (JSON line {}, column {})",
            self.message, self.pointer, self.line, self.column
        )
    }
}
pub fn parse(bytes: &[u8]) -> Result<AssertionMap, ParseError> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let map: AssertionMap = serde_path_to_error::deserialize(&mut deserializer).map_err(|e| {
        let pointer = e
            .path()
            .iter()
            .map(|segment| {
                use serde_path_to_error::Segment;
                let part = match segment {
                    Segment::Seq { index } => index.to_string(),
                    Segment::Map { key } => key.clone(),
                    Segment::Enum { variant } => variant.clone(),
                    Segment::Unknown => "?".into(),
                };
                format!("/{}", part.replace('~', "~0").replace('/', "~1"))
            })
            .collect();
        ParseError {
            pointer,
            line: e.inner().line(),
            column: e.inner().column(),
            message: e.inner().to_string(),
        }
    })?;
    deserializer.end().map_err(|e| ParseError {
        pointer: String::new(),
        line: e.line(),
        column: e.column(),
        message: e.to_string(),
    })?;
    if map.schema_version != 1 {
        return Err(ParseError {
            pointer: "/schemaVersion".into(),
            line: 0,
            column: 0,
            message: "unsupported map schema version; expected 1".into(),
        });
    }
    Ok(map)
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Review {
    pub fingerprint: String,
    pub reasons: BTreeSet<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct State {
    pub schema_version: u32,
    pub inputs_digest: String,
    pub evidence_digest: String,
    pub reviews: BTreeMap<String, Review>,
    pub scope_review: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inheritance: Option<Inheritance>,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Inheritance {
    pub from: Option<String>,
    pub skipped: Vec<SkippedMap>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkippedMap {
    pub run: String,
    pub reason: String,
}
pub fn flow_key(a: &Assertion, f: &Flow) -> String {
    format!("{}/{}", a.id, f.id)
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

pub fn seed(inputs: &Inputs, evidence_digest: &str) -> (AssertionMap, State) {
    seed_manifest(&inputs.manifest(), evidence_digest)
}
pub fn seed_manifest(inputs: &InputManifest, evidence_digest: &str) -> (AssertionMap, State) {
    (
        AssertionMap {
            schema_version: 1,
            assertions: inputs
                .assertions
                .iter()
                .map(|site| Assertion {
                    id: format!("a_{}", &digest(&site.at)[..20]),
                    at: site.at.clone(),
                    analysis: Analysis::Unmapped,
                    observes: vec![],
                    flows: vec![],
                })
                .collect(),
            retired_assertions: vec![],
        },
        State {
            schema_version: 2,
            inputs_digest: digest(inputs),
            evidence_digest: evidence_digest.into(),
            reviews: BTreeMap::new(),
            scope_review: BTreeSet::new(),
            inheritance: None,
        },
    )
}

/// Structural checks only; malformed entries cannot silently earn credit.
pub fn validate(map: &AssertionMap, inputs: &Inputs) -> Vec<String> {
    let mut errors = Vec::new();
    if map.schema_version != 1 || inputs.schema_version != 1 {
        errors.push("unsupported schema version".into());
    }
    let mut ids = BTreeSet::new();
    let mut sites = BTreeSet::new();
    for a in &map.assertions {
        if !valid_id(&a.id) || !ids.insert(&a.id) {
            errors.push(format!("{}: invalid/duplicate assertion ID", a.id));
        }
        if !sites.insert(&a.at) {
            errors.push(format!("{}: duplicate assertion location", a.id));
        }
        if a.at.offset(&inputs.files).is_none() {
            errors.push(format!("{}: invalid assertion anchor", a.id));
        }
        // Agents can register custom assertions absent from the syntax inventory.
        // They still require exact run evidence to earn execution-backed credit.
        let mut flows = BTreeSet::new();
        for f in &a.flows {
            let key = flow_key(a, f);
            if !valid_id(&f.id) || !flows.insert(&f.id) {
                errors.push(format!("{key}: invalid/duplicate flow ID"));
            }
            errors.extend(
                validate_flow(f, &inputs.files)
                    .into_iter()
                    .map(|e| format!("{key}: {e}")),
            );
        }
    }
    errors
}
pub fn validate_flow(flow: &Flow, files: &Files) -> Vec<String> {
    let mut errors = Vec::new();
    let mut nodes = BTreeSet::new();
    for node in &flow.nodes {
        if !valid_id(&node.id) || !nodes.insert(&node.id) {
            errors.push("invalid/duplicate node ID".into());
        }
        if node.at.offset(files).is_none() {
            errors.push(format!("node {}: invalid anchor", node.id));
        }
    }
    for edge in &flow.edges {
        if !nodes.contains(&edge.from) || !nodes.contains(&edge.to) {
            errors.push("dangling edge".into());
        }
    }
    if flow.counts_as_asserted.iter().any(|id| !nodes.contains(id)) {
        errors.push("unknown counted node".into());
    }
    if flow.explanation.trim().is_empty() {
        errors.push("missing explanation".into());
    }
    if flow.watch.is_empty() {
        errors.push("no review inputs declared".into());
    }
    for watch in &flow.watch {
        match watch {
            Watch::File { file } if !local_path(file) || !files.contains_key(file) => {
                errors.push(format!("watched file missing: {file}"))
            }
            Watch::Span { at } if at.offset(files).is_none() => {
                errors.push("invalid watched span".into())
            }
            _ => (),
        }
    }
    errors
}

/// Whole-file dependencies include the assertion's test and every flow node.
/// Span watches still identify review context, but hashes invalidate whole files.
pub fn dependencies<'a>(a: &'a Assertion, f: &'a Flow) -> BTreeSet<&'a str> {
    std::iter::once(a.at.file.as_str())
        .chain(f.nodes.iter().map(|n| n.at.file.as_str()))
        .chain(f.watch.iter().map(|w| match w {
            Watch::File { file } => file.as_str(),
            Watch::Span { at } => at.file.as_str(),
        }))
        .collect()
}

pub fn fingerprint(a: &Assertion, f: &Flow, files: &Files) -> String {
    let hashes = dependencies(a, f)
        .into_iter()
        .filter_map(|p| files.get(p).map(|s| (p.to_owned(), FileFingerprint::of(s))))
        .collect();
    fingerprint_manifest(a, f, &hashes)
}
fn fingerprint_manifest(a: &Assertion, f: &Flow, files: &FileManifest) -> String {
    let value = (&a.id, &a.at, &a.analysis, &a.observes, f);
    let hashes = dependencies(a, f)
        .into_iter()
        .map(|p| (p, files.get(p)))
        .collect::<Vec<_>>();
    digest(&(value, hashes))
}
pub fn reasons(a: &Assertion, f: &Flow, state: &State, inputs: &Inputs) -> BTreeSet<String> {
    let mut reasons = state
        .reviews
        .get(&flow_key(a, f))
        .map(|r| r.reasons.clone())
        .unwrap_or_default();
    if state.schema_version != 2 {
        reasons.insert("unsupported review state".into());
    }
    if state
        .reviews
        .get(&flow_key(a, f))
        .is_none_or(|r| r.fingerprint != fingerprint(a, f, &inputs.files))
    {
        reasons.insert("unreviewed map or changed review inputs".into());
    }
    reasons.extend(validate_flow(f, &inputs.files));
    if a.at.offset(&inputs.files).is_none() {
        reasons.insert("invalid assertion anchor".into());
    }
    reasons
}
pub fn review(
    map: &AssertionMap,
    state: &mut State,
    inputs: &Inputs,
    selected: &BTreeSet<String>,
    all: bool,
    ack_scope: bool,
) -> Result<(), String> {
    if map.schema_version != 1 {
        return Err("unsupported map schema version".into());
    }
    if state.inputs_digest != inputs.identity() || state.schema_version != 2 {
        return Err("carry the map to this run before review".into());
    }
    let keys = map
        .assertions
        .iter()
        .flat_map(|a| a.flows.iter().map(move |f| flow_key(a, f)))
        .collect::<BTreeSet<_>>();
    if !selected.is_subset(&keys) {
        return Err("unknown flow selected for review".into());
    }
    // Partial review permits unrelated stale anchors. ID/edge errors are still
    // rejected for the selected entries; a report excludes invalid siblings.
    let mut next = state.clone();
    let mut assertion_ids = BTreeSet::new();
    let mut locations = BTreeSet::new();
    for a in &map.assertions {
        if !valid_id(&a.id) || !assertion_ids.insert(&a.id) || !locations.insert(&a.at) {
            return Err("invalid/duplicate assertion identity".into());
        }
        let mut flow_ids = BTreeSet::new();
        for f in &a.flows {
            if !valid_id(&f.id) || !flow_ids.insert(&f.id) {
                return Err("invalid/duplicate flow ID".into());
            }
            let key = flow_key(a, f);
            if all || selected.contains(&key) {
                let errors = validate_flow(f, &inputs.files);
                if !errors.is_empty() || a.at.offset(&inputs.files).is_none() {
                    return Err(format!("{key}: invalid current references: {errors:?}"));
                }
                next.reviews.insert(
                    key,
                    Review {
                        fingerprint: fingerprint(a, f, &inputs.files),
                        reasons: BTreeSet::new(),
                    },
                );
            }
        }
    }
    next.reviews.retain(|k, _| keys.contains(k));
    if ack_scope {
        next.scope_review.clear();
    }
    *state = next;
    Ok(())
}

fn unique_occurrence(text: &str, snippet: &str) -> Option<usize> {
    if snippet.is_empty() {
        return None;
    }
    let first = text.find(snippet)?;
    // Include overlapping occurrences; match_indices skips them.
    let next = first + text[first..].chars().next()?.len_utf8();
    text[next..]
        .contains(snippet)
        .then_some(())
        .map_or(Some(first), |_| None)
}
fn target_file(file: &str, old: &FileManifest, new: &Files) -> Option<String> {
    if new.contains_key(file) {
        return Some(file.into());
    }
    let hash = old.get(file)?;
    let mut matches = new.iter().filter(|(_, s)| FileFingerprint::of(s) == *hash);
    let first = matches.next()?.0;
    matches.next().is_none().then(|| first.clone())
}
pub fn relocate(at: &Anchor, old: &FileManifest, new: &Files) -> Option<Anchor> {
    let before = old.get(&at.file)?;
    let target = target_file(&at.file, old, new)?;
    let after = &new[&target];
    let mut candidate = at.clone();
    candidate.file.clone_from(&target);
    if FileFingerprint::of(after) == *before && candidate.offset(new).is_some() {
        return Some(candidate);
    }
    let position = unique_occurrence(after, &at.text)?;
    Some(Anchor::new(
        &target,
        after,
        position,
        position + at.text.len(),
    ))
}

/// Carries explanations, never execution events. Uncertain matches are retained
/// as retired suggestions; no nearest-line heuristic assigns semantic meaning.
pub fn carry(
    map: &AssertionMap,
    state: &State,
    old: &InputManifest,
    new: &Inputs,
    evidence_digest: &str,
    context_changed: bool,
) -> Result<(AssertionMap, State), String> {
    if map.schema_version != 1 || old.schema_version != 2 || new.schema_version != 1 {
        return Err("unsupported map/input schema version".into());
    }
    if state.inputs_digest != digest(old) || state.schema_version != 2 {
        return Err("old map state does not match its run inputs".into());
    }
    let new_manifest = new.manifest();
    let (mut next, mut next_state) = seed_manifest(&new_manifest, evidence_digest);
    next.assertions.clear();
    next.retired_assertions = map.retired_assertions.clone();
    next_state.scope_review = state.scope_review.clone();
    let mut consumed = BTreeSet::new();
    let exact = map
        .assertions
        .iter()
        .map(|a| {
            relocate(&a.at, &old.files, &new.files).filter(|at| {
                new.assertions.iter().any(|s| &s.at == at)
                    || !old.assertions.iter().any(|s| s.at == a.at)
            })
        })
        .collect::<Vec<_>>();
    let reserved = exact.iter().flatten().collect::<BTreeSet<_>>();
    for (index, a) in map.assertions.iter().enumerate() {
        // A sole old/new unmatched site in the same file is a review
        // suggestion. Preserve its explanation but never its reviewed status.
        let candidates = new
            .assertions
            .iter()
            .filter(|s| s.at.file == a.at.file && !reserved.contains(&s.at))
            .collect::<Vec<_>>();
        let unmatched = map
            .assertions
            .iter()
            .zip(&exact)
            .filter(|(other, at)| other.at.file == a.at.file && at.is_none())
            .count();
        let replacement = if exact[index].is_none() && unmatched == 1 && candidates.len() == 1 {
            Some(&candidates[0].at)
        } else {
            None
        };
        let matched = exact[index]
            .as_ref()
            .or(replacement)
            .filter(|at| !consumed.contains(*at));
        let Some(at) = matched else {
            next.retired_assertions.push(Retired {
                assertion: a.clone(),
                reason:
                    "assertion removed, changed or ambiguous; reuse its explanation after review"
                        .into(),
            });
            continue;
        };
        consumed.insert(at.clone());
        let mut updated = a.clone();
        updated.at = at.clone();
        for (prior, f) in a.flows.iter().zip(&mut updated.flows) {
            let mut dirty = state
                .reviews
                .get(&flow_key(a, prior))
                .map(|r| r.reasons.clone())
                .unwrap_or_default();
            if state
                .reviews
                .get(&flow_key(a, prior))
                .is_none_or(|r| r.fingerprint != fingerprint_manifest(a, prior, &old.files))
            {
                dirty.insert("unreviewed map or changed review inputs".into());
            }
            for file in dependencies(a, prior) {
                if old.files.get(file) != new_manifest.files.get(file)
                    || !old.files.contains_key(file)
                {
                    dirty.insert(format!("dependency file changed or removed: {file}"));
                }
            }
            if replacement.is_some() {
                dirty.insert("assertion changed or replaced; confirm identity and meaning".into());
            }
            for node in &mut f.nodes {
                if let Some(at) = relocate(&node.at, &old.files, &new.files) {
                    node.at = at;
                } else {
                    dirty.insert(format!("node {} changed or ambiguous", node.id));
                }
            }
            for watch in &mut f.watch {
                match watch {
                    Watch::File { file } => {
                        if let Some(target) = target_file(file, &old.files, &new.files) {
                            *file = target;
                        } else {
                            dirty.insert(format!("watched file removed: {file}"));
                        }
                    }
                    Watch::Span { at } => {
                        if let Some(updated) = relocate(at, &old.files, &new.files) {
                            *at = updated;
                        } else {
                            dirty.insert(format!("watched span changed: {}:{}", at.file, at.line));
                        }
                    }
                }
            }
            if context_changed {
                dirty.insert("run configuration, dependencies or execution context changed".into());
            }
            next_state.reviews.insert(
                flow_key(a, f),
                Review {
                    fingerprint: String::new(),
                    reasons: dirty,
                },
            );
        }
        for f in &updated.flows {
            next_state
                .reviews
                .get_mut(&flow_key(&updated, f))
                .unwrap()
                .fingerprint = fingerprint_manifest(&updated, f, &new_manifest.files);
        }
        next.assertions.push(updated);
    }
    let mut ids = map
        .assertions
        .iter()
        .map(|a| a.id.clone())
        .chain(
            map.retired_assertions
                .iter()
                .map(|r| r.assertion.id.clone()),
        )
        .collect::<BTreeSet<_>>();
    for a in seed(new, evidence_digest).0.assertions {
        if !consumed.contains(&a.at) {
            let mut a = a;
            while !ids.insert(a.id.clone()) {
                a.id.push('_');
            }
            next.assertions.push(a);
        }
    }
    // Hashes identify changed files, not changed spans. A whole-file watch
    // assigns an edit to flow review; all other changes require scope review.
    let whole_files = |m: &AssertionMap| -> BTreeSet<String> {
        m.assertions
            .iter()
            .flat_map(|a| {
                a.flows.iter().flat_map(move |f| {
                    std::iter::once(a.at.file.clone()).chain(f.watch.iter().filter_map(
                        |w| match w {
                            Watch::File { file } => Some(file.clone()),
                            _ => None,
                        },
                    ))
                })
            })
            .collect()
    };
    let watched = whole_files(map)
        .union(&whole_files(&next))
        .cloned()
        .collect::<BTreeSet<_>>();
    for file in old
        .files
        .keys()
        .chain(new_manifest.files.keys())
        .collect::<BTreeSet<_>>()
    {
        if old.files.get(file) != new_manifest.files.get(file) && !watched.contains(file) {
            next_state.scope_review.insert(file.clone());
        }
    }
    next.assertions.sort_by(|a, b| a.at.cmp(&b.at));
    Ok((next, next_state))
}
