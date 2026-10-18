//! Atomic, integrity-checked merging of independently executed run shards.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use serde_json::Value;
use supercov_contracts::FrontendRunDeclaration;

use crate::{
    evidence_archive::{EvidenceArchiveEntry, read_archive, write_archive},
    lifecycle::{ProjectLock, finalize_published_run, publish_run, remove_stored_tree_deferred},
    run_store::{RawEvidenceMetadata, RunMetadata, StoredRun, discover_runs},
};

fn rewrite_scope_run_ids(value: &mut Value, merged_run_id: &str) {
    match value {
        Value::Array(values) => {
            for value in values {
                rewrite_scope_run_ids(value, merged_run_id);
            }
        }
        Value::Object(object) => {
            for (key, value) in object {
                if key == "scope" && value.is_object() {
                    value
                        .as_object_mut()
                        .expect("scope was checked as an object")
                        .insert("runId".into(), Value::String(merged_run_id.into()));
                } else {
                    rewrite_scope_run_ids(value, merged_run_id);
                }
            }
        }
        _ => {}
    }
}

/// The test a record is of: a test's own record names it, and a server's
/// names it in its scope.
fn record_test(value: &Value) -> Option<&str> {
    value["testId"]
        .as_str()
        .or_else(|| value["scope"]["testId"].as_str())
}

/// The tests a run's archive holds a record of.
fn measured_tests(archive: &[EvidenceArchiveEntry]) -> BTreeMap<String, String> {
    let mut tests = BTreeMap::new();
    for entry in archive {
        let Ok(text) = std::str::from_utf8(&entry.contents) else {
            continue;
        };
        for line in text.lines() {
            if !line.contains("\"testId\"") {
                continue;
            }
            if let Ok(value) = serde_json::from_str::<Value>(line)
                && let (Some(id), Some(name)) = (value["testId"].as_str(), value["test"].as_str())
            {
                tests.insert(id.to_owned(), name.to_owned());
            }
        }
    }
    tests
}

/// An entry's records for the merge, without those of the tests measured
/// again in another run. None when nothing of the entry is left.
pub(crate) fn rewritten_contents(
    contents: &[u8],
    merged_run_id: &str,
    superseded: Option<&BTreeSet<String>>,
) -> Result<Option<Vec<u8>>, String> {
    let contents = std::str::from_utf8(contents)
        .map_err(|_| "coverage evidence contains non-UTF-8 data".to_owned())?;
    let mut rewritten = Vec::with_capacity(contents.len());
    let mut left_out = false;
    let mut kept = false;
    for line in contents.split('\n') {
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(value)
                if superseded.is_some_and(|tests| {
                    record_test(&value).is_some_and(|test| tests.contains(test))
                }) =>
            {
                left_out = true;
            }
            Ok(mut value) => {
                if kept {
                    rewritten.push(b'\n');
                }
                kept = true;
                rewrite_scope_run_ids(&mut value, merged_run_id);
                rewritten.extend(
                    serde_json::to_vec(&value)
                        .map_err(|error| format!("failed to rewrite merged evidence: {error}"))?,
                );
            }
            Err(error) => {
                return Err(format!(
                    "cannot merge malformed recognized JSON evidence: {error}"
                ));
            }
        }
    }
    if contents.ends_with('\n') && kept {
        rewritten.push(b'\n');
    }
    Ok((kept || !left_out).then_some(rewritten))
}

fn merged_path(path: &str, shard: usize) -> String {
    if path.starts_with("server/background/") {
        return format!(
            "server/background/shard-{shard}-{}",
            path.rsplit('/').next().unwrap_or(path)
        );
    }
    if let Some(path) = path.strip_prefix("server/") {
        return format!("server/shard-{shard}/{path}");
    }
    format!("shards/{shard}/{path}")
}

fn incompatible_dimensions(first: &StoredRun, candidate: &StoredRun) -> Vec<&'static str> {
    let first = &first.metadata.integrity;
    let candidate = &candidate.metadata.integrity;
    let mut dimensions = Vec::new();
    if candidate.schema_version != first.schema_version {
        dimensions.push("coverage schema");
    }
    if candidate.instrumenter_version != first.instrumenter_version
        || candidate.fingerprint.instrumenter != first.fingerprint.instrumenter
    {
        dimensions.push("instrumenter");
    }
    if candidate.fingerprint.source != first.fingerprint.source {
        dimensions.push("source");
    }
    if candidate.fingerprint.tests != first.fingerprint.tests {
        dimensions.push("tests");
    }
    if candidate.fingerprint.dependencies != first.fingerprint.dependencies {
        dimensions.push("dependencies");
    }
    if candidate.fingerprint.configuration != first.fingerprint.configuration {
        dimensions.push("configuration");
    }
    // With every domain alike, what is left is how each was run: the test
    // command, for the languages whose fingerprint holds it. Commands that
    // differ are what shards are.
    if dimensions.is_empty()
        && candidate.fingerprint.combined != first.fingerprint.combined
        && candidate.fingerprint.execution == first.fingerprint.execution
    {
        dimensions.push("integrity fingerprint");
    }
    dimensions
}

/// The files a run was recorded with, from its archive.
fn recorded_files(
    archive: &[EvidenceArchiveEntry],
) -> Option<(&[u8], crate::source_manifest::InputManifest)> {
    let entry = archive
        .iter()
        .find(|entry| entry.path == crate::source_capture::ARCHIVE_PATH)?;
    Some((
        entry.contents.as_slice(),
        serde_json::from_slice(&entry.contents).ok()?,
    ))
}

/// Runs merged although their tests or configuration differ: the latest,
/// whose record of the files and integrity the merge takes, and what differed
/// that does not reach the others.
struct Grown {
    latest: usize,
    files: Option<Vec<u8>>,
    across: Vec<String>,
    /// Of each run, the tests whose record a changed support file made out
    /// of date and that another of the runs measured again: their old
    /// records are left out.
    superseded: BTreeMap<usize, BTreeSet<String>>,
}

/// Which of the runs the others can be added to, where their tests or test
/// configuration differ: the newest, when nothing that changed between each
/// of the others and it reaches what that run's tests did. That is a suite
/// that has gained tests since, with helpers they need and settings of
/// theirs, and what an earlier run recorded of its tests is still what they
/// do. Each file that differs is judged by `run_currency::judge`.
fn judged_against_latest(
    root: &Path,
    inputs: &[&StoredRun],
    archives: &[Vec<EvidenceArchiveEntry>],
    known: &[StoredRun],
    allow_changed: bool,
) -> Result<Option<Grown>, String> {
    let first = inputs[0];
    let fingerprint = |input: &StoredRun| input.metadata.integrity.fingerprint.clone();
    // Files no fingerprint covers -- a mock's data, read by a server the
    // tests reached -- are in each run's record of its files.
    let recorded =
        |index: usize| recorded_files(&archives[index]).map(|(_, manifest)| manifest.files);
    let differ = inputs.iter().any(|input| {
        fingerprint(input).tests != fingerprint(first).tests
            || fingerprint(input).configuration != fingerprint(first).configuration
            || fingerprint(input).source != fingerprint(first).source
    }) || (1..inputs.len()).any(|index| recorded(index) != recorded(0));
    if !differ {
        return Ok(None);
    }
    let latest = (0..inputs.len())
        .max_by(|left, right| {
            inputs[*left]
                .metadata
                .started_at
                .cmp(&inputs[*right].metadata.started_at)
        })
        .expect("at least two runs");
    let files = archives
        .iter()
        .map(|archive| recorded_files(archive))
        .collect::<Vec<_>>();
    // Without a record of the files, the domains are all there is to say.
    let unrecorded = |run: &StoredRun| {
        let other = inputs
            .iter()
            .find(|other| other.id != run.id)
            .expect("at least two runs");
        format!(
            "Cannot merge incompatible run {}: differing domains: {}",
            run.id,
            incompatible_dimensions(other, run).join(", ")
        )
    };
    let Some((_, into)) = &files[latest] else {
        return Err(unrecorded(inputs[latest]));
    };
    // A file only an earlier run recorded -- a mock no test of the latest
    // reached -- is not gone: it is judged by what the checkout holds now,
    // and the merge's record keeps it.
    let mut into = into.clone();
    for (_, recorded) in files.iter().flatten() {
        for file in recorded.files.keys() {
            if into.files.contains_key(file) {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(root.join(file)) {
                into.files.insert(
                    file.clone(),
                    crate::source_manifest::FileFingerprint::of(&text),
                );
            }
        }
    }
    let into = &into;
    // Every file known to hold tests, from every run there is: a file of some
    // other test changing takes nothing from this run.
    let tests = known
        .iter()
        .filter_map(crate::source_manifest::test_files)
        .flatten()
        .collect::<BTreeSet<_>>();
    let checkout = crate::run_currency::Checkout::new(root, &tests);
    let merged = |index: usize| inputs[index].metadata.merged == Some(true);
    let reference = crate::run_currency::Ran::from_entries(&archives[latest], merged(latest));
    // A file's text, where the checkout holds what the latest run recorded.
    let text = |file: &str| {
        let text = std::fs::read_to_string(root.join(file)).ok()?;
        (crate::source_manifest::FileFingerprint::of(&text).sha256 == into.files.get(file)?.sha256)
            .then_some(text)
    };
    let mut across = Vec::new();
    let mut superseded = BTreeMap::new();
    for (index, input) in inputs.iter().enumerate() {
        if index == latest {
            continue;
        }
        let Some((_, recorded)) = &files[index] else {
            return Err(unrecorded(input));
        };
        let judgment = crate::run_currency::judge(
            &checkout,
            &input.metadata.command.join(" "),
            &recorded.files,
            &into.files,
            &text,
            crate::source_manifest::test_files(input).as_ref(),
            &crate::run_currency::Ran::from_entries(&archives[index], merged(index)),
            reference.playwright.as_ref(),
            allow_changed,
        );
        // Tests a changed support file reached: kept out of this run's part
        // when another run measured them again, else the run cannot be
        // merged until they are.
        if !judgment.reached.is_empty() {
            let reached = judgment
                .reached
                .values()
                .flatten()
                .cloned()
                .collect::<BTreeSet<_>>();
            let own = measured_tests(&archives[index]);
            let again = archives
                .iter()
                .enumerate()
                .filter(|(other, _)| *other != index)
                .flat_map(|(_, archive)| measured_tests(archive).into_keys())
                .collect::<BTreeSet<_>>();
            let files = judgment.reached.keys().cloned().collect::<Vec<_>>();
            let missing = reached
                .iter()
                .filter(|test| !again.contains(*test))
                .map(|test| own.get(test).cloned().unwrap_or_else(|| test.clone()))
                .collect::<Vec<_>>();
            if !missing.is_empty() {
                return Err(format!(
                    "Cannot merge run {} with run {}: {} changed, and {} of its tests reached {} through a server that served them and were not measured again: {}. Measure them again (`supercov runs {} tests affected --names`), then merge.",
                    input.id,
                    inputs[latest].id,
                    listed(&files),
                    missing.len(),
                    if files.len() == 1 { "it" } else { "them" },
                    listed(&missing),
                    input.id,
                ));
            }
            for file in &files {
                across.push(format!(
                    "{file} (the {} test(s) of `{}` that reached it were measured again)",
                    judgment.reached[file].len(),
                    input.metadata.command.join(" ")
                ));
            }
            superseded.insert(index, reached);
        }
        if !judgment.outdated.is_empty() {
            return Err(format!(
                "Cannot merge run {} with run {}: {}. {}",
                input.id,
                inputs[latest].id,
                listed(&judgment.outdated),
                if judgment.own_tests {
                    "Its own tests changed; measure them again."
                } else if judgment.settled {
                    "Measure it again."
                } else {
                    "Measure it again, or pass --allow-changed if the change alters nothing its tests ran."
                },
            ));
        }
        across.extend(judgment.across);
    }
    Ok(Some(Grown {
        latest,
        files: Some(serde_json::to_vec(into).map_err(|error| error.to_string())?),
        across: crate::run_currency::across(across),
        superseded,
    }))
}

/// The language a run measured, from its frontend declaration.
fn measured_language(run: &StoredRun) -> Option<String> {
    let entries = read_archive(&run.evidence_path).ok()?;
    let entry = entries.iter().find(|entry| entry.path == "frontend.json")?;
    serde_json::from_slice::<FrontendRunDeclaration>(&entry.contents)
        .ok()
        .map(|declaration| declaration.language)
}

/// Why a run of another Supercov release cannot be merged with this one's.
pub fn recorded_by_another_release(run: &StoredRun) -> String {
    match run.metadata.recorded_by.as_deref() {
        Some(version) if version != env!("CARGO_PKG_VERSION") => format!(
            "it was recorded by Supercov {version}, which instruments differently from Supercov {}",
            env!("CARGO_PKG_VERSION")
        ),
        // Runs say which release recorded them from 4.7.0 on, so one that
        // does not is older; its date is what else it can say.
        None => format!(
            "it was recorded{} by a Supercov older than 4.7.0, which instruments differently from Supercov {}",
            run.metadata
                .started_at
                .get(..10)
                .map(|day| format!(" on {day}"))
                .unwrap_or_default(),
            env!("CARGO_PKG_VERSION")
        ),
        // The same release number instrumenting differently: a build of
        // Supercov's own source between releases.
        Some(version) => format!(
            "it was recorded by another build of Supercov {version}, which instruments differently from this one"
        ),
    }
}

/// A few of what moved, and how many more there are.
pub fn listed(items: &[String]) -> String {
    match items.len() {
        0..=3 => items.join("; "),
        n => format!("{}; and {} more", items[..3].join("; "), n - 3),
    }
}

/// Merge two or more complete, compatible stored runs into one immutable run.
pub fn merge_coverage_runs(
    root: &Path,
    run_ids: &[String],
    merged_run_id: &str,
    started_at: &str,
    allow_changed: bool,
) -> Result<String, String> {
    if run_ids.len() < 2 {
        return Err("Usage: supercov merge <run-id> <run-id> [...]".into());
    }
    if run_ids.iter().collect::<BTreeSet<_>>().len() != run_ids.len() {
        return Err("Each merged run must be unique".into());
    }
    let inventory =
        discover_runs(root).map_err(|error| format!("Cannot read local coverage runs: {error}"))?;
    let inputs = run_ids
        .iter()
        .map(|id| {
            inventory
                .runs
                .iter()
                .find(|run| run.id == *id)
                .ok_or_else(|| {
                    // A run that never finished publishing is named as such;
                    // anything else is simply not here.
                    match inventory.rejected.iter().find(|run| run.entry == *id) {
                        Some(rejected) => {
                            format!("Cannot merge coverage run {id}: {}", rejected.reason)
                        }
                        None => format!("Cannot merge coverage run {id}: no such local run"),
                    }
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let first = inputs[0];
    for input in inputs.iter().skip(1) {
        let dimensions = incompatible_dimensions(first, input);
        // Another release instruments differently, and nothing else is worth
        // saying about the difference: the run has to be measured again.
        if dimensions
            .iter()
            .any(|dimension| matches!(*dimension, "instrumenter" | "coverage schema"))
        {
            // Each language's instrumenter is its own, so runs of two
            // languages always differ here. Said as another build, a
            // JavaScript run and a Python run of one project were "measured
            // by another build of Supercov" -- the same build, minutes apart.
            if let (Some(first_language), Some(language)) =
                (measured_language(first), measured_language(input))
                && first_language != language
            {
                return Err(format!(
                    "Cannot merge run {}: it measured {language}, and run {} measured {first_language}; runs of different languages are not merged",
                    input.id, first.id
                ));
            }
            let older = [first, *input]
                .into_iter()
                .find(|run| run.metadata.recorded_by.as_deref() != Some(env!("CARGO_PKG_VERSION")))
                .unwrap_or(input);
            return Err(format!(
                "Cannot merge run {}: {}. Measure it again to merge it.",
                older.id,
                recorded_by_another_release(older)
            ));
        }
        // Tests and their configuration may differ; what that takes is
        // looked at file by file.
        // So may the source, where what changed is outside what is measured.
        // A different number of source files is a different source, even
        // where the digest says otherwise.
        let mut dimensions = dimensions;
        if input.metadata.integrity.fingerprint.source_files
            != first.metadata.integrity.fingerprint.source_files
            && !dimensions.contains(&"source")
        {
            dimensions.push("source files");
        }
        if dimensions
            .iter()
            .any(|dimension| !matches!(*dimension, "tests" | "configuration" | "source"))
        {
            return Err(format!(
                "Cannot merge incompatible run {}: differing domains: {}",
                input.id,
                dimensions.join(", ")
            ));
        }
    }

    let archives = inputs
        .iter()
        .map(|input| {
            read_archive(&input.evidence_path)
                .map_err(|error| format!("Cannot merge coverage run {}: {error}", input.id))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let grown = judged_against_latest(root, &inputs, &archives, &inventory.runs, allow_changed)?;
    let manifests = archives
        .iter()
        .map(|entries| {
            entries
                .iter()
                .find(|entry| entry.path == "manifest.json")
                .map(|entry| entry.contents.as_slice())
        })
        .collect::<Vec<_>>();
    let Some(mut manifest) = manifests[0] else {
        return Err("Cannot merge runs with different coverage denominators".into());
    };
    if let Some(Grown { latest, .. }) = grown {
        // A test file added since is one more entry in the scope's list of
        // what is not source. What is measured has to be the same.
        let measured = |manifest: Option<&[u8]>| {
            let mut value = serde_json::from_slice::<Value>(manifest?).ok()?;
            value.as_object_mut()?.remove("scope");
            Some(value)
        };
        let first = measured(manifests[0]);
        if first.is_none()
            || manifests
                .iter()
                .any(|candidate| measured(*candidate) != first)
        {
            return Err("Cannot merge runs with different coverage denominators".into());
        }
        manifest = manifests[latest].expect("measured above");
    } else if manifests
        .iter()
        .any(|candidate| *candidate != Some(manifest))
    {
        return Err("Cannot merge runs with different coverage denominators".into());
    }
    let models = archives
        .iter()
        .map(|entries| {
            entries
                .iter()
                .find(|entry| entry.path == "coverage-model.json")
                .map(|entry| entry.contents.as_slice())
        })
        .collect::<Vec<_>>();
    let Some(model) = models[0] else {
        return Err("Cannot merge runs without a coverage model".into());
    };
    if models.iter().any(|candidate| *candidate != Some(model)) {
        return Err("Cannot merge runs with different coverage models".into());
    }
    let declarations = archives
        .iter()
        .map(|entries| {
            let contents = entries
                .iter()
                .find(|entry| entry.path == "frontend.json")
                .ok_or_else(|| "Cannot merge runs without a frontend declaration".to_owned())?;
            serde_json::from_slice::<FrontendRunDeclaration>(&contents.contents)
                .map_err(|error| format!("Cannot merge invalid frontend declaration: {error}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut merged_declaration = declarations[0].clone();
    let mut runners = BTreeMap::new();
    for declaration in declarations {
        if declaration.protocol_version != merged_declaration.protocol_version
            || declaration.frontend_id != merged_declaration.frontend_id
            || declaration.frontend_version != merged_declaration.frontend_version
            || declaration.language != merged_declaration.language
            || declaration.structural_source != merged_declaration.structural_source
            || declaration.structural_limitations != merged_declaration.structural_limitations
        {
            return Err("Cannot merge runs with different frontend declarations".into());
        }
        for runner in declaration.runners {
            if let Some(existing) = runners.get(&runner.runner) {
                if existing != &runner {
                    return Err(format!(
                        "Cannot merge incompatible declarations for runner {}",
                        runner.runner
                    ));
                }
            } else {
                runners.insert(runner.runner.clone(), runner);
            }
        }
    }
    merged_declaration.runners = runners.into_values().collect();

    let mut lock = ProjectLock::acquire(root, merged_run_id, started_at)
        .map_err(|error| format!("Cannot lock coverage store for merge: {error}"))?;
    let work = root.join(".supercov/work").join(merged_run_id);
    let result = (|| {
        let mut entries = vec![
            EvidenceArchiveEntry {
                path: "coverage-model.json".into(),
                contents: model.to_vec(),
            },
            EvidenceArchiveEntry {
                path: "frontend.json".into(),
                contents: serde_json::to_vec(&merged_declaration)
                    .map_err(|error| format!("Could not encode merged frontend: {error}"))?,
            },
            EvidenceArchiveEntry {
                path: "manifest.json".into(),
                contents: manifest.to_vec(),
            },
        ];
        for (shard, archive) in archives.iter().enumerate() {
            for entry in archive.iter().filter(|entry| {
                !matches!(
                    entry.path.as_str(),
                    "coverage-model.json" | "frontend.json" | "manifest.json"
                )
            }) {
                // One record of the files for the whole merge: the latest
                // run's, which holds every file the others were recorded with.
                let superseded = grown
                    .as_ref()
                    .and_then(|grown| grown.superseded.get(&shard));
                let contents = match grown.as_ref().and_then(|grown| grown.files.as_ref()) {
                    Some(files) if entry.path == crate::source_capture::ARCHIVE_PATH => {
                        files.clone()
                    }
                    _ => match rewritten_contents(&entry.contents, merged_run_id, superseded)? {
                        Some(contents) => contents,
                        // Every record of the entry was of a test measured
                        // again elsewhere.
                        None => continue,
                    },
                };
                entries.push(EvidenceArchiveEntry {
                    path: merged_path(&entry.path, shard),
                    contents,
                });
            }
        }
        let archive_path = work.join("evidence.raw.gz");
        let raw = write_archive(entries, &archive_path)
            .map_err(|error| format!("Could not write merged evidence: {error}"))?;
        let metadata = RunMetadata {
            id: merged_run_id.into(),
            started_at: started_at.into(),
            duration_ms: 0.0,
            command: ["supercov", "merge"]
                .into_iter()
                .chain(allow_changed.then_some("--allow-changed"))
                .map(str::to_owned)
                .chain(run_ids.iter().cloned())
                .collect(),
            test_exit_code: Some(
                if inputs
                    .iter()
                    .all(|input| input.metadata.test_exit_code == Some(0))
                {
                    0
                } else {
                    1
                },
            ),
            // Of the latest run where the tests grew: it is the one the
            // checkout is compared with.
            integrity: inputs[grown.as_ref().map_or(0, |grown| grown.latest)]
                .metadata
                .integrity
                .clone(),
            raw_evidence: RawEvidenceMetadata {
                schema_version: raw.schema_version,
                format: raw.format.into(),
                file: raw.file.into(),
                files: raw.files,
                uncompressed_bytes: raw.uncompressed_bytes,
                compressed_bytes: raw.compressed_bytes,
            },
            isolated_build: None,
            instrumented_build_cache: None,
            timings: None,
            merged: Some(true),
            parents: Some(run_ids.to_vec()),
            source_roots: None,
            copy: None,
            recorded_by: None,
            companions: None,
            merged_across: grown
                .as_ref()
                .map(|grown| grown.across.clone())
                .filter(|across| !across.is_empty()),
        };
        publish_run(root, &metadata, &archive_path)
            .map_err(|error| format!("Could not publish merged run: {error}"))?;
        finalize_published_run(root, merged_run_id)
            .map_err(|error| format!("Could not finalize merged run: {error}"))?;
        Ok(merged_run_id.to_owned())
    })();
    let _ = remove_stored_tree_deferred(root, &work);
    let release = lock
        .release()
        .map_err(|error| format!("Could not release merge lock: {error}"));
    match (result, release) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(id), Ok(())) => Ok(id),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        fs,
        sync::{
            Arc, Barrier,
            atomic::{AtomicU64, Ordering},
        },
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;
    use crate::{evidence_archive::read_archive, run_store::create_analyzable_test_run};

    /// The name alone, so the uniqueness this depends on can be tested without
    /// creating a directory for every sample.
    fn temporary_name() -> String {
        // The clock below ticks once per microsecond, so the pid and the nonce
        // do not distinguish two tests that start together -- and the tests in
        // this module rewrite run-b's metadata and delete the root when they
        // finish, which the other test then reads as an incomplete run. The
        // counter is what makes each name its own.
        static UNIQUE: AtomicU64 = AtomicU64::new(0);
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!(
            "supercov-run-merge-{}-{nonce}-{}",
            std::process::id(),
            UNIQUE.fetch_add(1, Ordering::Relaxed)
        )
    }

    fn temporary() -> crate::test_directory::TestDirectory {
        let root = std::env::temp_dir().join(temporary_name());
        // `create_dir`, not `create_dir_all`: if a root is ever handed out
        // twice, the second test must fail here saying so, rather than share a
        // directory and fail somewhere else for a reason that reads as a bug in
        // the code under test.
        fs::create_dir(&root).unwrap();
        crate::test_directory::TestDirectory::adopt(root)
    }

    #[test]
    fn temporary_roots_stay_distinct_when_tests_start_together() {
        // What the harness does to adjacent tests: release several at once and
        // require that no two of them are handed the same directory.
        const THREADS: usize = 8;
        const EACH: usize = 500;
        let barrier = Arc::new(Barrier::new(THREADS));
        let names: Vec<String> = (0..THREADS)
            .map(|_| {
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    (0..EACH).map(|_| temporary_name()).collect::<Vec<_>>()
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .flat_map(|handle| handle.join().unwrap())
            .collect();
        let distinct = names.iter().collect::<BTreeSet<_>>();
        assert_eq!(
            distinct.len(),
            names.len(),
            "two tests would have shared a temporary root"
        );
    }

    #[test]
    fn merges_compatible_runs_atomically_and_rejects_duplicates() {
        let root = temporary();
        create_analyzable_test_run(&root, "run-a");
        create_analyzable_test_run(&root, "run-b");
        let id = merge_coverage_runs(
            &root,
            &["run-a".into(), "run-b".into()],
            "merged",
            "2026-01-01T00:00:00.000Z",
            false,
        )
        .unwrap();
        assert_eq!(id, "merged");
        let metadata: RunMetadata =
            serde_json::from_slice(&fs::read(root.join(".supercov/runs/merged/run.json")).unwrap())
                .unwrap();
        assert_eq!(metadata.merged, Some(true));
        assert_eq!(metadata.parents, Some(vec!["run-a".into(), "run-b".into()]));
        let archive = read_archive(&root.join(".supercov/runs/merged/evidence.raw.gz")).unwrap();
        assert!(archive.iter().any(|entry| entry.path == "manifest.json"));
        assert!(
            archive
                .iter()
                .any(|entry| entry.path.starts_with("shards/0/"))
        );
        assert!(
            archive
                .iter()
                .any(|entry| entry.path.starts_with("shards/1/"))
        );
        assert!(matches!(
            merge_coverage_runs(
                &root,
                &["run-a".into(), "run-a".into()],
                "invalid",
                "2026-01-01T00:00:00.000Z",
                false,
            ),
            Err(error) if error == "Each merged run must be unique"
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn names_every_incompatible_integrity_dimension() {
        let root = temporary();
        create_analyzable_test_run(&root, "run-a");
        create_analyzable_test_run(&root, "run-b");
        let metadata_path = root.join(".supercov/runs/run-b/run.json");
        let mut metadata: RunMetadata =
            serde_json::from_slice(&fs::read(&metadata_path).unwrap()).unwrap();
        metadata.integrity.fingerprint.source = "1".repeat(64);
        metadata.integrity.fingerprint.tests = "2".repeat(64);
        metadata.integrity.fingerprint.configuration = "3".repeat(64);
        metadata.integrity.fingerprint.combined = "4".repeat(64);
        fs::write(&metadata_path, serde_json::to_vec(&metadata).unwrap()).unwrap();

        let error = merge_coverage_runs(
            &root,
            &["run-a".into(), "run-b".into()],
            "merged",
            "2026-01-01T00:00:00.000Z",
            false,
        )
        .unwrap_err();
        assert_eq!(
            error,
            "Cannot merge incompatible run run-b: differing domains: source, tests, configuration"
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// Rewrites one entry of a stored run's evidence (None removes it) and
    /// keeps its metadata in step with the archive.
    fn rewrite_entry(root: &Path, id: &str, path: &str, contents: Option<Vec<u8>>) {
        let directory = root.join(".supercov/runs").join(id);
        let mut entries = read_archive(&directory.join("evidence.raw.gz")).unwrap();
        entries.retain(|entry| entry.path != path);
        if let Some(contents) = contents {
            entries.push(crate::evidence_archive::EvidenceArchiveEntry {
                path: path.into(),
                contents,
            });
        }
        let archive =
            crate::evidence_archive::write_archive(entries, &directory.join("evidence.raw.gz"))
                .unwrap();
        edit_metadata(root, id, |metadata| {
            metadata.raw_evidence.files = archive.files;
            metadata.raw_evidence.uncompressed_bytes = archive.uncompressed_bytes;
            metadata.raw_evidence.compressed_bytes = archive.compressed_bytes;
        });
    }

    fn edit_metadata(root: &Path, id: &str, edit: impl FnOnce(&mut RunMetadata)) {
        let path = root.join(".supercov/runs").join(id).join("run.json");
        let mut metadata: RunMetadata = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        edit(&mut metadata);
        fs::write(&path, serde_json::to_vec(&metadata).unwrap()).unwrap();
    }

    fn declaration_with(edit: impl FnOnce(&mut Value)) -> Option<Vec<u8>> {
        let root = temporary();
        create_analyzable_test_run(&root, "source");
        let entries = read_archive(&root.join(".supercov/runs/source/evidence.raw.gz")).unwrap();
        let mut value: Value = serde_json::from_slice(
            &entries
                .iter()
                .find(|entry| entry.path == "frontend.json")
                .unwrap()
                .contents,
        )
        .unwrap();
        edit(&mut value);
        Some(serde_json::to_vec(&value).unwrap())
    }

    /// Every way two runs can fail to merge is said by name, and a second
    /// runner, unlike a changed one, is simply added.
    #[test]
    fn each_refusal_to_merge_says_why() {
        type Setup = Box<dyn Fn(&Path)>;
        let version = env!("CARGO_PKG_VERSION");
        let cases: Vec<(&str, Setup, String)> = vec![
            (
                "another instrumenter, release unknown",
                Box::new(|root| {
                    edit_metadata(root, "run-b", |m| m.integrity.instrumenter_version = "x".into())
                }),
                format!(
                    "Cannot merge run run-a: it was recorded by a Supercov older than 4.7.0, which instruments differently from Supercov {version}. Measure it again to merge it."
                ),
            ),
            (
                "another build of this release",
                Box::new(move |root| {
                    for id in ["run-a", "run-b"] {
                        edit_metadata(root, id, |m| m.recorded_by = Some(version.into()));
                    }
                    edit_metadata(root, "run-b", |m| {
                        m.integrity.fingerprint.instrumenter = "9".repeat(64)
                    });
                }),
                format!(
                    "Cannot merge run run-b: it was recorded by another build of Supercov {version}, which instruments differently from this one. Measure it again to merge it."
                ),
            ),
            (
                "another release",
                Box::new(|root| {
                    edit_metadata(root, "run-b", |m| {
                        m.recorded_by = Some("0.0.1".into());
                        m.integrity.fingerprint.instrumenter = "9".repeat(64);
                    })
                }),
                "Cannot merge run run-a: it was recorded by a Supercov older than 4.7.0".into(),
            ),
            (
                "another language",
                Box::new(|root| {
                    edit_metadata(root, "run-b", |m| m.integrity.instrumenter_version = "x".into());
                    rewrite_entry(
                        root,
                        "run-b",
                        "frontend.json",
                        declaration_with(|v| v["language"] = "python".into()),
                    );
                }),
                "Cannot merge run run-b: it measured python, and run run-a measured javascript; runs of different languages are not merged".into(),
            ),
            (
                "dependencies",
                Box::new(|root| {
                    edit_metadata(root, "run-b", |m| {
                        m.integrity.fingerprint.dependencies = "9".repeat(64)
                    })
                }),
                "Cannot merge incompatible run run-b: differing domains: dependencies".into(),
            ),
            (
                "source file count",
                Box::new(|root| {
                    edit_metadata(root, "run-b", |m| m.integrity.fingerprint.source_files = 2)
                }),
                "Cannot merge incompatible run run-b: differing domains: source files".into(),
            ),
            (
                "denominator",
                Box::new(|root| {
                    rewrite_entry(root, "run-b", "manifest.json", Some(b"{}".to_vec()))
                }),
                "Cannot merge runs with different coverage denominators".into(),
            ),
            (
                "model",
                Box::new(|root| {
                    rewrite_entry(root, "run-b", "coverage-model.json", Some(b"{}".to_vec()))
                }),
                "Cannot merge runs with different coverage models".into(),
            ),
            (
                "invalid declaration",
                Box::new(|root| {
                    rewrite_entry(root, "run-b", "frontend.json", Some(b"[]".to_vec()))
                }),
                "Cannot merge invalid frontend declaration".into(),
            ),
            (
                "protocol version",
                Box::new(|root| {
                    rewrite_entry(
                        root,
                        "run-b",
                        "frontend.json",
                        declaration_with(|v| v["protocolVersion"] = 3.into()),
                    )
                }),
                "Cannot merge runs with different frontend declarations".into(),
            ),
            (
                "frontend id",
                Box::new(|root| {
                    rewrite_entry(
                        root,
                        "run-b",
                        "frontend.json",
                        declaration_with(|v| v["frontendId"] = "other".into()),
                    )
                }),
                "Cannot merge runs with different frontend declarations".into(),
            ),
            (
                "declared language",
                Box::new(|root| {
                    rewrite_entry(
                        root,
                        "run-b",
                        "frontend.json",
                        declaration_with(|v| v["language"] = "typescript".into()),
                    )
                }),
                "Cannot merge runs with different frontend declarations".into(),
            ),
            (
                "structural source",
                Box::new(|root| {
                    rewrite_entry(
                        root,
                        "run-b",
                        "frontend.json",
                        declaration_with(|v| v["structuralSource"] = "mixed".into()),
                    )
                }),
                "Cannot merge runs with different frontend declarations".into(),
            ),
            (
                "structural limitations",
                Box::new(|root| {
                    rewrite_entry(
                        root,
                        "run-b",
                        "frontend.json",
                        declaration_with(|v| v["structuralLimitations"] = serde_json::json!(["x"])),
                    )
                }),
                "Cannot merge runs with different frontend declarations".into(),
            ),
            (
                "coverage schema",
                Box::new(|root| edit_metadata(root, "run-b", |m| m.integrity.schema_version = 3)),
                "Cannot merge run run-a: it was recorded by a Supercov older than 4.7.0".into(),
            ),
            (
                "frontend version",
                Box::new(|root| {
                    rewrite_entry(
                        root,
                        "run-b",
                        "frontend.json",
                        declaration_with(|v| v["frontendVersion"] = "fixture-v2".into()),
                    )
                }),
                "Cannot merge runs with different frontend declarations".into(),
            ),
            (
                "runner",
                Box::new(|root| {
                    rewrite_entry(
                        root,
                        "run-b",
                        "frontend.json",
                        declaration_with(|v| {
                            v["runners"][0]["executionModel"] = "process-per-test".into()
                        }),
                    )
                }),
                "Cannot merge incompatible declarations for runner node:test".into(),
            ),
        ];
        for (label, setup, expected) in cases {
            let root = temporary();
            create_analyzable_test_run(&root, "run-a");
            create_analyzable_test_run(&root, "run-b");
            setup(&root);
            let error = merge_coverage_runs(
                &root,
                &["run-a".into(), "run-b".into()],
                "merged",
                "2026-01-01T00:00:00.000Z",
                false,
            )
            .unwrap_err();
            assert!(error.starts_with(&expected), "{label}: {error}");
        }

        let root = temporary();
        create_analyzable_test_run(&root, "run-a");
        let merge = |ids: &[&str]| {
            merge_coverage_runs(
                &root,
                &ids.iter().map(|id| (*id).to_owned()).collect::<Vec<_>>(),
                "merged",
                "2026-01-01T00:00:00.000Z",
                false,
            )
        };
        assert_eq!(
            merge(&["run-a"]).unwrap_err(),
            "Usage: supercov merge <run-id> <run-id> [...]"
        );
        assert_eq!(
            merge(&["run-a", "absent"]).unwrap_err(),
            "Cannot merge coverage run absent: no such local run"
        );
        create_analyzable_test_run(&root, "run-b");
        rewrite_entry(
            &root,
            "run-b",
            "frontend.json",
            declaration_with(|v| {
                let mut second = v["runners"][0].clone();
                second["runner"] = "vitest".into();
                v["runners"].as_array_mut().unwrap().push(second);
            }),
        );
        // One run whose tests failed makes the merged run's tests fail.
        edit_metadata(&root, "run-b", |m| m.test_exit_code = Some(1));
        merge(&["run-a", "run-b"]).unwrap();
        let merged: RunMetadata =
            serde_json::from_slice(&fs::read(root.join(".supercov/runs/merged/run.json")).unwrap())
                .unwrap();
        assert_eq!(merged.test_exit_code, Some(1));
        let entries = read_archive(&root.join(".supercov/runs/merged/evidence.raw.gz")).unwrap();
        let declaration: Value = serde_json::from_slice(
            &entries
                .iter()
                .find(|entry| entry.path == "frontend.json")
                .unwrap()
                .contents,
        )
        .unwrap();
        let runners = declaration["runners"]
            .as_array()
            .unwrap()
            .iter()
            .map(|runner| runner["runner"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(runners, ["node:test", "vitest"]);
    }

    #[test]
    fn rewrites_nested_scope_run_ids_and_rejects_non_json_records() {
        let rewritten = rewritten_contents(
            b"{\"scope\":{\"runId\":\"old\",\"testId\":\"test\"},\"nested\":[{\"scope\":{\"runId\":\"old\"}}]}\n",
            "merged",
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            std::str::from_utf8(&rewritten).unwrap(),
            "{\"scope\":{\"runId\":\"merged\",\"testId\":\"test\"},\"nested\":[{\"scope\":{\"runId\":\"merged\"}}]}\n"
        );
        assert!(rewritten_contents(b"plain\n", "merged", None).is_err());
    }

    #[test]
    fn a_test_measured_again_elsewhere_leaves_its_records_out() {
        let superseded = BTreeSet::from(["paid".to_owned()]);
        let records = b"{\"testId\":\"paid\",\"test\":\"pays\"}\n{\"testId\":\"mail\",\"test\":\"mails\"}\n{\"scope\":{\"testId\":\"paid\"},\"type\":\"hit\"}\n";
        let kept = rewritten_contents(records, "merged", Some(&superseded))
            .unwrap()
            .unwrap();
        assert_eq!(
            std::str::from_utf8(&kept).unwrap(),
            "{\"testId\":\"mail\",\"test\":\"mails\"}\n"
        );
        // An entry of the superseded test alone is left out whole.
        let only = b"{\"testId\":\"paid\",\"test\":\"pays\"}\n";
        assert_eq!(
            rewritten_contents(only, "merged", Some(&superseded)).unwrap(),
            None
        );
    }

    /// A merged entry keeps its records under the merged run's id, drops
    /// those of tests measured again elsewhere, and is gone when nothing is
    /// left of it; anything but JSON lines is refused.
    #[test]
    fn merged_records_take_the_new_run_and_drop_superseded_tests() {
        let superseded = ["t2".to_owned()].into_iter().collect::<BTreeSet<_>>();
        let rewrite = |text: &str, superseded: Option<&BTreeSet<String>>| {
            rewritten_contents(text.as_bytes(), "merged", superseded)
                .map(|kept| kept.map(|bytes| String::from_utf8(bytes).unwrap()))
        };
        assert_eq!(
            rewrite(
                "{\"testId\":\"t1\",\"scope\":{\"runId\":\"a\"}}\n{\"testId\":\"t2\"}\n{\"scope\":{\"testId\":\"t2\"}}\n{\"scope\":\"text\"}\n",
                Some(&superseded)
            ),
            Ok(Some(
                "{\"testId\":\"t1\",\"scope\":{\"runId\":\"merged\"}}\n{\"scope\":\"text\"}\n"
                    .to_owned()
            ))
        );
        // Every record superseded: nothing of the entry is left.
        assert_eq!(
            rewrite("{\"testId\":\"t2\"}\n", Some(&superseded)),
            Ok(None)
        );
        // An entry with no records stays, empty.
        assert_eq!(rewrite("", None), Ok(Some(String::new())));
        // No newline at the end: none added.
        assert_eq!(
            rewrite("[{\"scope\":{}}]", None),
            Ok(Some("[{\"scope\":{\"runId\":\"merged\"}}]".to_owned()))
        );
        assert!(
            rewrite("not json\n", None)
                .unwrap_err()
                .contains("malformed")
        );
        assert!(
            rewritten_contents(&[0xff, 0xfe], "merged", None)
                .unwrap_err()
                .contains("non-UTF-8")
        );
        let entry = |contents: &[u8]| EvidenceArchiveEntry {
            path: "e.jsonl".into(),
            contents: contents.to_vec(),
        };
        let tests = measured_tests(&[
            entry(b"{\"testId\":\"t1\",\"test\":\"adds\"}\n{\"testId\":\"t3\"}\nnot \"testId\" json\nplain\n"),
            entry(&[0xff, 0xfe]),
        ]);
        assert_eq!(
            tests,
            [("t1".to_owned(), "adds".to_owned())].into_iter().collect()
        );
        let items = |n: usize| (1..=n).map(|i| format!("i{i}")).collect::<Vec<_>>();
        assert_eq!(listed(&items(0)), "");
        assert_eq!(listed(&items(3)), "i1; i2; i3");
        assert_eq!(listed(&items(5)), "i1; i2; i3; and 2 more");
    }
}
