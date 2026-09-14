//! Agent-authored assertion maps. Edges are explanations, never inferred proofs.
//! This module owns format validation, text relocation and input acknowledgement bookkeeping.

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
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TestSelector {
    pub file: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Flow {
    pub id: String,
    #[serde(deserialize_with = "required_basis")]
    #[schemars(required, schema_with = "basis_schema")]
    pub basis: Option<String>,
    pub explanation: String,
    pub applies_to: Vec<TestSelector>,
    pub nodes: Vec<Node>,
    #[serde(default)]
    pub edges: Vec<Edge>,
    pub counts_as_asserted: Vec<String>,
    pub watch: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub questions: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Assertion {
    pub id: String,
    pub at: Anchor,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub questions: Vec<String>,
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
    #[schemars(range(min = 2, max = 2))]
    pub schema_version: u32,
    pub assertions: Vec<Assertion>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub change_assessments: Vec<ChangeAssessment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retired_assertions: Vec<Retired>,
}

// Missing basis is a syntax error; null explicitly means unfinished work.
fn required_basis<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    let value = Option::<String>::deserialize(d)?;
    if value.as_deref().is_some_and(|s| !valid_basis(s)) {
        return Err(serde::de::Error::custom(
            "expected null or scov2:<64 lowercase hex digits>",
        ));
    }
    Ok(value)
}
fn basis_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({"type":["string","null"],"pattern":"^scov2:[0-9a-f]{64}$"})
}
fn valid_basis(s: &str) -> bool {
    s.strip_prefix("scov2:").is_some_and(|h| {
        h.len() == 64
            && h.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChangeAssessment {
    pub id: String,
    #[serde(deserialize_with = "required_basis")]
    #[schemars(required, schema_with = "basis_schema")]
    pub basis: Option<String>,
    pub affected_flows: Vec<String>,
    pub explanation: String,
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
    if map.schema_version != 2 {
        return Err(ParseError {
            pointer: "/schemaVersion".into(),
            line: 0,
            column: 0,
            message: "unsupported map schema version; expected 2".into(),
        });
    }
    Ok(map)
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FlowState {
    pub generation: String,
    pub reasons: BTreeSet<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Change {
    pub id: String,
    pub file: Option<String>,
    pub before: Option<String>,
    pub after: Option<String>,
    pub reason: String,
    pub known_flows: BTreeSet<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct State {
    pub schema_version: u32,
    pub inputs_digest: String,
    pub evidence_digest: String,
    pub flows: BTreeMap<String, FlowState>,
    pub changes: Vec<Change>,
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
            schema_version: 2,
            assertions: inputs
                .assertions
                .iter()
                .map(|site| Assertion {
                    id: format!("a_{}", &digest(&site.at)[..20]),
                    at: site.at.clone(),
                    questions: vec![],
                    observes: vec![],
                    flows: vec![],
                })
                .collect(),
            change_assessments: vec![],
            retired_assertions: vec![],
        },
        State {
            schema_version: 3,
            inputs_digest: digest(inputs),
            evidence_digest: evidence_digest.into(),
            flows: BTreeMap::new(),
            changes: vec![],
            inheritance: None,
        },
    )
}

/// Structural checks only; malformed entries cannot silently earn credit.
pub fn validate(map: &AssertionMap, inputs: &Inputs) -> Vec<String> {
    let mut errors = Vec::new();
    if map.schema_version != 2 || inputs.schema_version != 1 {
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
    let mut changes = BTreeSet::new();
    for change in &map.change_assessments {
        if !valid_id(&change.id) || !changes.insert(&change.id) {
            errors.push("invalid/duplicate change assessment ID".into());
        }
    }
    errors
}
/// Things worth telling the author that do not make the map wrong.
///
/// A redundant `watch` is the one that matters today. Supercov already marks
/// every flow dirty when a dependency manifest or the execution configuration
/// changes, so naming one of those files per flow catches nothing extra. It
/// does teach a false model -- that per-flow watching is how dependency drift
/// is caught -- and an author who believes it spends the effort on entries that
/// change nothing instead of on the helper their claim actually rests on.
pub fn advisories(map: &AssertionMap) -> Vec<String> {
    let mut out = Vec::new();
    for a in &map.assertions {
        for f in &a.flows {
            for file in &f.watch {
                if crate::integrity::globally_tracked(file) {
                    out.push(format!(
                        "{}: watch \"{file}\" is redundant; Supercov invalidates every flow when that file changes",
                        flow_key(a, f)
                    ));
                }
            }
        }
    }
    out
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
        if !nodes.contains(&edge.from) || (!nodes.contains(&edge.to) && edge.to != "$assertion") {
            errors.push("dangling edge".into());
        }
    }
    if flow.counts_as_asserted.iter().any(|id| !nodes.contains(id)) {
        errors.push("unknown counted node".into());
    }
    // Traverse only the author's graph. Never infer a dependency from source.
    let mut reaches = BTreeSet::from(["$assertion".to_owned()]);
    loop {
        let size = reaches.len();
        for edge in &flow.edges {
            if reaches.contains(&edge.to) {
                reaches.insert(edge.from.clone());
            }
        }
        if reaches.len() == size {
            break;
        }
    }
    for id in &flow.counts_as_asserted {
        if !reaches.contains(id) {
            errors.push(format!(
                "counted node {id} has no authored path to $assertion"
            ));
        }
    }
    if flow.edges.iter().any(|e| e.kind.trim().is_empty()) {
        errors.push("missing edge kind".into());
    }
    if flow
        .applies_to
        .iter()
        .any(|t| !local_path(&t.file) || !files.contains_key(&t.file) || t.name.trim().is_empty())
    {
        errors.push("invalid test selector file or name".into());
    }
    if flow.applies_to.iter().collect::<BTreeSet<_>>().len() != flow.applies_to.len() {
        errors.push("duplicate test selector".into());
    }
    if flow
        .counts_as_asserted
        .iter()
        .collect::<BTreeSet<_>>()
        .len()
        != flow.counts_as_asserted.len()
    {
        errors.push("duplicate counted node".into());
    }
    if flow.explanation.trim().is_empty() {
        errors.push("missing explanation".into());
    }
    for file in &flow.watch {
        if !local_path(file) || !files.contains_key(file) {
            errors.push(format!("watched file missing: {file}"));
        }
    }
    errors
}

/// Whole-file input dependencies, not a mechanically inferred semantic slice.
pub fn dependencies<'a>(a: &'a Assertion, f: &'a Flow) -> BTreeSet<&'a str> {
    std::iter::once(a.at.file.as_str())
        .chain(f.applies_to.iter().map(|t| t.file.as_str()))
        .chain(f.nodes.iter().map(|n| n.at.file.as_str()))
        // A watch on a file Supercov already answers for run-wide contributes
        // nothing here, and hashing its bytes would quietly undo the manifest
        // rule: a version bump would still make every flow that names
        // `package.json` stale, which is most of them in a real map. The
        // run-level signal still fires, as a change to assess.
        //
        // Only the watch list is filtered. An anchor or a node in one of those
        // files is the flow's actual subject -- `setup.py` is a dependency
        // manifest and measured source at once -- and editing it must still
        // cost a review.
        .chain(
            f.watch
                .iter()
                .map(String::as_str)
                .filter(|path| !crate::integrity::globally_tracked(path)),
        )
        .collect()
}
fn token(value: &impl Serialize) -> String {
    format!("scov2:{}", digest(value))
}
pub fn change_errors(
    map: &AssertionMap,
    change: &Change,
    response: &ChangeAssessment,
) -> Vec<String> {
    let keys = map
        .assertions
        .iter()
        .flat_map(|a| a.flows.iter().map(move |f| flow_key(a, f)))
        .collect::<BTreeSet<_>>();
    let affected = response
        .affected_flows
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut errors = Vec::new();
    if response.explanation.trim().is_empty() {
        errors.push("missing impact explanation".into());
    }
    if affected.len() != response.affected_flows.len() {
        errors.push("duplicate affected flow".into());
    }
    if !affected.is_subset(&keys) {
        errors.push("unknown affected flow".into());
    }
    if !change
        .known_flows
        .intersection(&keys)
        .all(|k| affected.contains(k))
    {
        errors.push("known dependent flows must be included unless removed from the map".into());
    }
    errors
}
pub fn expected_change_basis(
    change: &Change,
    response: &ChangeAssessment,
    inputs: &InputManifest,
) -> String {
    token(&(
        "supercov-change-v2",
        change,
        digest(inputs),
        &response.id,
        &response.affected_flows,
        &response.explanation,
    ))
}
pub fn change_current(map: &AssertionMap, change: &Change, inputs: &InputManifest) -> bool {
    let responses = map
        .change_assessments
        .iter()
        .filter(|r| r.id == change.id)
        .collect::<Vec<_>>();
    matches!(responses.as_slice(), [r] if change_errors(map, change, r).is_empty() && r.basis.as_deref() == Some(expected_change_basis(change, r, inputs).as_str()))
}
/// Why a file is one of a flow's dependencies, and where the flow sits in it.
///
/// "dependency file changed" names a file and leaves the author to work out
/// what it has to do with this claim. A flow depends on a file for one of four
/// reasons, and they call for different judgements: a node there is the claim's
/// subject, a watch is something the author asked to be told about, the test
/// file is where the claim is exercised, and the assertion's own file is where
/// it is written. Saying which -- and where the nodes are -- is the difference
/// between rereading a claim and glancing at a line number.
fn why_depended_on(a: &Assertion, f: &Flow, file: &str) -> String {
    let mut roles: Vec<String> = Vec::new();
    let lines = f
        .nodes
        .iter()
        .filter(|n| n.at.file == file)
        .map(|n| format!("{}:{}", n.id, n.at.line))
        .collect::<Vec<_>>();
    if !lines.is_empty() {
        roles.push(format!("holds this flow's {}", lines.join(", ")));
    }
    if a.at.file == file {
        roles.push(format!("holds the assertion, line {}", a.at.line));
    }
    if f.applies_to.iter().any(|t| t.file == file) {
        roles.push("is the test this claim applies to".to_owned());
    }
    if f.watch.iter().any(|w| w == file) {
        roles.push("is watched by this flow".to_owned());
    }
    if roles.is_empty() {
        // Every path into dependencies() is covered above; say nothing rather
        // than guess if that ever stops being true.
        return String::new();
    }
    format!(" ({})", roles.join("; "))
}

fn generation(
    a: &Assertion,
    f: &Flow,
    map: &AssertionMap,
    state: &State,
    inputs: &InputManifest,
) -> String {
    let key = flow_key(a, f);
    let base = state.flows.get(&key).map_or("0", |s| s.generation.as_str());
    let impacts = state
        .changes
        .iter()
        .filter(|c| change_current(map, c, inputs))
        .filter_map(|c| {
            map.change_assessments
                .iter()
                .find(|r| r.id == c.id && r.affected_flows.contains(&key))
                .map(|r| (&c.id, &r.basis))
        })
        .collect::<BTreeMap<_, _>>();
    if impacts.is_empty() {
        base.into()
    } else {
        digest(&("supercov-generation-v2", base, impacts))
    }
}
pub fn expected_basis(
    a: &Assertion,
    f: &Flow,
    map: &AssertionMap,
    state: &State,
    inputs: &InputManifest,
) -> String {
    let mut claim = f.clone();
    claim.basis = None;
    let hashes = dependencies(a, f)
        .into_iter()
        .map(|p| (p, inputs.files.get(p)))
        .collect::<BTreeMap<_, _>>();
    token(&(
        "supercov-flow-v2",
        &inputs.context_digest,
        &a.id,
        &a.at,
        &a.observes,
        claim,
        hashes,
        generation(a, f, map, state, inputs),
    ))
}
pub fn reasons(
    a: &Assertion,
    f: &Flow,
    map: &AssertionMap,
    state: &State,
    inputs: &Inputs,
) -> BTreeSet<String> {
    reasons_for_manifest(a, f, map, state, inputs, &inputs.manifest())
}
pub fn reasons_for_manifest(
    a: &Assertion,
    f: &Flow,
    map: &AssertionMap,
    state: &State,
    inputs: &Inputs,
    manifest: &InputManifest,
) -> BTreeSet<String> {
    let mut reasons = BTreeSet::new();
    if state.schema_version != 3 || state.inputs_digest != digest(manifest) {
        reasons.insert("state does not match run inputs".into());
    }
    if f.basis.as_deref() != Some(expected_basis(a, f, map, state, manifest).as_str()) {
        reasons.insert(
            if f.basis.is_none() {
                "draft: input acknowledgement not recorded"
            } else {
                "claim or inputs changed; needs rechecking"
            }
            .into(),
        );
        if let Some(s) = state.flows.get(&flow_key(a, f)) {
            reasons.extend(s.reasons.iter().cloned());
        }
    }
    reasons.extend(validate_flow(f, &inputs.files));
    if a.at.offset(&inputs.files).is_none() {
        reasons.insert("invalid assertion anchor".into());
    }
    if !f.questions.is_empty() {
        reasons.insert("flow has unresolved questions".into());
    }
    reasons
}
/// Read-only validation. Tokens acknowledge authored claims, never prove them.
pub fn validation(map: &AssertionMap, state: &State, inputs: &Inputs) -> serde_json::Value {
    use serde_json::json;
    let manifest = inputs.manifest();
    let mut errors = validate(map, inputs);
    for r in &map.change_assessments {
        if !state.changes.iter().any(|c| c.id == r.id) {
            errors.push(format!("{}: unknown change assessment", r.id));
        }
    }
    let changes = state.changes.iter().map(|c| {
        let response = map.change_assessments.iter().find(|r| r.id == c.id);
        let faults = response.map(|r| change_errors(map, c, r)).unwrap_or_default();
        errors.extend(faults.iter().map(|e| format!("{}: {e}", c.id)));
        json!({"id":c.id,"file":c.file,"before":c.before,"after":c.after,"reason":c.reason,"knownFlows":c.known_flows,
            "current":change_current(map,c,&manifest),"assessment":response,"errors":faults,
            "expectedBasis":response.map(|r| expected_change_basis(c,r,&manifest))})
    }).collect::<Vec<_>>();
    let flows = map.assertions.iter().flat_map(|a| a.flows.iter().map(move |f| (a,f))).map(|(a,f)| {
        json!({"id":flow_key(a,f),"expectedBasis":expected_basis(a,f,map,state,&manifest),"reasons":reasons_for_manifest(a,f,map,state,inputs,&manifest)})
    }).collect::<Vec<_>>();
    json!({"valid":errors.is_empty(),"stage":"references","errors":errors,"flows":flows,"changes":changes,
        "meaning":"Authored graph references and input acknowledgements only; no semantic proof or completeness claim"})
}
pub fn invalidate(state: &mut State, map: &AssertionMap, reason: &str) {
    for a in &map.assertions {
        for f in &a.flows {
            let key = flow_key(a, f);
            let base = state.flows.get(&key).map_or("0", |s| s.generation.as_str());
            state.flows.insert(
                key,
                FlowState {
                    generation: digest(&(base, reason, &state.inputs_digest)),
                    reasons: BTreeSet::from([reason.into()]),
                },
            );
        }
    }
}
pub fn add_change(
    state: &mut State,
    file: Option<String>,
    before: Option<String>,
    after: Option<String>,
    reason: String,
    known_flows: BTreeSet<String>,
) {
    // Include pending history so edit/revert/edit cannot alias a still-pending event.
    let id = format!(
        "c_{}",
        &digest(&(
            "supercov-change-id-v2",
            &state.changes,
            &file,
            &before,
            &after,
            &reason
        ))[..24]
    );
    state.changes.push(Change {
        id,
        file,
        before,
        after,
        reason,
        known_flows,
    });
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
    // The file changed somewhere. That says nothing about this anchor: read the
    // recorded position in the new file and see whether it still holds the same
    // text. If it does, the anchor did not move and there is nothing to find.
    //
    // Without this, every anchor in a changed file is re-found by searching the
    // whole file, and that search insists the text be unique -- so a statement
    // that appears twice is reported "changed or ambiguous" while sitting
    // untouched at the line it was recorded at. That is a false statement about
    // a specific node, and it is most of the staleness in a real map.
    if let Some(start) = candidate.offset(new)
        && after.get(start..start + at.text.len()) == Some(at.text.as_str())
    {
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
    if map.schema_version != 2 || old.schema_version != 2 || new.schema_version != 1 {
        return Err("unsupported map/input schema version".into());
    }
    if state.inputs_digest != digest(old) || state.schema_version != 3 {
        return Err("old map state does not match its run inputs".into());
    }
    let new_manifest = new.manifest();
    let (mut next, mut next_state) = seed_manifest(&new_manifest, evidence_digest);
    next.assertions.clear();
    next.retired_assertions = map.retired_assertions.clone();
    next_state.changes = state
        .changes
        .iter()
        .filter(|c| !change_current(map, c, old))
        .cloned()
        .collect();
    next.change_assessments = map
        .change_assessments
        .iter()
        .filter(|r| next_state.changes.iter().any(|c| c.id == r.id))
        .cloned()
        .collect();
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
            let base = generation(a, prior, map, state, old);
            let mut dirty = BTreeSet::new();
            if prior
                .basis
                .as_deref()
                .is_some_and(|basis| basis != expected_basis(a, prior, map, state, old))
            {
                dirty.insert("inherited claim still needs rechecking".into());
            }
            for file in dependencies(a, prior) {
                if old.files.get(file) != new_manifest.files.get(file)
                    || !old.files.contains_key(file)
                {
                    dirty.insert(format!(
                        "dependency file changed or removed: {file}{}",
                        why_depended_on(a, prior, file)
                    ));
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
            for file in f
                .watch
                .iter_mut()
                .chain(f.applies_to.iter_mut().map(|t| &mut t.file))
            {
                if let Some(target) = target_file(file, &old.files, &new.files) {
                    *file = target;
                } else {
                    dirty.insert(format!("dependency file removed: {file}"));
                }
            }
            if context_changed {
                dirty.insert("run configuration, dependencies or execution context changed".into());
            }
            next_state.flows.insert(
                flow_key(a, f),
                FlowState {
                    generation: if dirty.is_empty() {
                        base
                    } else {
                        digest(&("supercov-carry-v2", base, &new_manifest, &dirty))
                    },
                    reasons: dirty,
                },
            );
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
    for file in old
        .files
        .keys()
        .chain(new_manifest.files.keys())
        .collect::<BTreeSet<_>>()
    {
        // A manifest is answered for by the run's dependency fingerprint, which
        // reads what it declares. Reporting its bytes here as well would make
        // cutting a release look like a change to assess when nothing about the
        // project moved.
        if crate::integrity::tracked_manifest(file) {
            continue;
        }
        if old.files.get(file) != new_manifest.files.get(file) {
            let known = map
                .assertions
                .iter()
                .flat_map(|a| {
                    a.flows
                        .iter()
                        .filter(|f| dependencies(a, f).contains(file.as_str()))
                        .map(move |f| flow_key(a, f))
                })
                .collect();
            add_change(
                &mut next_state,
                Some(file.clone()),
                old.files.get(file).map(|f| f.sha256.clone()),
                new_manifest.files.get(file).map(|f| f.sha256.clone()),
                "captured source file changed".into(),
                known,
            );
        }
    }
    next.assertions.sort_by(|a, b| a.at.cmp(&b.at));
    Ok((next, next_state))
}

#[path = "assertion_legacy.rs"]
mod legacy;
pub fn parse_stored(bytes: &[u8]) -> Result<AssertionMap, String> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    match value
        .get("schemaVersion")
        .and_then(serde_json::Value::as_u64)
    {
        None | Some(1) => legacy::import(bytes),
        _ => parse(bytes).map_err(|e| e.to_string()),
    }
}
pub fn parse_state(
    bytes: &[u8],
    map: &AssertionMap,
    inputs: &InputManifest,
    evidence: &str,
    legacy_digest: Option<&str>,
) -> Result<State, String> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if value["schemaVersion"] == 3 {
        let state: State = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        if state.inputs_digest != digest(inputs) || state.evidence_digest != evidence {
            return Err("Assertion state belongs to different run evidence; rerun tests".into());
        }
        Ok(state)
    } else {
        legacy::state(bytes, map, inputs, evidence, legacy_digest)
    }
}
