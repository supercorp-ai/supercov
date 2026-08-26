//! Strict ingestion of the private rustc companion's manifest candidate.

use std::collections::BTreeSet;

use serde::Deserialize;

const SCHEMA: &str = "supercov-rust-manifest-candidate-v1";
const MODEL: &str = "rust-source-v1";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RustCompilerManifest {
    pub schema: String,
    pub model: String,
    #[serde(rename = "crate")]
    pub crate_name: String,
    pub measurement_complete: bool,
    pub points: Vec<RustCompilerPoint>,
    pub branches: Vec<RustCompilerBranch>,
    pub decisions: Vec<RustCompilerDecision>,
    pub selection_groups: Vec<RustCompilerSelectionGroup>,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RustCompilerPoint {
    pub id: String,
    pub kind: String,
    pub source_key: String,
    pub start: u32,
    pub end: u32,
    pub provenance: String,
    pub discriminator: String,
    pub probe_ordinal: String,
    pub definitions: Vec<String>,
    pub canonical: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RustCompilerBranchAlternative {
    pub id: String,
    pub label: String,
    pub probe_ordinal: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RustCompilerBranch {
    pub id: String,
    pub kind: String,
    pub discriminator: String,
    pub source_key: String,
    pub start: u32,
    pub end: u32,
    pub provenance: String,
    pub probe_ordinal: String,
    pub definitions: Vec<String>,
    pub alternatives: Vec<RustCompilerBranchAlternative>,
    pub canonical: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RustCompilerCondition {
    pub source_key: String,
    pub start: u32,
    pub end: u32,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RustCompilerDecision {
    pub id: String,
    pub kind: String,
    pub source_key: String,
    pub start: u32,
    pub end: u32,
    pub provenance: String,
    pub probe_ordinal: String,
    pub definitions: Vec<String>,
    pub conditions: Vec<RustCompilerCondition>,
    pub canonical: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RustCompilerMatchArm {
    pub branch_id: String,
    pub body_source_key: String,
    pub body_start: u32,
    pub body_end: u32,
    pub guarded: bool,
    pub guard_decision_id: Option<String>,
    pub selected_ordinal: String,
    pub not_selected_ordinal: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RustCompilerSelectionGroup {
    pub id: String,
    pub kind: String,
    pub source_key: String,
    pub start: u32,
    pub end: u32,
    pub provenance: String,
    pub probe_ordinal: String,
    pub definitions: Vec<String>,
    pub parent_group_id: Option<String>,
    pub parent_site: Option<String>,
    pub parent_arm_index: Option<usize>,
    pub arms: Vec<RustCompilerMatchArm>,
    pub canonical: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RustCompilerManifestError {
    Json(String),
    Invalid(String),
}

impl std::fmt::Display for RustCompilerManifestError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Json(error) => write!(formatter, "invalid Rust compiler manifest JSON: {error}"),
            Self::Invalid(error) => write!(formatter, "invalid Rust compiler manifest: {error}"),
        }
    }
}

impl std::error::Error for RustCompilerManifestError {}

fn sorted_unique_nonempty(values: &[String]) -> bool {
    !values.is_empty()
        && values.iter().all(|value| !value.trim().is_empty())
        && values.windows(2).all(|pair| pair[0] < pair[1])
}

fn valid_id(id: &str, allowed: &[&str]) -> bool {
    let mut parts = id.split(':');
    parts.next() == Some("rs")
        && parts.next().is_some_and(|kind| allowed.contains(&kind))
        && parts.next().is_some_and(|digest| {
            digest.len() == 24 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
        && parts.next().is_none()
}

fn source_range(key: &str, start: u32, end: u32) -> bool {
    !key.trim().is_empty() && start < end
}

fn ordinal(value: &str) -> Option<u64> {
    value.parse::<u64>().ok().filter(|ordinal| *ordinal != 0)
}

fn provenance(value: &str) -> bool {
    matches!(
        value,
        "authored-source" | "synthetic-expansion" | "generated-source"
    )
}

fn insert_identity(
    ids: &mut BTreeSet<String>,
    ordinals: &mut BTreeSet<u64>,
    id: &str,
    probe_ordinal: &str,
) -> Result<(), RustCompilerManifestError> {
    if !ids.insert(id.into()) {
        return Err(RustCompilerManifestError::Invalid(format!(
            "duplicate obligation ID {id}"
        )));
    }
    let ordinal = ordinal(probe_ordinal).ok_or_else(|| {
        RustCompilerManifestError::Invalid(format!(
            "obligation {id} has invalid probe ordinal {probe_ordinal}"
        ))
    })?;
    if !ordinals.insert(ordinal) {
        return Err(RustCompilerManifestError::Invalid(format!(
            "duplicate probe ordinal {ordinal}"
        )));
    }
    Ok(())
}

impl RustCompilerManifest {
    pub fn parse(bytes: &[u8]) -> Result<Self, RustCompilerManifestError> {
        let manifest: Self = serde_json::from_slice(bytes)
            .map_err(|error| RustCompilerManifestError::Json(error.to_string()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<(), RustCompilerManifestError> {
        let invalid = |reason: &str| RustCompilerManifestError::Invalid(reason.into());
        if self.schema != SCHEMA || self.model != MODEL {
            return Err(invalid("unsupported schema or source model"));
        }
        if self.crate_name.trim().is_empty() || self.measurement_complete {
            return Err(invalid(
                "a private candidate needs a crate identity and cannot claim completeness",
            ));
        }
        if self.points.is_empty() || !sorted_unique_nonempty(&self.limitations) {
            return Err(invalid(
                "a candidate needs points and sorted explicit limitations",
            ));
        }
        if !self.points.windows(2).all(|pair| pair[0].id < pair[1].id)
            || !self.branches.windows(2).all(|pair| pair[0].id < pair[1].id)
            || !self
                .decisions
                .windows(2)
                .all(|pair| pair[0].id < pair[1].id)
            || !self
                .selection_groups
                .windows(2)
                .all(|pair| pair[0].id < pair[1].id)
        {
            return Err(invalid("obligation arrays are not in canonical ID order"));
        }

        let mut ids = BTreeSet::new();
        let mut ordinals = BTreeSet::new();
        for point in &self.points {
            if !valid_id(&point.id, &["statement", "function"])
                || !matches!(point.kind.as_str(), "statement" | "function")
                || !source_range(&point.source_key, point.start, point.end)
                || !provenance(&point.provenance)
                || !sorted_unique_nonempty(&point.definitions)
                || point.canonical.is_empty()
            {
                return Err(invalid("malformed point obligation"));
            }
            insert_identity(&mut ids, &mut ordinals, &point.id, &point.probe_ordinal)?;
        }
        let mut branch_ids = BTreeSet::new();
        for branch in &self.branches {
            if !valid_id(&branch.id, &["branch"])
                || !matches!(
                    branch.kind.as_str(),
                    "decision-outcome"
                        | "loop-entry"
                        | "match-arm"
                        | "let-else"
                        | "try-operator"
                        | "assertion-outcome"
                )
                || !source_range(&branch.source_key, branch.start, branch.end)
                || !provenance(&branch.provenance)
                || !sorted_unique_nonempty(&branch.definitions)
                || branch.alternatives.len() < 2
                || branch.canonical.is_empty()
            {
                return Err(invalid("malformed branch obligation"));
            }
            insert_identity(&mut ids, &mut ordinals, &branch.id, &branch.probe_ordinal)?;
            branch_ids.insert(branch.id.as_str());
            let mut labels = BTreeSet::new();
            for alternative in &branch.alternatives {
                if !valid_id(&alternative.id, &["branch"])
                    || alternative.label.trim().is_empty()
                    || !labels.insert(alternative.label.as_str())
                {
                    return Err(invalid("malformed branch alternative"));
                }
                insert_identity(
                    &mut ids,
                    &mut ordinals,
                    &alternative.id,
                    &alternative.probe_ordinal,
                )?;
            }
        }
        let mut decision_ids = BTreeSet::new();
        for decision in &self.decisions {
            if !valid_id(&decision.id, &["decision"])
                || !matches!(
                    decision.kind.as_str(),
                    "if" | "if-let"
                        | "while"
                        | "while-let"
                        | "let-chain"
                        | "match-guard"
                        | "assertion"
                )
                || !source_range(&decision.source_key, decision.start, decision.end)
                || !provenance(&decision.provenance)
                || !sorted_unique_nonempty(&decision.definitions)
                || decision.conditions.is_empty()
                || decision.canonical.is_empty()
                || decision.conditions.iter().any(|condition| {
                    !source_range(&condition.source_key, condition.start, condition.end)
                        || condition.source.trim().is_empty()
                })
            {
                return Err(invalid("malformed decision obligation"));
            }
            insert_identity(
                &mut ids,
                &mut ordinals,
                &decision.id,
                &decision.probe_ordinal,
            )?;
            decision_ids.insert(decision.id.as_str());
        }
        let selection_ids = self
            .selection_groups
            .iter()
            .map(|group| group.id.as_str())
            .collect::<BTreeSet<_>>();
        if selection_ids.len() != self.selection_groups.len() {
            return Err(invalid("duplicate match selection group ID"));
        }
        for group in &self.selection_groups {
            if !valid_id(&group.id, &["match-group"])
                || group.kind != "match"
                || !source_range(&group.source_key, group.start, group.end)
                || !provenance(&group.provenance)
                || !sorted_unique_nonempty(&group.definitions)
                || group.arms.len() < 2
                || group.canonical.is_empty()
                || group
                    .parent_group_id
                    .as_ref()
                    .is_some_and(|parent| !selection_ids.contains(parent.as_str()))
                || group.parent_site.is_some() != group.parent_group_id.is_some()
                || group.parent_arm_index.is_some() != group.parent_group_id.is_some()
                || group
                    .parent_site
                    .as_deref()
                    .is_some_and(|site| !matches!(site, "guard" | "body"))
            {
                return Err(invalid("malformed match selection group"));
            }
            insert_identity(&mut ids, &mut ordinals, &group.id, &group.probe_ordinal)?;
            let mut arm_branches = BTreeSet::new();
            for arm in &group.arms {
                if !branch_ids.contains(arm.branch_id.as_str())
                    || !arm_branches.insert(arm.branch_id.as_str())
                    || !source_range(&arm.body_source_key, arm.body_start, arm.body_end)
                    || arm.guarded != arm.guard_decision_id.is_some()
                    || arm
                        .guard_decision_id
                        .as_ref()
                        .is_some_and(|guard| !decision_ids.contains(guard.as_str()))
                    || ordinal(&arm.selected_ordinal).is_none()
                    || ordinal(&arm.not_selected_ordinal).is_none()
                {
                    return Err(invalid("malformed match arm mapping"));
                }
                let branch = self
                    .branches
                    .iter()
                    .find(|branch| branch.id == arm.branch_id)
                    .expect("validated branch reference");
                let alternatives = branch
                    .alternatives
                    .iter()
                    .map(|alternative| alternative.probe_ordinal.as_str())
                    .collect::<BTreeSet<_>>();
                if alternatives
                    != BTreeSet::from([
                        arm.selected_ordinal.as_str(),
                        arm.not_selected_ordinal.as_str(),
                    ])
                {
                    return Err(invalid("match arm ordinals do not match its branch"));
                }
            }
        }
        for group in &self.selection_groups {
            let mut visited = BTreeSet::new();
            let mut current = group;
            while let Some(parent_id) = &current.parent_group_id {
                if !visited.insert(current.id.as_str()) {
                    return Err(invalid("cyclic match selection group parentage"));
                }
                let parent = self
                    .selection_groups
                    .iter()
                    .find(|candidate| candidate.id == *parent_id)
                    .expect("validated parent group reference");
                if current
                    .parent_arm_index
                    .is_none_or(|index| index >= parent.arms.len())
                {
                    return Err(invalid("match selection parent arm is out of range"));
                }
                current = parent;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn valid_manifest() -> serde_json::Value {
        json!({
            "schema": SCHEMA,
            "model": MODEL,
            "crate": "fixture",
            "measurementComplete": false,
            "points": [{
                "id": "rs:function:000000000000000000000001",
                "kind": "function",
                "sourceKey": "project:src/lib.rs",
                "start": 0,
                "end": 10,
                "provenance": "authored-source",
                "discriminator": "",
                "probeOrdinal": "1",
                "definitions": ["fixture::function"],
                "canonical": "function"
            }],
            "branches": [{
                "id": "rs:branch:000000000000000000000002",
                "kind": "decision-outcome",
                "discriminator": "decision-outcome:if",
                "sourceKey": "project:src/lib.rs",
                "start": 11,
                "end": 20,
                "provenance": "authored-source",
                "probeOrdinal": "2",
                "definitions": ["fixture::function"],
                "alternatives": [
                    {"id": "rs:branch:000000000000000000000003", "label": "condition true", "probeOrdinal": "3"},
                    {"id": "rs:branch:000000000000000000000004", "label": "condition false", "probeOrdinal": "4"}
                ],
                "canonical": "branch"
            }],
            "decisions": [{
                "id": "rs:decision:000000000000000000000005",
                "kind": "if",
                "sourceKey": "project:src/lib.rs",
                "start": 11,
                "end": 15,
                "provenance": "authored-source",
                "probeOrdinal": "5",
                "definitions": ["fixture::function"],
                "conditions": [{"sourceKey": "project:src/lib.rs", "start": 11, "end": 15, "source": "value"}],
                "canonical": "decision"
            }],
            "selectionGroups": [],
            "limitations": ["RUST_PRIVATE_CANDIDATE: incomplete"]
        })
    }

    #[test]
    fn accepts_only_a_strict_collision_free_private_candidate() {
        let manifest =
            RustCompilerManifest::parse(&serde_json::to_vec(&valid_manifest()).unwrap()).unwrap();
        assert_eq!(manifest.crate_name, "fixture");

        let mut unknown = valid_manifest();
        unknown["unexpected"] = json!(true);
        assert!(matches!(
            RustCompilerManifest::parse(&serde_json::to_vec(&unknown).unwrap()),
            Err(RustCompilerManifestError::Json(_))
        ));

        let mut collision = valid_manifest();
        collision["decisions"][0]["probeOrdinal"] = json!("1");
        assert!(matches!(
            RustCompilerManifest::parse(&serde_json::to_vec(&collision).unwrap()),
            Err(RustCompilerManifestError::Invalid(_))
        ));

        let mut complete = valid_manifest();
        complete["measurementComplete"] = json!(true);
        assert!(matches!(
            RustCompilerManifest::parse(&serde_json::to_vec(&complete).unwrap()),
            Err(RustCompilerManifestError::Invalid(_))
        ));
    }
}
